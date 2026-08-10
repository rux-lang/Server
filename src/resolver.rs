use std::fmt::Write as _;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rux_application::{
    DependencyRecord, ResolverIndexErrorKind, ResolverIndexRecord, ResolverIndexes,
    ResolverVersionRecord,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use crate::contract::{DataEnvelope, Problem, ProblemResponse, ValidationError};

const INDEX_CACHE_CONTROL: &str = "public, no-cache";

#[derive(Clone)]
pub(crate) struct ResolverState {
    indexes: Arc<dyn ResolverIndexes>,
}

pub fn router(indexes: Arc<dyn ResolverIndexes>) -> Router {
    Router::new()
        .route("/v1/index/{namespace}/{package}", get(resolver_index))
        .with_state(ResolverState { indexes })
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ResolverIndexDocument {
    namespace: String,
    package: String,
    versions: Vec<ResolverVersionDocument>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ResolverVersionDocument {
    version: String,
    min_rux: String,
    yanked: bool,
    dependencies: Vec<ResolverDependencyDocument>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct ResolverDependencyDocument {
    alias: String,
    target_namespace: String,
    target_package: String,
    version_range: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_os: Option<Vec<String>>,
}

#[utoipa::path(
    get,
    path = "/index/{namespace}/{package}",
    params(
        ("namespace" = String, Path, description = "Registry namespace identity"),
        ("package" = String, Path, description = "Package identity"),
        ("If-None-Match" = Option<String>, Header, description = "Previously returned resolver index entity tag")
    ),
    responses(
        (status = 200, description = "Deterministic resolver index", body = DataEnvelope<ResolverIndexDocument>,
            headers(
                ("ETag" = String, description = "Strong SHA-256 validator for the exact JSON representation"),
                ("Cache-Control" = String, description = "Public cache policy requiring revalidation")
            )
        ),
        (status = 304, description = "The resolver index matches If-None-Match",
            headers(
                ("ETag" = String, description = "Current strong entity tag"),
                ("Cache-Control" = String, description = "Public cache policy requiring revalidation")
            )
        ),
        (status = 404, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn resolver_index(
    State(state): State<ResolverState>,
    Path((namespace, package)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let index = match state.indexes.get(&namespace, &package).await {
        Ok(index) => index,
        Err(error) => return resolver_problem(error.kind()).into_response(),
    };
    let document = DataEnvelope::new(index_document(&index));
    let body = serde_json::to_vec(&document).expect("resolver index DTOs always serialize");
    let etag = strong_etag(&body);

    if if_none_match(&headers, etag.to_str().expect("generated ETags are ASCII")) {
        return cached_response(StatusCode::NOT_MODIFIED, etag, Body::empty(), false);
    }
    cached_response(StatusCode::OK, etag, Body::from(body), true)
}

fn index_document(index: &ResolverIndexRecord) -> ResolverIndexDocument {
    ResolverIndexDocument {
        namespace: index.namespace.as_str().to_owned(),
        package: index.package.as_str().to_owned(),
        versions: index.versions.iter().map(version_document).collect(),
    }
}

fn version_document(version: &ResolverVersionRecord) -> ResolverVersionDocument {
    ResolverVersionDocument {
        version: version.version.as_str().to_owned(),
        min_rux: version.min_rux.as_str().to_owned(),
        yanked: version.yanked,
        dependencies: version
            .dependencies
            .iter()
            .map(dependency_document)
            .collect(),
    }
}

fn dependency_document(dependency: &DependencyRecord) -> ResolverDependencyDocument {
    ResolverDependencyDocument {
        alias: dependency.alias.as_str().to_owned(),
        target_namespace: dependency.target_namespace.as_str().to_owned(),
        target_package: dependency.target_package.as_str().to_owned(),
        version_range: dependency.version_range.as_str().to_owned(),
        target_os: (!dependency.target_os.is_empty()).then(|| {
            dependency
                .target_os
                .iter()
                .map(|target| target.as_str().to_owned())
                .collect()
        }),
    }
}

fn strong_etag(body: &[u8]) -> HeaderValue {
    let digest = Sha256::digest(body);
    let mut value = String::with_capacity(73);
    value.push_str("\"sha256-");
    for byte in digest {
        write!(&mut value, "{byte:02x}").expect("writing to a String cannot fail");
    }
    value.push('"');
    HeaderValue::from_str(&value).expect("a hexadecimal digest is a valid entity tag")
}

fn cached_response(
    status: StatusCode,
    etag: HeaderValue,
    body: Body,
    include_content_type: bool,
) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(ETAG, etag);
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(INDEX_CACHE_CONTROL));
    if include_content_type {
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }
    response
}

fn if_none_match(headers: &HeaderMap, current: &str) -> bool {
    let mut combined = Vec::new();
    let mut present = false;
    for value in headers.get_all(IF_NONE_MATCH) {
        if present {
            combined.push(b',');
        }
        combined.extend_from_slice(value.as_bytes());
        present = true;
    }
    if !present {
        return false;
    }
    let current = current
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .expect("generated ETags are quoted");
    parse_if_none_match(&combined, current.as_bytes()).unwrap_or(false)
}

fn parse_if_none_match(value: &[u8], current: &[u8]) -> Option<bool> {
    let value = trim_ows(value);
    if value == b"*" {
        return Some(true);
    }

    let mut offset = 0;
    let mut saw_tag = false;
    let mut matched = false;
    while offset < value.len() {
        while offset < value.len() && matches!(value[offset], b' ' | b'\t' | b',') {
            offset += 1;
        }
        if offset == value.len() {
            break;
        }
        if value[offset..].starts_with(b"W/") {
            offset += 2;
        }
        if value.get(offset) != Some(&b'"') {
            return None;
        }
        offset += 1;
        let start = offset;
        while let Some(byte) = value.get(offset) {
            if *byte == b'"' {
                break;
            }
            if !matches!(*byte, 0x21 | 0x23..=0x7e | 0x80..=0xff) {
                return None;
            }
            offset += 1;
        }
        if value.get(offset) != Some(&b'"') {
            return None;
        }
        matched |= &value[start..offset] == current;
        saw_tag = true;
        offset += 1;
        while offset < value.len() && matches!(value[offset], b' ' | b'\t') {
            offset += 1;
        }
        if offset < value.len() && value[offset] != b',' {
            return None;
        }
    }
    saw_tag.then_some(matched)
}

fn trim_ows(mut value: &[u8]) -> &[u8] {
    while matches!(value.first(), Some(b' ' | b'\t')) {
        value = &value[1..];
    }
    while matches!(value.last(), Some(b' ' | b'\t')) {
        value = &value[..value.len() - 1];
    }
    value
}

fn resolver_problem(kind: ResolverIndexErrorKind) -> Problem {
    match kind {
        ResolverIndexErrorKind::InvalidNamespace => invalid_identity(
            "invalid_namespace",
            "must satisfy the registry identity-segment syntax",
            "/namespace",
        ),
        ResolverIndexErrorKind::InvalidPackage => invalid_identity(
            "invalid_package",
            "must satisfy the registry identity-segment syntax",
            "/package",
        ),
        ResolverIndexErrorKind::PackageNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "package_not_found",
            "The package was not found",
        ),
        ResolverIndexErrorKind::Unavailable => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "resolver_index_unavailable",
            "The resolver index is temporarily unavailable",
        ),
    }
}

fn invalid_identity(code: &str, detail: &str, pointer: &str) -> Problem {
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
    use axum::body::to_bytes;
    use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
    use axum::http::{Request, StatusCode};
    use rux_application::{ResolverIndexError, ResolverIndexRecord, ResolverVersionRecord};
    use rux_domain::{IdentitySegment, SemanticVersion, TargetOs, VersionRange};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    #[derive(Clone)]
    struct StubIndexes {
        error: Option<ResolverIndexErrorKind>,
    }

    #[async_trait]
    impl ResolverIndexes for StubIndexes {
        async fn get(
            &self,
            _namespace: &str,
            _package: &str,
        ) -> Result<ResolverIndexRecord, ResolverIndexError> {
            if let Some(kind) = self.error {
                return Err(ResolverIndexError::new(kind));
            }
            Ok(fixture())
        }
    }

    #[tokio::test]
    async fn resolver_index_returns_deterministic_document_and_strong_etag() {
        let response = request(None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(response.headers()[CACHE_CONTROL], INDEX_CACHE_CONTROL);
        let etag = response.headers()[ETAG]
            .to_str()
            .expect("ETag should be text")
            .to_owned();
        assert!(etag.starts_with("\"sha256-"));
        assert_eq!(etag.len(), 73);

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        assert_eq!(strong_etag(&body), etag);
        let document: Value = serde_json::from_slice(&body).expect("body should be JSON");
        assert_eq!(document["data"]["namespace"], "Rux_Tools");
        assert_eq!(document["data"]["package"], "Example_Pkg");
        assert_eq!(document["data"]["versions"][0]["version"], "1.0.0");
        assert_eq!(document["data"]["versions"][0]["yanked"], true);
        assert_eq!(
            document["data"]["versions"][0]["dependencies"][0]["version_range"],
            "^1"
        );
        assert_eq!(
            document["data"]["versions"][0]["dependencies"][0]["target_os"],
            json!(["Windows", "Linux"])
        );
    }

    #[test]
    fn strong_etags_are_stable_and_change_with_the_representation() {
        let first = strong_etag(br#"{"data":{"yanked":false}}"#);
        assert_eq!(first, strong_etag(br#"{"data":{"yanked":false}}"#));
        assert_ne!(first, strong_etag(br#"{"data":{"yanked":true}}"#));
    }

    #[tokio::test]
    async fn matching_conditional_requests_return_empty_not_modified_responses() {
        let initial = request(None).await;
        let etag = initial.headers()[ETAG]
            .to_str()
            .expect("ETag should be text")
            .to_owned();
        for condition in [
            etag.clone(),
            format!("W/{etag}"),
            format!("\"stale\", {etag}"),
            "*".into(),
        ] {
            let response = request(Some(&condition)).await;
            assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "{condition}");
            assert_eq!(response.headers()[ETAG], etag);
            assert_eq!(response.headers()[CACHE_CONTROL], INDEX_CACHE_CONTROL);
            assert!(response.headers().get(CONTENT_TYPE).is_none());
            assert!(
                to_bytes(response.into_body(), usize::MAX)
                    .await
                    .expect("body should read")
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn stale_and_malformed_conditions_are_ignored() {
        for condition in ["\"stale\"", "not-an-etag", "\"unterminated"] {
            assert_eq!(request(Some(condition)).await.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn repeated_if_none_match_fields_are_combined() {
        let initial = request(None).await;
        let etag = initial.headers()[ETAG]
            .to_str()
            .expect("ETag should be text")
            .to_owned();
        let response = router(Arc::new(StubIndexes { error: None }))
            .oneshot(
                Request::builder()
                    .uri("/v1/index/rux-tools/example-pkg")
                    .header(IF_NONE_MATCH, "\"stale\"")
                    .header(IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn resolver_errors_use_stable_problem_responses() {
        for (kind, status, code) in [
            (
                ResolverIndexErrorKind::InvalidNamespace,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                ResolverIndexErrorKind::InvalidPackage,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                ResolverIndexErrorKind::PackageNotFound,
                StatusCode::NOT_FOUND,
                "package_not_found",
            ),
            (
                ResolverIndexErrorKind::Unavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "resolver_index_unavailable",
            ),
        ] {
            let response = router(Arc::new(StubIndexes { error: Some(kind) }))
                .oneshot(
                    Request::builder()
                        .uri("/v1/index/rux/example")
                        .body(Body::empty())
                        .expect("request should build"),
                )
                .await
                .expect("router should respond");
            assert_eq!(response.status(), status);
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body should read");
            let document: Value = serde_json::from_slice(&body).expect("problem should be JSON");
            assert_eq!(document["code"], code);
        }
    }

    async fn request(condition: Option<&str>) -> Response {
        let mut builder = Request::builder().uri("/v1/index/rux-tools/example-pkg");
        if let Some(condition) = condition {
            builder = builder.header(IF_NONE_MATCH, condition);
        }
        router(Arc::new(StubIndexes { error: None }))
            .oneshot(builder.body(Body::empty()).expect("request should build"))
            .await
            .expect("router should respond")
    }

    fn fixture() -> ResolverIndexRecord {
        ResolverIndexRecord {
            namespace: identity("Rux_Tools"),
            package: identity("Example_Pkg"),
            versions: vec![ResolverVersionRecord {
                version: semantic_version("1.0.0"),
                min_rux: semantic_version("0.4.0"),
                yanked: true,
                dependencies: vec![DependencyRecord {
                    alias: identity("Json"),
                    target_namespace: identity("Rux"),
                    target_package: identity("Json"),
                    version_range: VersionRange::new("^1").expect("valid range fixture"),
                    target_os: vec![TargetOs::Windows, TargetOs::Linux],
                }],
            }],
        }
    }

    fn identity(value: &str) -> IdentitySegment {
        IdentitySegment::new(value).expect("valid identity fixture")
    }

    fn semantic_version(value: &str) -> SemanticVersion {
        SemanticVersion::new(value).expect("valid version fixture")
    }
}
