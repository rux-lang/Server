use std::sync::Arc;

use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rux_application::{
    PackageKind, PackageSearch, PackageSearchErrorKind, PackageSearchParameters,
    PackageSearchRecord,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::{IntoParams, ToSchema};

use crate::contract::{Problem, ProblemResponse, ValidationError};
use crate::metadata::PackageTypeDocument;
use crate::paths::{canonical_package_path, canonical_version_path};

#[derive(Clone)]
pub(crate) struct SearchState {
    search: Arc<dyn PackageSearch>,
}

pub fn router(search: Arc<dyn PackageSearch>) -> Router {
    Router::new()
        .route("/v1/search", get(search_packages))
        .with_state(SearchState { search })
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in = Query)]
pub(crate) struct SearchQuery {
    #[param(max_length = 256)]
    q: Option<String>,
    namespace: Option<String>,
    keyword: Option<String>,
    package_type: Option<String>,
    /// `relevance`, `name`, `downloads`, `recent_downloads`, `updated`, or
    /// `created`. Defaults to `relevance` when `q` is present, `name` otherwise.
    sort: Option<String>,
    /// `asc` or `desc`. Defaults to `asc` for `name` and `desc` for every other
    /// ordering. Relevance only supports `desc`.
    order: Option<String>,
    #[param(minimum = 1, maximum = 10000, default = 1)]
    page: Option<u32>,
    #[param(minimum = 1, maximum = 100, default = 20)]
    per_page: Option<u16>,
    /// Deprecated alias for `per_page`. Supplying both is a validation error.
    #[param(minimum = 1, maximum = 100)]
    limit: Option<u16>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PackageSearchDocument {
    namespace: String,
    package: String,
    version: String,
    package_type: PackageTypeDocument,
    description: Option<String>,
    #[schema(format = "date-time")]
    published_at: String,
    yanked: bool,
    downloads_total: i64,
    downloads_30d: i64,
    package_url: String,
    version_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SearchPageMeta {
    total: u64,
    page: u32,
    per_page: u16,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PackageSearchResponse {
    data: Vec<PackageSearchDocument>,
    meta: SearchPageMeta,
}

#[utoipa::path(
    get,
    path = "/search",
    params(SearchQuery),
    responses(
        (status = 200, description = "Ranked package search or deterministic catalog browse page", body = PackageSearchResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn search_packages(
    State(state): State<SearchState>,
    query: Result<Query<SearchQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return malformed_query_problem().into_response();
    };
    if query.per_page.is_some() && query.limit.is_some() {
        return conflicting_page_size_problem().into_response();
    }
    // Which spelling the client used decides where an out-of-range page size is
    // pointed, so the error names a parameter the request actually contains.
    let page_size_pointer = if query.limit.is_some() {
        "/limit"
    } else {
        "/per_page"
    };
    let parameters = PackageSearchParameters {
        query: query.q,
        namespace: query.namespace,
        keyword: query.keyword,
        package_type: query.package_type,
        sort: query.sort,
        order: query.order,
        page: query.page,
        per_page: query.per_page.or(query.limit),
    };
    match state.search.search(parameters).await {
        Ok(page) => Json(PackageSearchResponse {
            data: page.items.iter().map(search_document).collect(),
            meta: SearchPageMeta {
                total: page.total,
                page: page.page,
                per_page: page.per_page,
            },
        })
        .into_response(),
        Err(error) => search_problem(error.kind(), page_size_pointer).into_response(),
    }
}

fn search_document(record: &PackageSearchRecord) -> PackageSearchDocument {
    PackageSearchDocument {
        namespace: record.namespace.as_str().to_owned(),
        package: record.package.as_str().to_owned(),
        version: record.version.as_str().to_owned(),
        package_type: package_type_document(record.package_type),
        description: record.description.clone(),
        published_at: timestamp(record.published_at),
        yanked: record.yanked,
        downloads_total: record.downloads_total,
        downloads_30d: record.downloads_30d,
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

const fn package_type_document(value: PackageKind) -> PackageTypeDocument {
    match value {
        PackageKind::Program => PackageTypeDocument::Program,
        PackageKind::Library => PackageTypeDocument::Library,
        PackageKind::Source => PackageTypeDocument::Source,
    }
}

fn timestamp(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .expect("UTC search timestamps should format as RFC 3339")
}

fn malformed_query_problem() -> Problem {
    invalid_parameter(
        "invalid_query_parameters",
        "query parameters must be unique, known, and correctly typed",
        None,
    )
}

fn conflicting_page_size_problem() -> Problem {
    invalid_parameter(
        "conflicting_page_size",
        "limit is a deprecated alias for per_page; supply only one of them",
        Some("/per_page"),
    )
}

fn search_problem(kind: PackageSearchErrorKind, page_size_pointer: &str) -> Problem {
    match kind {
        PackageSearchErrorKind::InvalidQuery => invalid_parameter(
            "invalid_search_query",
            "must contain at most 256 UTF-8 bytes after trimming and no NUL character",
            Some("/q"),
        ),
        PackageSearchErrorKind::InvalidNamespace => invalid_parameter(
            "invalid_namespace",
            "must satisfy the registry identity-segment syntax",
            Some("/namespace"),
        ),
        PackageSearchErrorKind::InvalidKeyword => invalid_parameter(
            "invalid_keyword",
            "must satisfy the registry identity-segment syntax",
            Some("/keyword"),
        ),
        PackageSearchErrorKind::InvalidPackageType => invalid_parameter(
            "invalid_package_type",
            "must be program, library, or source",
            Some("/package_type"),
        ),
        PackageSearchErrorKind::InvalidSort => invalid_parameter(
            "invalid_sort",
            "must be relevance, name, downloads, recent_downloads, updated, or created",
            Some("/sort"),
        ),
        PackageSearchErrorKind::InvalidOrder => invalid_parameter(
            "invalid_order",
            "must be asc or desc; relevance only supports desc",
            Some("/order"),
        ),
        PackageSearchErrorKind::InvalidPage => invalid_parameter(
            "invalid_page",
            "must be an integer from 1 through 10000",
            Some("/page"),
        ),
        PackageSearchErrorKind::InvalidPerPage => invalid_parameter(
            "invalid_per_page",
            "must be an integer from 1 through 100",
            Some(page_size_pointer),
        ),
        PackageSearchErrorKind::Unavailable => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "search_unavailable",
            "Package search is temporarily unavailable",
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
    .with_detail("One or more query parameters are invalid.")
    .with_errors(vec![error])
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use rux_application::{PackageSearchError, PackageSearchPage, PackageSearchParameters};
    use rux_domain::{IdentitySegment, SemanticVersion};
    use serde_json::{Value, json};
    use time::macros::datetime;
    use tower::ServiceExt;

    use super::*;

    struct StubSearch {
        error: Option<PackageSearchErrorKind>,
    }

    #[async_trait]
    impl PackageSearch for StubSearch {
        async fn search(
            &self,
            _parameters: PackageSearchParameters,
        ) -> Result<PackageSearchPage, PackageSearchError> {
            if let Some(kind) = self.error {
                return Err(PackageSearchError::new(kind));
            }
            Ok(PackageSearchPage {
                items: vec![fixture()],
                total: 137,
                page: 2,
                per_page: 15,
            })
        }
    }

    #[tokio::test]
    async fn search_returns_representative_metadata_and_page_counts() {
        let response = test_router(None)
            .oneshot(request("/v1/search?q=json&page=2&per_page=15"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!({
                "data": [{
                    "namespace": "Rux_Tools",
                    "package": "Json_Parser",
                    "version": "1.2.0",
                    "package_type": "library",
                    "description": "Literal JSON parsing",
                    "published_at": "2026-08-02T12:00:00Z",
                    "yanked": false,
                    "downloads_total": 4_820,
                    "downloads_30d": 310,
                    "package_url": "/v1/packages/rux-tools/json-parser",
                    "version_url": "/v1/packages/rux-tools/json-parser/1.2.0"
                }],
                "meta": {"total": 137, "page": 2, "per_page": 15}
            })
        );
    }

    #[tokio::test]
    async fn sort_order_and_the_deprecated_limit_alias_are_accepted() {
        for uri in [
            "/v1/search?sort=downloads",
            "/v1/search?sort=recent_downloads",
            "/v1/search?sort=name&order=desc",
            "/v1/search?limit=1",
        ] {
            let response = test_router(None).oneshot(request(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{uri}");
        }
    }

    #[tokio::test]
    async fn malformed_and_domain_query_errors_use_problem_contract() {
        let malformed = test_router(None)
            .oneshot(request("/v1/search?unknown=value"))
            .await
            .unwrap();
        assert_eq!(malformed.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response_json(malformed).await["errors"][0]["code"],
            "invalid_query_parameters"
        );

        // The cursor parameter is gone, so it is now simply unknown.
        let retired = test_router(None)
            .oneshot(request("/v1/search?cursor=bad"))
            .await
            .unwrap();
        assert_eq!(retired.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let invalid = test_router(Some(PackageSearchErrorKind::InvalidSort))
            .oneshot(request("/v1/search?sort=stars"))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let problem = response_json(invalid).await;
        assert_eq!(problem["code"], "invalid_request");
        assert_eq!(problem["errors"][0]["code"], "invalid_sort");
        assert_eq!(problem["errors"][0]["pointer"], "/sort");

        let order = test_router(Some(PackageSearchErrorKind::InvalidOrder))
            .oneshot(request("/v1/search?order=sideways"))
            .await
            .unwrap();
        let problem = response_json(order).await;
        assert_eq!(problem["errors"][0]["code"], "invalid_order");
        assert_eq!(problem["errors"][0]["pointer"], "/order");

        let page = test_router(Some(PackageSearchErrorKind::InvalidPage))
            .oneshot(request("/v1/search?page=0"))
            .await
            .unwrap();
        assert_eq!(response_json(page).await["errors"][0]["pointer"], "/page");

        let unavailable = test_router(Some(PackageSearchErrorKind::Unavailable))
            .oneshot(request("/v1/search"))
            .await
            .unwrap();
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response_json(unavailable).await["code"],
            "search_unavailable"
        );
    }

    #[tokio::test]
    async fn page_size_errors_point_at_the_parameter_the_client_sent() {
        let per_page = test_router(Some(PackageSearchErrorKind::InvalidPerPage))
            .oneshot(request("/v1/search?per_page=0"))
            .await
            .unwrap();
        assert_eq!(
            response_json(per_page).await["errors"][0]["pointer"],
            "/per_page"
        );

        let limit = test_router(Some(PackageSearchErrorKind::InvalidPerPage))
            .oneshot(request("/v1/search?limit=0"))
            .await
            .unwrap();
        assert_eq!(response_json(limit).await["errors"][0]["pointer"], "/limit");
    }

    #[tokio::test]
    async fn supplying_both_page_size_parameters_is_rejected() {
        let response = test_router(None)
            .oneshot(request("/v1/search?limit=5&per_page=5"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            response_json(response).await["errors"][0]["code"],
            "conflicting_page_size"
        );
    }

    fn test_router(error: Option<PackageSearchErrorKind>) -> Router {
        router(Arc::new(StubSearch { error }))
    }

    fn request(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    async fn response_json(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    fn fixture() -> PackageSearchRecord {
        PackageSearchRecord {
            namespace: IdentitySegment::new("Rux_Tools").unwrap(),
            package: IdentitySegment::new("Json_Parser").unwrap(),
            version: SemanticVersion::new("1.2.0").unwrap(),
            package_type: PackageKind::Library,
            description: Some("Literal JSON parsing".into()),
            published_at: datetime!(2026-08-02 12:00 UTC),
            yanked: false,
            downloads_total: 4_820,
            downloads_30d: 310,
        }
    }
}
