use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rux_application::{
    DependentPackageRecord, Discovery, DiscoveryErrorKind, DiscoveryPageParameters,
    HighlightPackageRecord, KeywordRecord, POPULARITY_WINDOW_DAYS, PackageVersionHistoryRecord,
    SitemapEntryKind, SitemapEntryRecord,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::contract::{DataEnvelope, Problem, ProblemResponse, ValidationError};
use crate::metadata::{PackageTypeDocument, package_type_document, timestamp};
use crate::paths::{canonical_download_path, canonical_package_path, canonical_version_path};

#[derive(Clone)]
pub(crate) struct DiscoveryState {
    discovery: Arc<dyn Discovery>,
}

pub fn router(discovery: Arc<dyn Discovery>) -> Router {
    Router::new()
        .route(
            "/v1/packages/{namespace}/{package}/dependents",
            get(package_dependents),
        )
        .route(
            "/v1/packages/{namespace}/{package}/versions",
            get(package_versions),
        )
        .route("/v1/keywords", get(keywords))
        .route("/v1/highlights", get(highlights))
        .route("/v1/sitemap", get(sitemap))
        .with_state(DiscoveryState { discovery })
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub(crate) struct DiscoveryQuery {
    #[param(minimum = 1, maximum = 100, default = 20)]
    limit: Option<u16>,
    cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub(crate) struct SitemapQuery {
    #[param(minimum = 1, maximum = 1000, default = 100)]
    limit: Option<u16>,
    cursor: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyQuery {}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DiscoveryPageMeta {
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DependentRequirementDocument {
    alias: String,
    version_range: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DependentPackageDocument {
    namespace: String,
    package: String,
    version: String,
    package_type: PackageTypeDocument,
    description: Option<String>,
    #[schema(format = "date-time")]
    published_at: String,
    yanked: bool,
    requirements: Vec<DependentRequirementDocument>,
    package_url: String,
    version_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DependentsResponse {
    data: Vec<DependentPackageDocument>,
    meta: DiscoveryPageMeta,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct KeywordDocument {
    keyword: String,
    normalized_keyword: String,
    package_count: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct KeywordsResponse {
    data: Vec<KeywordDocument>,
    meta: DiscoveryPageMeta,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct VersionHistoryDocument {
    version: String,
    min_rux: String,
    package_type: PackageTypeDocument,
    #[schema(format = "date-time")]
    published_at: String,
    yanked: bool,
    canonical_url: String,
    download_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct VersionsResponse {
    data: Vec<VersionHistoryDocument>,
    meta: DiscoveryPageMeta,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct HighlightPackageDocument {
    namespace: String,
    package: String,
    version: String,
    package_type: PackageTypeDocument,
    description: Option<String>,
    #[schema(format = "date-time")]
    published_at: String,
    downloads_30d: Option<u64>,
    package_url: String,
    version_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct HighlightsDocument {
    window_days: i64,
    recent: Vec<HighlightPackageDocument>,
    popular: Vec<HighlightPackageDocument>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum SitemapEntryDocument {
    Keyword {
        keyword: String,
        normalized_keyword: String,
        #[schema(format = "date-time")]
        last_modified: String,
    },
    Namespace {
        namespace: String,
        normalized_namespace: String,
        #[schema(format = "date-time")]
        last_modified: String,
    },
    Package {
        namespace: String,
        normalized_namespace: String,
        package: String,
        normalized_package: String,
        #[schema(format = "date-time")]
        last_modified: String,
    },
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SitemapResponse {
    data: Vec<SitemapEntryDocument>,
    meta: DiscoveryPageMeta,
}

#[utoipa::path(
    get,
    path = "/packages/{namespace}/{package}/dependents",
    params(
        ("namespace" = String, Path, description = "Registry namespace identity"),
        ("package" = String, Path, description = "Package identity"),
        DiscoveryQuery
    ),
    responses(
        (status = 200, description = "Representative-version dependent packages", body = DependentsResponse),
        (status = 404, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn package_dependents(
    State(state): State<DiscoveryState>,
    Path((namespace, package)): Path<(String, String)>,
    query: Result<Query<DiscoveryQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return malformed_query_problem().into_response();
    };
    match state
        .discovery
        .dependents(&namespace, &package, parameters(query))
        .await
    {
        Ok(page) => Json(DependentsResponse {
            data: page.items.iter().map(dependent_document).collect(),
            meta: DiscoveryPageMeta {
                next_cursor: page.next_cursor,
            },
        })
        .into_response(),
        Err(error) => discovery_problem(error.kind(), false).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/keywords",
    params(DiscoveryQuery),
    responses(
        (status = 200, description = "Representative-version keyword aggregates", body = KeywordsResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn keywords(
    State(state): State<DiscoveryState>,
    query: Result<Query<DiscoveryQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return malformed_query_problem().into_response();
    };
    match state.discovery.keywords(parameters(query)).await {
        Ok(page) => Json(KeywordsResponse {
            data: page.items.iter().map(keyword_document).collect(),
            meta: DiscoveryPageMeta {
                next_cursor: page.next_cursor,
            },
        })
        .into_response(),
        Err(error) => discovery_problem(error.kind(), false).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/packages/{namespace}/{package}/versions",
    params(
        ("namespace" = String, Path, description = "Registry namespace identity"),
        ("package" = String, Path, description = "Package identity"),
        DiscoveryQuery
    ),
    responses(
        (status = 200, description = "Descending package version history", body = VersionsResponse),
        (status = 404, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn package_versions(
    State(state): State<DiscoveryState>,
    Path((namespace, package)): Path<(String, String)>,
    query: Result<Query<DiscoveryQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return malformed_query_problem().into_response();
    };
    match state
        .discovery
        .versions(&namespace, &package, parameters(query))
        .await
    {
        Ok(page) => Json(VersionsResponse {
            data: page
                .items
                .iter()
                .map(|item| version_document(item, &namespace, &package))
                .collect(),
            meta: DiscoveryPageMeta {
                next_cursor: page.next_cursor,
            },
        })
        .into_response(),
        Err(error) => discovery_problem(error.kind(), false).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/highlights",
    responses(
        (status = 200, description = "Recent and 30-day-popular package highlights", body = DataEnvelope<HighlightsDocument>),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn highlights(
    State(state): State<DiscoveryState>,
    query: Result<Query<EmptyQuery>, QueryRejection>,
) -> Response {
    if query.is_err() {
        return malformed_query_problem().into_response();
    }
    match state.discovery.highlights().await {
        Ok(highlights) => Json(DataEnvelope::new(HighlightsDocument {
            window_days: POPULARITY_WINDOW_DAYS,
            recent: highlights.recent.iter().map(highlight_document).collect(),
            popular: highlights.popular.iter().map(highlight_document).collect(),
        }))
        .into_response(),
        Err(error) => discovery_problem(error.kind(), false).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/sitemap",
    params(SitemapQuery),
    responses(
        (status = 200, description = "Structured catalog sitemap source records", body = SitemapResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn sitemap(
    State(state): State<DiscoveryState>,
    query: Result<Query<SitemapQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return malformed_query_problem().into_response();
    };
    match state
        .discovery
        .sitemap(DiscoveryPageParameters {
            limit: query.limit,
            cursor: query.cursor,
        })
        .await
    {
        Ok(page) => Json(SitemapResponse {
            data: page.items.iter().map(sitemap_document).collect(),
            meta: DiscoveryPageMeta {
                next_cursor: page.next_cursor,
            },
        })
        .into_response(),
        Err(error) => discovery_problem(error.kind(), true).into_response(),
    }
}

fn parameters(query: DiscoveryQuery) -> DiscoveryPageParameters {
    DiscoveryPageParameters {
        limit: query.limit,
        cursor: query.cursor,
    }
}

fn dependent_document(record: &DependentPackageRecord) -> DependentPackageDocument {
    DependentPackageDocument {
        namespace: record.namespace.as_str().to_owned(),
        package: record.package.as_str().to_owned(),
        version: record.version.as_str().to_owned(),
        package_type: package_type_document(record.package_type),
        description: record.description.clone(),
        published_at: timestamp(record.published_at),
        yanked: record.yanked,
        requirements: record
            .requirements
            .iter()
            .map(|requirement| DependentRequirementDocument {
                alias: requirement.alias.as_str().to_owned(),
                version_range: requirement.version_range.as_str().to_owned(),
            })
            .collect(),
        package_url: canonical_package_path(
            record.namespace.normalized(),
            record.package.normalized(),
        ),
        version_url: canonical_version_path(
            record.namespace.normalized(),
            record.package.normalized(),
            record.version.as_str(),
        ),
    }
}

fn keyword_document(record: &KeywordRecord) -> KeywordDocument {
    KeywordDocument {
        keyword: record.keyword.as_str().to_owned(),
        normalized_keyword: record.keyword.normalized().to_owned(),
        package_count: record.package_count,
    }
}

fn version_document(
    record: &PackageVersionHistoryRecord,
    namespace: &str,
    package: &str,
) -> VersionHistoryDocument {
    let namespace = namespace.to_ascii_lowercase().replace('_', "-");
    let package = package.to_ascii_lowercase().replace('_', "-");
    VersionHistoryDocument {
        version: record.version.as_str().to_owned(),
        min_rux: record.min_rux.as_str().to_owned(),
        package_type: package_type_document(record.package_type),
        published_at: timestamp(record.published_at),
        yanked: record.yanked,
        canonical_url: canonical_version_path(&namespace, &package, record.version.as_str()),
        download_url: canonical_download_path(&namespace, &package, record.version.as_str()),
    }
}

fn highlight_document(record: &HighlightPackageRecord) -> HighlightPackageDocument {
    HighlightPackageDocument {
        namespace: record.namespace.as_str().to_owned(),
        package: record.package.as_str().to_owned(),
        version: record.version.as_str().to_owned(),
        package_type: package_type_document(record.package_type),
        description: record.description.clone(),
        published_at: timestamp(record.published_at),
        downloads_30d: record.downloads,
        package_url: canonical_package_path(
            record.namespace.normalized(),
            record.package.normalized(),
        ),
        version_url: canonical_version_path(
            record.namespace.normalized(),
            record.package.normalized(),
            record.version.as_str(),
        ),
    }
}

fn sitemap_document(record: &SitemapEntryRecord) -> SitemapEntryDocument {
    match record.kind {
        SitemapEntryKind::Keyword => {
            let keyword = record.keyword.as_ref().expect("keyword sitemap record");
            SitemapEntryDocument::Keyword {
                keyword: keyword.as_str().to_owned(),
                normalized_keyword: keyword.normalized().to_owned(),
                last_modified: timestamp(record.last_modified),
            }
        }
        SitemapEntryKind::Namespace => {
            let namespace = record.namespace.as_ref().expect("namespace sitemap record");
            SitemapEntryDocument::Namespace {
                namespace: namespace.as_str().to_owned(),
                normalized_namespace: namespace.normalized().to_owned(),
                last_modified: timestamp(record.last_modified),
            }
        }
        SitemapEntryKind::Package => {
            let namespace = record
                .namespace
                .as_ref()
                .expect("package namespace sitemap record");
            let package = record.package.as_ref().expect("package sitemap record");
            SitemapEntryDocument::Package {
                namespace: namespace.as_str().to_owned(),
                normalized_namespace: namespace.normalized().to_owned(),
                package: package.as_str().to_owned(),
                normalized_package: package.normalized().to_owned(),
                last_modified: timestamp(record.last_modified),
            }
        }
    }
}

fn malformed_query_problem() -> Problem {
    invalid_parameter(
        "invalid_query_parameters",
        "query parameters must be unique, known, and correctly typed",
        None,
    )
}

fn discovery_problem(kind: DiscoveryErrorKind, sitemap: bool) -> Problem {
    match kind {
        DiscoveryErrorKind::InvalidNamespace => invalid_parameter(
            "invalid_namespace",
            "must satisfy the registry identity-segment syntax",
            Some("/namespace"),
        ),
        DiscoveryErrorKind::InvalidPackage => invalid_parameter(
            "invalid_package",
            "must satisfy the registry identity-segment syntax",
            Some("/package"),
        ),
        DiscoveryErrorKind::InvalidLimit => invalid_parameter(
            "invalid_limit",
            if sitemap {
                "must be an integer from 1 through 1000"
            } else {
                "must be an integer from 1 through 100"
            },
            Some("/limit"),
        ),
        DiscoveryErrorKind::InvalidCursor => invalid_parameter(
            "invalid_cursor",
            "must be an opaque cursor issued for the same discovery collection",
            Some("/cursor"),
        ),
        DiscoveryErrorKind::PackageNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "package_not_found",
            "The package was not found",
        ),
        DiscoveryErrorKind::Unavailable => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "discovery_unavailable",
            "Package discovery is temporarily unavailable",
        ),
    }
}

fn invalid_parameter(code: &str, detail: &str, pointer: Option<&str>) -> Problem {
    let mut error = ValidationError::new(code, detail);
    if let Some(pointer) = pointer {
        error = error.with_pointer(pointer);
    }
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
        "The request is invalid",
    )
    .with_detail("One or more query or path parameters are invalid.")
    .with_errors(vec![error])
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use rux_application::{
        DependencyRecord, DiscoveryError, DiscoveryPage, HighlightPackageRecord, KeywordRecord,
        PackageHighlightsRecord, PackageKind, PackageVersionHistoryRecord, SitemapEntryRecord,
    };
    use rux_domain::{IdentitySegment, SemanticVersion, VersionRange};
    use serde_json::{Value, json};
    use time::OffsetDateTime;
    use tower::ServiceExt;

    use super::*;

    struct StubDiscovery {
        error: Option<DiscoveryErrorKind>,
    }

    #[async_trait]
    impl Discovery for StubDiscovery {
        async fn dependents(
            &self,
            _namespace: &str,
            _package: &str,
            _parameters: DiscoveryPageParameters,
        ) -> Result<DiscoveryPage<DependentPackageRecord>, DiscoveryError> {
            self.fail()?;
            Ok(DiscoveryPage {
                items: vec![DependentPackageRecord {
                    namespace: identity("Community_Tools"),
                    package: identity("Http_Client"),
                    version: version("1.0.0"),
                    package_type: PackageKind::Source,
                    description: Some("HTTP client".into()),
                    published_at: OffsetDateTime::UNIX_EPOCH,
                    yanked: false,
                    requirements: vec![DependencyRecord {
                        alias: identity("Json"),
                        target_namespace: identity("Rux"),
                        target_package: identity("Json"),
                        version_range: VersionRange::new("^1").unwrap(),
                    }],
                }],
                next_cursor: Some("opaque".into()),
            })
        }

        async fn keywords(
            &self,
            _parameters: DiscoveryPageParameters,
        ) -> Result<DiscoveryPage<KeywordRecord>, DiscoveryError> {
            self.fail()?;
            Ok(DiscoveryPage {
                items: vec![KeywordRecord {
                    keyword: identity("Data_Formats"),
                    package_count: 3,
                }],
                next_cursor: None,
            })
        }

        async fn versions(
            &self,
            _namespace: &str,
            _package: &str,
            _parameters: DiscoveryPageParameters,
        ) -> Result<DiscoveryPage<PackageVersionHistoryRecord>, DiscoveryError> {
            self.fail()?;
            Ok(DiscoveryPage {
                items: vec![PackageVersionHistoryRecord {
                    version: version("1.0.0+linux"),
                    min_rux: version("0.4.0"),
                    package_type: PackageKind::Library,
                    published_at: OffsetDateTime::UNIX_EPOCH,
                    yanked: true,
                }],
                next_cursor: None,
            })
        }

        async fn highlights(&self) -> Result<PackageHighlightsRecord, DiscoveryError> {
            self.fail()?;
            Ok(PackageHighlightsRecord {
                recent: vec![highlight(None)],
                popular: vec![highlight(Some(12))],
            })
        }

        async fn sitemap(
            &self,
            _parameters: DiscoveryPageParameters,
        ) -> Result<DiscoveryPage<SitemapEntryRecord>, DiscoveryError> {
            self.fail()?;
            Ok(DiscoveryPage {
                items: vec![SitemapEntryRecord {
                    kind: SitemapEntryKind::Package,
                    namespace: Some(identity("Rux")),
                    package: Some(identity("Json")),
                    keyword: None,
                    last_modified: OffsetDateTime::UNIX_EPOCH,
                }],
                next_cursor: None,
            })
        }
    }

    impl StubDiscovery {
        fn fail(&self) -> Result<(), DiscoveryError> {
            self.error
                .map_or(Ok(()), |kind| Err(DiscoveryError::new(kind)))
        }
    }

    #[tokio::test]
    async fn dependents_preserve_display_names_requirements_and_urls() {
        let response = test_router(None)
            .oneshot(request("/v1/packages/rux/json/dependents?limit=1"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!({
                "data": [{
                    "namespace": "Community_Tools",
                    "package": "Http_Client",
                    "version": "1.0.0",
                    "package_type": "source",
                    "description": "HTTP client",
                    "published_at": "1970-01-01T00:00:00Z",
                    "yanked": false,
                    "requirements": [{"alias": "Json", "version_range": "^1"}],
                    "package_url": "/v1/packages/community-tools/http-client",
                    "version_url": "/v1/packages/community-tools/http-client/1.0.0"
                }],
                "meta": {"next_cursor": "opaque"}
            })
        );
    }

    #[tokio::test]
    async fn highlights_and_sitemap_expose_bounded_catalog_shapes() {
        let highlights = test_router(None)
            .oneshot(request("/v1/highlights"))
            .await
            .unwrap();
        let body = response_json(highlights).await;
        assert_eq!(body["data"]["window_days"], 30);
        assert_eq!(body["data"]["recent"][0]["downloads_30d"], Value::Null);
        assert_eq!(body["data"]["popular"][0]["downloads_30d"], 12);

        let sitemap = test_router(None)
            .oneshot(request("/v1/sitemap"))
            .await
            .unwrap();
        assert_eq!(
            response_json(sitemap).await["data"][0],
            json!({
                "kind": "package",
                "namespace": "Rux",
                "normalized_namespace": "rux",
                "package": "Json",
                "normalized_package": "json",
                "last_modified": "1970-01-01T00:00:00Z"
            })
        );
    }

    #[tokio::test]
    async fn malformed_queries_and_service_errors_use_problem_contracts() {
        let malformed = test_router(None)
            .oneshot(request("/v1/keywords?unknown=true"))
            .await
            .unwrap();
        assert_eq!(malformed.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(response_json(malformed).await["code"], "invalid_request");

        let missing = test_router(Some(DiscoveryErrorKind::PackageNotFound))
            .oneshot(request("/v1/packages/rux/missing/versions"))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(response_json(missing).await["code"], "package_not_found");
    }

    fn test_router(error: Option<DiscoveryErrorKind>) -> Router {
        router(Arc::new(StubDiscovery { error }))
    }

    fn request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    async fn response_json(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    fn highlight(downloads: Option<u64>) -> HighlightPackageRecord {
        HighlightPackageRecord {
            namespace: identity("Rux"),
            package: identity("Json"),
            version: version("1.0.0"),
            package_type: PackageKind::Library,
            description: None,
            published_at: OffsetDateTime::UNIX_EPOCH,
            downloads,
        }
    }

    fn identity(value: &str) -> IdentitySegment {
        IdentitySegment::new(value).unwrap()
    }

    fn version(value: &str) -> SemanticVersion {
        SemanticVersion::new(value).unwrap()
    }
}
