use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::patch;
use axum::{Json, Router};
use rux_application::{PackageVersionYankState, YankErrorKind, Yanks};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::contract::{DataEnvelope, Problem, ProblemResponse, ValidationError};

#[derive(Clone)]
pub(crate) struct YankState {
    yanks: Arc<dyn Yanks>,
}

pub fn router(yanks: Arc<dyn Yanks>) -> Router {
    Router::new()
        .route(
            "/v1/packages/{namespace}/{package}/{version}",
            patch(set_yank_state),
        )
        .with_state(YankState { yanks })
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SetYankStateRequest {
    yanked: bool,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PackageVersionYankStateDocument {
    namespace: String,
    package: String,
    version: String,
    yanked: bool,
}

#[utoipa::path(
    patch,
    path = "/packages/{namespace}/{package}/{version}",
    params(
        ("namespace" = String, Path, description = "Registry namespace identity"),
        ("package" = String, Path, description = "Package identity"),
        ("version" = String, Path, description = "Exact strict Semantic Version")
    ),
    security(("bearer_token" = [])),
    request_body = SetYankStateRequest,
    responses(
        (status = 200, description = "Resulting package version yank state", body = DataEnvelope<PackageVersionYankStateDocument>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 404, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn set_yank_state(
    State(state): State<YankState>,
    headers: HeaderMap,
    Path((namespace, package, version)): Path<(String, String, String)>,
    payload: Result<Json<SetYankStateRequest>, JsonRejection>,
) -> Response {
    let Some(credential) = bearer_credential(&headers) else {
        return yank_problem(YankErrorKind::AuthenticationRequired).into_response();
    };
    let Ok(Json(payload)) = payload else {
        return invalid_json().into_response();
    };
    match state
        .yanks
        .set_yanked(&credential, &namespace, &package, &version, payload.yanked)
        .await
    {
        Ok(result) => Json(DataEnvelope::new(yank_document(&result))).into_response(),
        Err(error) => yank_problem(error.kind()).into_response(),
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

fn yank_document(state: &PackageVersionYankState) -> PackageVersionYankStateDocument {
    PackageVersionYankStateDocument {
        namespace: state.namespace.as_str().to_owned(),
        package: state.package.as_str().to_owned(),
        version: state.version.as_str().to_owned(),
        yanked: state.yanked,
    }
}

fn invalid_json() -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
        "The request is invalid",
    )
    .with_detail("The request body must match the package version yank-state schema.")
}

fn yank_problem(kind: YankErrorKind) -> Problem {
    match kind {
        YankErrorKind::InvalidNamespace => invalid_path(
            "invalid_namespace",
            "must satisfy the registry identity-segment syntax",
            "/namespace",
        ),
        YankErrorKind::InvalidPackage => invalid_path(
            "invalid_package",
            "must satisfy the registry identity-segment syntax",
            "/package",
        ),
        YankErrorKind::InvalidVersion => invalid_path(
            "invalid_version",
            "must be a strict Semantic Version",
            "/version",
        ),
        YankErrorKind::AuthenticationRequired => Problem::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "Authentication is required",
        ),
        YankErrorKind::InsufficientScope => Problem::new(
            StatusCode::FORBIDDEN,
            "insufficient_scope",
            "The API token does not grant the required scope",
        ),
        YankErrorKind::Forbidden => Problem::new(
            StatusCode::FORBIDDEN,
            "yank_forbidden",
            "The caller cannot change this package version's yank state",
        ),
        YankErrorKind::PackageVersionNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "package_version_not_found",
            "The package version was not found",
        ),
        YankErrorKind::Unavailable => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "yank_unavailable",
            "The package version yank state is temporarily unavailable",
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use rux_application::{
        ResolverIndexError, ResolverIndexRecord, ResolverIndexes, ResolverVersionRecord, YankError,
    };
    use rux_domain::{IdentitySegment, SemanticVersion};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    struct StubYanks {
        error: Option<YankErrorKind>,
    }

    #[async_trait]
    impl Yanks for StubYanks {
        async fn set_yanked(
            &self,
            _credential: &str,
            _namespace: &str,
            _package: &str,
            _version: &str,
            yanked: bool,
        ) -> Result<PackageVersionYankState, YankError> {
            if let Some(kind) = self.error {
                return Err(YankError::new(kind));
            }
            Ok(PackageVersionYankState {
                namespace: IdentitySegment::new("Rux_Tools").unwrap(),
                package: IdentitySegment::new("Example_Pkg").unwrap(),
                version: SemanticVersion::new("1.2.3+linux").unwrap(),
                yanked,
            })
        }
    }

    #[tokio::test]
    async fn patch_returns_the_resulting_display_preserving_state() {
        let response = test_router(None)
            .oneshot(request(r#"{"yanked":true}"#, true))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!({"data": {
                "namespace": "Rux_Tools",
                "package": "Example_Pkg",
                "version": "1.2.3+linux",
                "yanked": true
            }})
        );
    }

    #[tokio::test]
    async fn patch_requires_a_strict_body_and_bearer_credential() {
        let missing_auth = test_router(None)
            .oneshot(request(r#"{"yanked":true}"#, false))
            .await
            .unwrap();
        assert_eq!(missing_auth.status(), StatusCode::UNAUTHORIZED);

        for body in [r"{}", r#"{"yanked":"yes"}"#, r#"{"yanked":true,"extra":1}"#] {
            let response = test_router(None)
                .oneshot(request(body, true))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(response_json(response).await["code"], "invalid_request");
        }
    }

    #[tokio::test]
    async fn yank_failures_use_stable_problem_contracts() {
        let cases = [
            (
                YankErrorKind::InvalidNamespace,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                YankErrorKind::InvalidPackage,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                YankErrorKind::InvalidVersion,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                YankErrorKind::AuthenticationRequired,
                StatusCode::UNAUTHORIZED,
                "authentication_required",
            ),
            (
                YankErrorKind::InsufficientScope,
                StatusCode::FORBIDDEN,
                "insufficient_scope",
            ),
            (
                YankErrorKind::Forbidden,
                StatusCode::FORBIDDEN,
                "yank_forbidden",
            ),
            (
                YankErrorKind::PackageVersionNotFound,
                StatusCode::NOT_FOUND,
                "package_version_not_found",
            ),
            (
                YankErrorKind::Unavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "yank_unavailable",
            ),
        ];
        for (kind, status, code) in cases {
            let response = test_router(Some(kind))
                .oneshot(request(r#"{"yanked":false}"#, true))
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(response_json(response).await["code"], code);
        }
    }

    struct SharedCatalog {
        yanked: AtomicBool,
    }

    #[async_trait]
    impl Yanks for SharedCatalog {
        async fn set_yanked(
            &self,
            _credential: &str,
            _namespace: &str,
            _package: &str,
            _version: &str,
            yanked: bool,
        ) -> Result<PackageVersionYankState, YankError> {
            self.yanked.store(yanked, Ordering::SeqCst);
            Ok(PackageVersionYankState {
                namespace: IdentitySegment::new("Rux").unwrap(),
                package: IdentitySegment::new("Example").unwrap(),
                version: SemanticVersion::new("1.0.0").unwrap(),
                yanked,
            })
        }
    }

    #[async_trait]
    impl ResolverIndexes for SharedCatalog {
        async fn get(
            &self,
            _namespace: &str,
            _package: &str,
        ) -> Result<ResolverIndexRecord, ResolverIndexError> {
            Ok(ResolverIndexRecord {
                namespace: IdentitySegment::new("Rux").unwrap(),
                package: IdentitySegment::new("Example").unwrap(),
                versions: vec![ResolverVersionRecord {
                    version: SemanticVersion::new("1.0.0").unwrap(),
                    min_rux: SemanticVersion::new("0.4.0").unwrap(),
                    yanked: self.yanked.load(Ordering::SeqCst),
                    dependencies: Vec::new(),
                }],
            })
        }
    }

    #[tokio::test]
    async fn committed_yank_state_immediately_changes_the_resolver_etag() {
        let catalog = Arc::new(SharedCatalog {
            yanked: AtomicBool::new(false),
        });
        let app = router(catalog.clone()).merge(crate::resolver::router(catalog));
        let initial = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/v1/index/rux/example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(initial.status(), StatusCode::OK);
        let initial_etag = initial.headers()["etag"].clone();

        let changed = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri("/v1/packages/rux/example/1.0.0")
                    .header(AUTHORIZATION, "Bearer rux_pat_example")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"yanked":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(changed.status(), StatusCode::OK);

        let refreshed = app
            .oneshot(
                Request::builder()
                    .uri("/v1/index/rux/example")
                    .header("if-none-match", &initial_etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(refreshed.status(), StatusCode::OK);
        assert_ne!(refreshed.headers()["etag"], initial_etag);
        assert_eq!(
            response_json(refreshed).await["data"]["versions"][0]["yanked"],
            true
        );
    }

    fn test_router(error: Option<YankErrorKind>) -> Router {
        router(Arc::new(StubYanks { error }))
    }

    fn request(body: &str, authenticated: bool) -> Request<Body> {
        let mut request = Request::builder()
            .method("PATCH")
            .uri("/v1/packages/rux-tools/example-pkg/1.2.3+linux")
            .header("content-type", "application/json");
        if authenticated {
            request = request.header(AUTHORIZATION, "Bearer rux_pat_example");
        }
        request.body(Body::from(body.to_owned())).unwrap()
    }

    async fn response_json(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
    }
}
