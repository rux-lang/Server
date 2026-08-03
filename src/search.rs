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
    #[param(minimum = 1, maximum = 100, default = 20)]
    limit: Option<u16>,
    cursor: Option<String>,
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
    package_url: String,
    version_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SearchPageMeta {
    next_cursor: Option<String>,
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
    let parameters = PackageSearchParameters {
        query: query.q,
        namespace: query.namespace,
        keyword: query.keyword,
        package_type: query.package_type,
        limit: query.limit,
        cursor: query.cursor,
    };
    match state.search.search(parameters).await {
        Ok(page) => Json(PackageSearchResponse {
            data: page.items.iter().map(search_document).collect(),
            meta: SearchPageMeta {
                next_cursor: page.next_cursor,
            },
        })
        .into_response(),
        Err(error) => search_problem(error.kind()).into_response(),
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

fn search_problem(kind: PackageSearchErrorKind) -> Problem {
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
        PackageSearchErrorKind::InvalidLimit => invalid_parameter(
            "invalid_limit",
            "must be an integer from 1 through 100",
            Some("/limit"),
        ),
        PackageSearchErrorKind::InvalidCursor => invalid_parameter(
            "invalid_cursor",
            "must be an opaque cursor issued for the same search criteria",
            Some("/cursor"),
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
                next_cursor: Some("opaque-next".into()),
            })
        }
    }

    #[tokio::test]
    async fn search_returns_representative_metadata_and_cursor() {
        let response = test_router(None)
            .oneshot(request("/v1/search?q=json&limit=1"))
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
                    "package_url": "/v1/packages/rux-tools/json-parser",
                    "version_url": "/v1/packages/rux-tools/json-parser/1.2.0"
                }],
                "meta": {"next_cursor": "opaque-next"}
            })
        );
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

        let invalid = test_router(Some(PackageSearchErrorKind::InvalidCursor))
            .oneshot(request("/v1/search?cursor=bad"))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let problem = response_json(invalid).await;
        assert_eq!(problem["code"], "invalid_request");
        assert_eq!(problem["errors"][0]["code"], "invalid_cursor");
        assert_eq!(problem["errors"][0]["pointer"], "/cursor");

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
            match_class: 4,
            relevance: 1_000_000,
        }
    }
}
