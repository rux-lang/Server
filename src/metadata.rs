use std::fmt::Write as _;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rux_application::{
    DependencyRecord, PackageKind, PackageMetadata, PackageMetadataErrorKind, PackageSummaryRecord,
    PackageVersionMetadataRecord,
};
use serde::Serialize;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::ToSchema;

use crate::contract::{DataEnvelope, Problem, ProblemResponse, ValidationError};
use crate::paths::{canonical_download_path, canonical_package_path, canonical_version_path};

#[derive(Clone)]
pub(crate) struct MetadataState {
    metadata: Arc<dyn PackageMetadata>,
}

pub fn router(metadata: Arc<dyn PackageMetadata>) -> Router {
    Router::new()
        .route("/v1/packages/{namespace}/{package}", get(package_summary))
        .route(
            "/v1/packages/{namespace}/{package}/{version}",
            get(package_version),
        )
        .with_state(MetadataState { metadata })
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PackageSummaryDocument {
    namespace: String,
    package: String,
    #[schema(format = "date-time")]
    created_at: String,
    canonical_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PackageVersionDocument {
    namespace: String,
    package: String,
    version: String,
    manifest_schema_version: u16,
    min_rux: String,
    package_type: PackageTypeDocument,
    description: Option<String>,
    authors: Vec<String>,
    keywords: Vec<String>,
    repository_url: Option<String>,
    homepage_url: Option<String>,
    dependencies: Vec<PackageDependencyDocument>,
    #[schema(value_type = Object)]
    normalized_manifest: Value,
    readme_file: Option<TextFileDocument>,
    license: Option<String>,
    license_file: Option<TextFileDocument>,
    checksum: ChecksumDocument,
    artifact_size: u64,
    artifact_file_count: u32,
    artifact_expanded_bytes: u64,
    source_file_count: u32,
    source_line_count: u64,
    #[schema(format = "date-time")]
    published_at: String,
    yanked: bool,
    package_url: String,
    canonical_url: String,
    download_url: String,
}

#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(rename_all = "snake_case")]
pub(crate) enum PackageTypeDocument {
    Program,
    Library,
    Source,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PackageDependencyDocument {
    alias: String,
    target_namespace: String,
    target_package: String,
    version_range: String,
}

/// An archive entry served alongside the release, as stored.
///
/// One shape for every `*File` manifest field: the path the manifest named and
/// the text that was published at it.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TextFileDocument {
    path: String,
    source: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ChecksumDocument {
    algorithm: &'static str,
    digest: String,
}

#[utoipa::path(
    get,
    path = "/packages/{namespace}/{package}",
    params(
        ("namespace" = String, Path, description = "Registry namespace identity"),
        ("package" = String, Path, description = "Package identity")
    ),
    responses(
        (status = 200, description = "Version-independent package summary", body = DataEnvelope<PackageSummaryDocument>),
        (status = 404, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn package_summary(
    State(state): State<MetadataState>,
    Path((namespace, package)): Path<(String, String)>,
) -> Response {
    match state.metadata.package(&namespace, &package).await {
        Ok(summary) => Json(DataEnvelope::new(summary_document(&summary))).into_response(),
        Err(error) => metadata_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/packages/{namespace}/{package}/{version}",
    params(
        ("namespace" = String, Path, description = "Registry namespace identity"),
        ("package" = String, Path, description = "Package identity"),
        ("version" = String, Path, description = "Exact strict Semantic Version")
    ),
    responses(
        (status = 200, description = "Exact package version metadata", body = DataEnvelope<PackageVersionDocument>),
        (status = 404, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn package_version(
    State(state): State<MetadataState>,
    Path((namespace, package, version)): Path<(String, String, String)>,
) -> Response {
    match state.metadata.version(&namespace, &package, &version).await {
        Ok(version) => Json(DataEnvelope::new(version_document(&version))).into_response(),
        Err(error) => metadata_problem(error.kind()).into_response(),
    }
}

fn summary_document(summary: &PackageSummaryRecord) -> PackageSummaryDocument {
    PackageSummaryDocument {
        namespace: summary.namespace.as_str().to_owned(),
        package: summary.package.as_str().to_owned(),
        created_at: timestamp(summary.created_at),
        canonical_url: canonical_package_path(
            summary.namespace.normalized(),
            summary.package.normalized(),
        ),
    }
}

fn version_document(version: &PackageVersionMetadataRecord) -> PackageVersionDocument {
    PackageVersionDocument {
        namespace: version.namespace.as_str().to_owned(),
        package: version.package.as_str().to_owned(),
        version: version.version.as_str().to_owned(),
        manifest_schema_version: version.manifest_schema_version,
        min_rux: version.min_rux.as_str().to_owned(),
        package_type: package_type_document(version.package_type),
        description: version.description.clone(),
        authors: version.authors.clone(),
        keywords: version
            .keywords
            .iter()
            .map(|keyword| keyword.as_str().to_owned())
            .collect(),
        repository_url: version.repository_url.clone(),
        homepage_url: version.homepage_url.clone(),
        dependencies: version
            .dependencies
            .iter()
            .map(dependency_document)
            .collect(),
        normalized_manifest: Value::Object(version.normalized_manifest.clone()),
        readme_file: version.readme_file.as_ref().map(text_file_document),
        license: version.license_expression.clone(),
        license_file: version.license_file.as_ref().map(text_file_document),
        checksum: ChecksumDocument {
            algorithm: "sha256",
            digest: hexadecimal(version.artifact_sha256.as_bytes()),
        },
        artifact_size: version.artifact_size,
        artifact_file_count: version.artifact_file_count,
        artifact_expanded_bytes: version.artifact_expanded_bytes,
        source_file_count: version.source_file_count,
        source_line_count: version.source_line_count,
        published_at: timestamp(version.published_at),
        yanked: version.yanked,
        package_url: canonical_package_path(
            version.namespace.normalized(),
            version.package.normalized(),
        ),
        canonical_url: canonical_version_path(
            version.namespace.normalized(),
            version.package.normalized(),
            version.version.as_str(),
        ),
        download_url: canonical_download_path(
            version.namespace.normalized(),
            version.package.normalized(),
            version.version.as_str(),
        ),
    }
}

pub(crate) const fn package_type_document(value: PackageKind) -> PackageTypeDocument {
    match value {
        PackageKind::Program => PackageTypeDocument::Program,
        PackageKind::Library => PackageTypeDocument::Library,
        PackageKind::Source => PackageTypeDocument::Source,
    }
}

fn dependency_document(dependency: &DependencyRecord) -> PackageDependencyDocument {
    PackageDependencyDocument {
        alias: dependency.alias.as_str().to_owned(),
        target_namespace: dependency.target_namespace.as_str().to_owned(),
        target_package: dependency.target_package.as_str().to_owned(),
        version_range: dependency.version_range.as_str().to_owned(),
    }
}

fn text_file_document((path, source): &(String, String)) -> TextFileDocument {
    TextFileDocument {
        path: path.clone(),
        source: source.clone(),
    }
}

fn hexadecimal(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value
}

pub(crate) fn timestamp(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .expect("UTC metadata timestamps should format as RFC 3339")
}

fn metadata_problem(kind: PackageMetadataErrorKind) -> Problem {
    match kind {
        PackageMetadataErrorKind::InvalidNamespace => invalid_path(
            "invalid_namespace",
            "must satisfy the registry identity-segment syntax",
            "/namespace",
        ),
        PackageMetadataErrorKind::InvalidPackage => invalid_path(
            "invalid_package",
            "must satisfy the registry identity-segment syntax",
            "/package",
        ),
        PackageMetadataErrorKind::InvalidVersion => invalid_path(
            "invalid_version",
            "must be a strict Semantic Version",
            "/version",
        ),
        PackageMetadataErrorKind::PackageNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "package_not_found",
            "The package was not found",
        ),
        PackageMetadataErrorKind::PackageVersionNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "package_version_not_found",
            "The package version was not found",
        ),
        PackageMetadataErrorKind::Unavailable => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "package_metadata_unavailable",
            "Package metadata is temporarily unavailable",
        ),
    }
}

fn invalid_path(code: &str, detail: &str, pointer: &str) -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
        "The request is invalid",
    )
    .with_detail("One or more path parameters are invalid.")
    .with_errors(vec![
        ValidationError::new(code, detail).with_pointer(pointer),
    ])
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use rux_application::{
        ArtifactSha256, PackageMetadataError, PackageSummaryRecord, PackageVersionMetadataRecord,
    };
    use rux_domain::{IdentitySegment, SemanticVersion, VersionRange};
    use serde_json::{Map, Value, json};
    use time::macros::datetime;
    use tower::ServiceExt;

    use super::*;

    struct StubMetadata {
        error: Option<PackageMetadataErrorKind>,
        with_license_file: bool,
    }

    #[async_trait]
    impl PackageMetadata for StubMetadata {
        async fn package(
            &self,
            _namespace: &str,
            _package: &str,
        ) -> Result<PackageSummaryRecord, PackageMetadataError> {
            if let Some(kind) = self.error {
                return Err(PackageMetadataError::new(kind));
            }
            Ok(summary_fixture())
        }

        async fn version(
            &self,
            _namespace: &str,
            _package: &str,
            _version: &str,
        ) -> Result<PackageVersionMetadataRecord, PackageMetadataError> {
            if let Some(kind) = self.error {
                return Err(PackageMetadataError::new(kind));
            }
            Ok(version_fixture(self.with_license_file))
        }
    }

    #[tokio::test]
    async fn package_summary_preserves_display_names_and_returns_canonical_path() {
        let response = test_router(None, false)
            .oneshot(request("/v1/packages/rux-tools/example-pkg"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!({"data": {
                "namespace": "Rux_Tools",
                "package": "Example_Pkg",
                "created_at": "2026-08-02T12:00:00Z",
                "canonical_url": "/v1/packages/rux-tools/example-pkg"
            }})
        );
    }

    #[tokio::test]
    async fn exact_version_exposes_safe_complete_metadata() {
        let response = test_router(None, false)
            .oneshot(request("/v1/packages/rux-tools/example-pkg/1.2.3+linux"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        let data = &body["data"];
        assert_eq!(data["normalized_manifest"]["manifest"]["version"], 1);
        assert_eq!(
            data["readme_file"],
            json!({"path": "README.md", "source": "# Example\n"})
        );
        assert_eq!(data["license"], "MIT");
        assert!(data["license_file"].is_null());
        assert_eq!(data["checksum"]["algorithm"], "sha256");
        assert_eq!(data["checksum"]["digest"], "04".repeat(32));
        assert_eq!(data["dependencies"][0]["alias"], "Json");
        assert_eq!(
            data["canonical_url"],
            "/v1/packages/rux-tools/example-pkg/1.2.3+linux"
        );
        assert_eq!(
            data["download_url"],
            "/v1/packages/rux-tools/example-pkg/1.2.3+linux/download"
        );
        let serialized = serde_json::to_string(&body).unwrap();
        for forbidden in [
            "storage_key",
            "published_by",
            "yanked_by",
            "package_version_id",
            "namespace_id",
        ] {
            assert!(!serialized.contains(forbidden));
        }
    }

    #[tokio::test]
    async fn expression_and_license_file_are_reported_independently() {
        let response = test_router(None, true)
            .oneshot(request("/v1/packages/rux-tools/example-pkg/1.2.3+linux"))
            .await
            .unwrap();
        let data = response_json(response).await;
        let data = &data["data"];
        assert_eq!(data["license"], "MIT");
        assert_eq!(
            data["license_file"],
            json!({"path": "LICENSE.md", "source": "License text"})
        );
    }

    #[tokio::test]
    async fn metadata_errors_use_stable_problem_contracts() {
        let cases = [
            (
                PackageMetadataErrorKind::InvalidNamespace,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                PackageMetadataErrorKind::InvalidPackage,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                PackageMetadataErrorKind::InvalidVersion,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                PackageMetadataErrorKind::PackageNotFound,
                StatusCode::NOT_FOUND,
                "package_not_found",
            ),
            (
                PackageMetadataErrorKind::PackageVersionNotFound,
                StatusCode::NOT_FOUND,
                "package_version_not_found",
            ),
            (
                PackageMetadataErrorKind::Unavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "package_metadata_unavailable",
            ),
        ];
        for (kind, status, code) in cases {
            let response = test_router(Some(kind), false)
                .oneshot(request("/v1/packages/rux/example/1.0.0"))
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(response_json(response).await["code"], code);
        }
    }

    fn test_router(error: Option<PackageMetadataErrorKind>, with_license_file: bool) -> Router {
        router(Arc::new(StubMetadata {
            error,
            with_license_file,
        }))
    }

    fn request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    async fn response_json(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    fn summary_fixture() -> PackageSummaryRecord {
        PackageSummaryRecord {
            namespace: identity("Rux_Tools"),
            package: identity("Example_Pkg"),
            created_at: datetime!(2026-08-02 12:00 UTC),
        }
    }

    fn version_fixture(with_license_file: bool) -> PackageVersionMetadataRecord {
        let mut manifest = Map::new();
        manifest.insert("manifest".into(), json!({"version": 1}));
        PackageVersionMetadataRecord {
            namespace: identity("Rux_Tools"),
            package: identity("Example_Pkg"),
            version: SemanticVersion::new("1.2.3+linux").unwrap(),
            manifest_schema_version: 1,
            min_rux: SemanticVersion::new("0.4.0").unwrap(),
            package_type: PackageKind::Source,
            description: Some("Example package".into()),
            repository_url: Some("https://example.test/repository".into()),
            homepage_url: None,
            readme_file: Some(("README.md".into(), "# Example\n".into())),
            license_expression: Some("MIT".into()),
            license_file: with_license_file.then(|| ("LICENSE.md".into(), "License text".into())),
            normalized_manifest: manifest,
            artifact_sha256: ArtifactSha256::new([4; 32]),
            artifact_size: 1024,
            artifact_file_count: 3,
            artifact_expanded_bytes: 2048,
            source_file_count: 1,
            source_line_count: 12,
            published_at: datetime!(2026-08-02 12:01 UTC),
            yanked: false,
            authors: vec!["Rux Contributors".into()],
            keywords: vec![identity("Registry")],
            dependencies: vec![DependencyRecord {
                alias: identity("Json"),
                target_namespace: identity("Rux"),
                target_package: identity("Json"),
                version_range: VersionRange::new("^1").unwrap(),
            }],
        }
    }

    fn identity(value: &str) -> IdentitySegment {
        IdentitySegment::new(value).unwrap()
    }
}
