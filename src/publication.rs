use std::sync::Arc;

use axum::extract::multipart::MultipartRejection;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::header::{AUTHORIZATION, LOCATION};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use rux_application::{
    ArtifactUpload, DependencyRecord, JsonObject, PackageKind, PublicationErrorKind,
    PublicationMetadata, Publications, PublishedPackageVersion,
};
use rux_manifest::{
    BuildMode, DefineValue, DependencySource, License, Manifest, ManifestKind, Optimization,
    PackageManifest, PackageType,
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::ToSchema;

use crate::contract::{DataEnvelope, Problem, ProblemResponse, ValidationError};
use crate::paths::canonical_version_path as canonical_version_resource_path;
use crate::upload::{
    InspectionError, InspectionErrorKind, UPLOAD_REQUEST_MAX_BYTES, UploadError, UploadErrorKind,
    UploadReceiver,
};

#[derive(Clone)]
pub(crate) struct PublicationState {
    publications: Arc<dyn Publications>,
    uploads: UploadReceiver,
}

pub fn router(publications: Arc<dyn Publications>, uploads: UploadReceiver) -> Router {
    Router::new()
        .route("/v1/packages", post(publish_package))
        .layer(DefaultBodyLimit::max(UPLOAD_REQUEST_MAX_BYTES))
        .with_state(PublicationState {
            publications,
            uploads,
        })
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PublishedPackageVersionDocument {
    namespace: String,
    package: String,
    version: String,
    #[schema(format = "date-time")]
    published_at: String,
}

#[utoipa::path(
    post,
    path = "/packages",
    security(("bearer_token" = [])),
    request_body(content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "Immutable package version published", body = DataEnvelope<PublishedPackageVersionDocument>),
        (status = 400, response = ProblemResponse),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 404, response = ProblemResponse),
        (status = 409, response = ProblemResponse),
        (status = 413, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn publish_package(
    State(state): State<PublicationState>,
    headers: HeaderMap,
    multipart: Result<Multipart, MultipartRejection>,
) -> Response {
    let Some(credential) = bearer_credential(&headers) else {
        return publication_problem(PublicationErrorKind::AuthenticationRequired).into_response();
    };
    let Ok(multipart) = multipart else {
        return malformed_multipart().into_response();
    };
    let received = match state.uploads.receive(multipart).await {
        Ok(received) => received,
        Err(error) => return upload_problem(&error).into_response(),
    };
    let inspected = match received.inspect().await {
        Ok(inspected) => inspected,
        Err(error) => return inspection_problem(&error).into_response(),
    };
    let Some(metadata) = publication_metadata(inspected.inspection()) else {
        return publication_unavailable().into_response();
    };
    let artifact = ArtifactUpload {
        path: inspected.package().path().to_path_buf(),
        namespace: metadata.namespace.clone(),
        package: metadata.package.clone(),
        version: metadata.version.clone(),
        sha256: inspected.package().sha256(),
        byte_size: inspected.package().byte_size(),
    };

    match state
        .publications
        .publish(&credential, metadata, artifact)
        .await
    {
        Ok(published) => published_response(&published),
        Err(error) => publication_problem(error.kind()).into_response(),
    }
}

fn bearer_credential(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|credential| {
            !credential.is_empty() && !credential.bytes().any(|byte| byte.is_ascii_whitespace())
        })
        .map(str::to_owned)
}

fn publication_metadata(
    inspection: &rux_artifact::ArtifactInspection,
) -> Option<PublicationMetadata> {
    let manifest = inspection.manifest();
    let ManifestKind::Package(package) = manifest.kind() else {
        return None;
    };
    let namespace = package.namespace()?.clone();
    let dependencies = package
        .dependencies()
        .iter()
        .map(|(alias, dependency)| {
            let DependencySource::Registry { namespace, version } = dependency.source() else {
                return None;
            };
            Some(DependencyRecord {
                alias: alias.clone(),
                target_namespace: namespace.clone(),
                target_package: dependency.package().clone(),
                version_range: version.clone(),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let (license_expression, license_file) = match package.license() {
        Some(License::Expression(expression)) => (Some(expression.clone()), None),
        Some(License::File(path)) => (
            None,
            Some((
                path.as_str().to_owned(),
                inspection.license_file()?.to_owned(),
            )),
        ),
        None => (None, None),
    };
    let readme = package.readme().map(|path| {
        (
            path.as_str().to_owned(),
            inspection
                .readme()
                .expect("artifact inspection returns referenced README text")
                .to_owned(),
        )
    });

    Some(PublicationMetadata {
        namespace,
        package: package.name().clone(),
        version: package.version().clone(),
        manifest_schema_version: manifest.header().version(),
        min_rux: manifest.header().min_rux().clone(),
        package_type: package_kind(package.package_type()),
        description: package.description().map(str::to_owned),
        repository_url: package.repository().map(|value| value.as_str().to_owned()),
        homepage_url: package.homepage().map(|value| value.as_str().to_owned()),
        readme,
        license_expression,
        license_file,
        normalized_manifest: normalized_manifest(manifest, package),
        artifact_file_count: inspection.file_count(),
        artifact_expanded_bytes: inspection.expanded_bytes(),
        source_file_count: inspection.source_file_count(),
        source_line_count: inspection.source_line_count(),
        authors: package.authors().to_vec(),
        keywords: package.keywords().to_vec(),
        dependencies,
    })
}

const fn package_kind(value: PackageType) -> PackageKind {
    match value {
        PackageType::Program => PackageKind::Program,
        PackageType::Library => PackageKind::Library,
        PackageType::Source => PackageKind::Source,
    }
}

const fn package_type_name(value: PackageType) -> &'static str {
    match value {
        PackageType::Program => "program",
        PackageType::Library => "library",
        PackageType::Source => "source",
    }
}

fn normalized_manifest(manifest: &Manifest, package: &PackageManifest) -> JsonObject {
    let mut package_json = Map::new();
    package_json.insert(
        "namespace".into(),
        json!(
            package
                .namespace()
                .expect("publication requires a namespace")
                .as_str()
        ),
    );
    package_json.insert("name".into(), json!(package.name().as_str()));
    package_json.insert("version".into(), json!(package.version().as_str()));
    package_json.insert(
        "type".into(),
        json!(package_type_name(package.package_type())),
    );
    insert_optional(&mut package_json, "description", package.description());
    if !package.authors().is_empty() {
        package_json.insert("authors".into(), json!(package.authors()));
    }
    if !package.keywords().is_empty() {
        package_json.insert(
            "keywords".into(),
            json!(
                package
                    .keywords()
                    .iter()
                    .map(|item| item.as_str().to_owned())
                    .collect::<Vec<_>>()
            ),
        );
    }
    match package.license() {
        Some(License::Expression(value)) => {
            package_json.insert("license".into(), json!(value));
        }
        Some(License::File(value)) => {
            package_json.insert("license_file".into(), json!(value.as_str()));
        }
        None => {}
    }
    insert_optional(
        &mut package_json,
        "repository",
        package.repository().map(rux_manifest::WebUrl::as_str),
    );
    insert_optional(
        &mut package_json,
        "homepage",
        package.homepage().map(rux_manifest::WebUrl::as_str),
    );
    insert_optional(
        &mut package_json,
        "readme",
        package.readme().map(rux_manifest::ManifestPath::as_str),
    );

    let dependencies = package
        .dependencies()
        .iter()
        .map(|(alias, dependency)| {
            let DependencySource::Registry { namespace, version } = dependency.source() else {
                unreachable!("publication manifests contain only registry dependencies");
            };
            (
                alias.as_str().to_owned(),
                json!({
                    "namespace": namespace.as_str(),
                    "package": dependency.package().as_str(),
                    "version": version.as_str(),
                }),
            )
        })
        .collect::<Map<_, _>>();

    let mut root = Map::new();
    root.insert(
        "manifest".into(),
        json!({
            "version": manifest.header().version(),
            "min_rux": manifest.header().min_rux().as_str(),
        }),
    );
    root.insert("package".into(), Value::Object(package_json));
    root.insert("dependencies".into(), Value::Object(dependencies));
    root.insert("build".into(), normalized_build(package));
    root
}

fn normalized_build(package: &PackageManifest) -> Value {
    let mode = |mode| {
        let effective = package.build().resolve(mode);
        json!({
            "optimization": optimization_name(effective.optimization()),
            "debug_info": effective.debug_info(),
            "debug_assertions": effective.debug_assertions(),
            "output": effective.output().as_str(),
            "defines": effective.defines().iter().map(|(name, value)| {
                (name.clone(), define_value(value))
            }).collect::<Map<_, _>>(),
        })
    };
    json!({
        "debug": mode(BuildMode::Debug),
        "release": mode(BuildMode::Release),
    })
}

const fn optimization_name(value: Optimization) -> &'static str {
    match value {
        Optimization::None => "none",
        Optimization::Size => "size",
        Optimization::Speed => "speed",
    }
}

fn define_value(value: &DefineValue) -> Value {
    match value {
        DefineValue::String(value) => json!(value),
        DefineValue::Boolean(value) => json!(value),
        DefineValue::Integer(value) => json!(value),
    }
}

fn insert_optional(map: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        map.insert(key.to_owned(), json!(value));
    }
}

fn published_response(published: &PublishedPackageVersion) -> Response {
    let location = canonical_version_path(published);
    let document = PublishedPackageVersionDocument {
        namespace: published.namespace.as_str().to_owned(),
        package: published.package.as_str().to_owned(),
        version: published.version.as_str().to_owned(),
        published_at: timestamp(published.published_at),
    };
    let mut response = (StatusCode::CREATED, Json(DataEnvelope::new(document))).into_response();
    response.headers_mut().insert(
        LOCATION,
        HeaderValue::from_str(&location).expect("canonical package paths contain valid headers"),
    );
    response
}

fn canonical_version_path(published: &PublishedPackageVersion) -> String {
    canonical_version_resource_path(
        published.namespace.normalized(),
        published.package.normalized(),
        published.version.as_str(),
    )
}

fn timestamp(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .expect("UTC publication timestamps should format as RFC 3339")
}

fn malformed_multipart() -> Problem {
    Problem::new(
        StatusCode::BAD_REQUEST,
        "invalid_multipart",
        "The multipart request is invalid",
    )
}

fn upload_problem(error: &UploadError) -> Problem {
    match error.kind() {
        UploadErrorKind::RequestTooLarge
        | UploadErrorKind::ManifestTooLarge
        | UploadErrorKind::ArtifactTooLarge => Problem::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "upload_too_large",
            "The publication upload is too large",
        ),
        UploadErrorKind::MalformedMultipart | UploadErrorKind::RequestReadFailed => {
            malformed_multipart()
        }
        UploadErrorKind::UnexpectedPart
        | UploadErrorKind::DuplicateManifest
        | UploadErrorKind::DuplicatePackage
        | UploadErrorKind::MissingManifest
        | UploadErrorKind::MissingPackage => Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_upload",
            "The publication upload is invalid",
        )
        .with_errors(vec![ValidationError::new(
            upload_error_code(error.kind()),
            "the multipart form must contain exactly one manifest and one package part",
        )]),
        UploadErrorKind::TemporaryCapacityExceeded
        | UploadErrorKind::TemporaryStorageUnavailable => publication_unavailable(),
    }
}

const fn upload_error_code(kind: UploadErrorKind) -> &'static str {
    match kind {
        UploadErrorKind::UnexpectedPart => "unexpected_part",
        UploadErrorKind::DuplicateManifest => "duplicate_manifest",
        UploadErrorKind::DuplicatePackage => "duplicate_package",
        UploadErrorKind::MissingManifest => "missing_manifest",
        UploadErrorKind::MissingPackage => "missing_package",
        _ => "invalid_upload",
    }
}

fn inspection_problem(error: &InspectionError) -> Problem {
    match error.kind() {
        InspectionErrorKind::InvalidArtifact => {
            let artifact = error
                .artifact_error()
                .expect("invalid artifact errors retain their diagnostic");
            let errors = artifact.manifest_errors().map_or_else(
                || {
                    vec![ValidationError::new(
                        artifact.code().as_str(),
                        artifact.message(),
                    )]
                },
                |manifest_errors| {
                    manifest_errors
                        .as_slice()
                        .iter()
                        .map(|error| {
                            ValidationError::new(
                                error.code().as_str(),
                                format!(
                                    "{} at line {}, column {}",
                                    error.message(),
                                    error.span().start().line(),
                                    error.span().start().column()
                                ),
                            )
                        })
                        .collect()
                },
            );
            Problem::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_artifact",
                "The package artifact is invalid",
            )
            .with_errors(errors)
        }
        InspectionErrorKind::TemporaryStorageUnavailable
        | InspectionErrorKind::InspectionUnavailable => publication_unavailable(),
    }
}

fn publication_problem(kind: PublicationErrorKind) -> Problem {
    match kind {
        PublicationErrorKind::AuthenticationRequired => Problem::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "Authentication is required",
        ),
        PublicationErrorKind::InsufficientScope => Problem::new(
            StatusCode::FORBIDDEN,
            "insufficient_scope",
            "The API token does not grant the required scope",
        ),
        PublicationErrorKind::NamespaceNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "namespace_not_found",
            "The namespace was not found",
        ),
        PublicationErrorKind::Forbidden => Problem::new(
            StatusCode::FORBIDDEN,
            "publication_forbidden",
            "The caller cannot publish to this namespace",
        ),
        PublicationErrorKind::NamespaceBlocked | PublicationErrorKind::PackageBlocked => {
            Problem::new(
                StatusCode::FORBIDDEN,
                "identity_blocked",
                "The package identity is blocked from publication",
            )
        }
        PublicationErrorKind::VersionConflict => Problem::new(
            StatusCode::CONFLICT,
            "version_conflict",
            "The package version already exists",
        ),
        PublicationErrorKind::Unavailable => publication_unavailable(),
    }
}

fn publication_unavailable() -> Problem {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "publication_unavailable",
        "Package publication is temporarily unavailable",
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};
    use std::sync::Mutex;

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use rux_application::{ArtifactSha256, PublicationError, PublicationMetadata};
    use rux_artifact::ARTIFACT_MAX_BYTES;
    use time::macros::datetime;
    use tower::ServiceExt;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::*;

    const BOUNDARY: &str = "publication-boundary";
    const MANIFEST: &str = r#"[Manifest]
Version = 1
MinRux = "0.4.0"

[Package]
Namespace = "Rux_Tools"
Name = "Example_Pkg"
Version = "1.2.3+native"
Type = "Source"
Authors = ["Rux Contributors"]
Keywords = ["Registry"]
License = "MIT"
"#;

    #[derive(Debug)]
    struct CapturedPublication {
        credential: String,
        metadata: PublicationMetadata,
        sha256: ArtifactSha256,
        byte_size: u64,
        staged_bytes: Vec<u8>,
    }

    #[derive(Default)]
    struct FakePublications {
        captured: Mutex<Option<CapturedPublication>>,
    }

    #[async_trait]
    impl Publications for FakePublications {
        async fn publish(
            &self,
            credential: &str,
            metadata: PublicationMetadata,
            artifact: ArtifactUpload,
        ) -> Result<PublishedPackageVersion, PublicationError> {
            let staged_bytes = std::fs::read(&artifact.path).expect("staged artifact should exist");
            let namespace = metadata.namespace.clone();
            let package = metadata.package.clone();
            let version = metadata.version.clone();
            *self
                .captured
                .lock()
                .expect("capture should not be poisoned") = Some(CapturedPublication {
                credential: credential.to_owned(),
                metadata,
                sha256: artifact.sha256,
                byte_size: artifact.byte_size,
                staged_bytes,
            });
            Ok(PublishedPackageVersion {
                namespace,
                package,
                version,
                published_at: datetime!(2026-08-02 12:34:56 UTC),
            })
        }
    }

    #[tokio::test]
    async fn publication_composes_inspection_workflow_and_created_response() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let uploads = UploadReceiver::new(directory.path(), ARTIFACT_MAX_BYTES)
            .expect("receiver should configure");
        let publications = Arc::new(FakePublications::default());
        let package = artifact_package(MANIFEST, &[("Src/Main.rux", b"Main() {}\n")]);
        let body = multipart_body(MANIFEST.as_bytes(), &package);

        let response = router(publications.clone(), uploads.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/packages")
                    .header(AUTHORIZATION, "Bearer rux_pat_publish_secret")
                    .header(
                        axum::http::header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={BOUNDARY}"),
                    )
                    .body(Body::from(body))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers()[LOCATION],
            "/v1/packages/rux-tools/example-pkg/1.2.3+native"
        );
        let response_body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should read");
        let document: Value = serde_json::from_slice(&response_body).expect("response is JSON");
        assert_eq!(document["data"]["namespace"], "Rux_Tools");
        assert_eq!(document["data"]["package"], "Example_Pkg");
        assert_eq!(document["data"]["version"], "1.2.3+native");
        assert_eq!(document["data"]["published_at"], "2026-08-02T12:34:56Z");

        let captured = publications
            .captured
            .lock()
            .expect("capture should not be poisoned")
            .take()
            .expect("publication should be invoked");
        assert_eq!(captured.credential, "rux_pat_publish_secret");
        assert_eq!(captured.staged_bytes, package);
        assert_eq!(captured.byte_size, package.len() as u64);
        assert_ne!(captured.sha256, ArtifactSha256::new([0; 32]));
        assert_eq!(captured.metadata.namespace.as_str(), "Rux_Tools");
        assert_eq!(
            captured.metadata.normalized_manifest["build"]["debug"]["output"],
            "Bin/Debug"
        );
        assert_eq!(uploads.temporary_bytes_in_use(), 0);
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("staging directory should read")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn publication_rejects_missing_bearer_before_workflow() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let uploads = UploadReceiver::new(directory.path(), ARTIFACT_MAX_BYTES)
            .expect("receiver should configure");
        let publications = Arc::new(FakePublications::default());

        let response = router(publications.clone(), uploads)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/packages")
                    .header(
                        axum::http::header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={BOUNDARY}"),
                    )
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(
            publications
                .captured
                .lock()
                .expect("capture should not be poisoned")
                .is_none()
        );
    }

    fn multipart_body(manifest: &[u8], package: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        for (name, bytes) in [("manifest", manifest), ("package", package)] {
            body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        body
    }

    fn artifact_package(manifest: &str, entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer
            .start_file("Rux.toml", options)
            .expect("manifest entry should start");
        writer
            .write_all(manifest.as_bytes())
            .expect("manifest entry should write");
        for (name, bytes) in entries {
            writer
                .start_file(*name, options)
                .expect("fixture entry should start");
            writer.write_all(bytes).expect("fixture entry should write");
        }
        writer
            .finish()
            .expect("fixture archive should finish")
            .into_inner()
    }
}
