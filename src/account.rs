use std::sync::Arc;

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::delete;
use axum::{Json, Router};
use rux_application::{AccountLifecycle, AccountLifecycleErrorKind, Authentication};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::auth::{
    CSRF_COOKIE, CSRF_HEADER, SESSION_COOKIE, append_cleared_session_cookies,
    authentication_problem, cookie_value, origin_matches,
};
use crate::contract::{Problem, ProblemResponse, ValidationError};

#[derive(Clone)]
pub(crate) struct AccountState {
    authentication: Arc<dyn Authentication>,
    accounts: Arc<dyn AccountLifecycle>,
    allowed_web_origin: String,
}

pub fn router(
    authentication: Arc<dyn Authentication>,
    accounts: Arc<dyn AccountLifecycle>,
    allowed_web_origin: String,
) -> Router {
    Router::new()
        .route("/v1/account", delete(delete_account))
        .with_state(AccountState {
            authentication,
            accounts,
            allowed_web_origin,
        })
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteAccountRequest {
    github_login: String,
}

#[utoipa::path(
    delete,
    path = "/account",
    security(("session_cookie" = [], "csrf_cookie" = [], "csrf_header" = [])),
    params(
        ("X-CSRF-Token" = String, Header, description = "CSRF token returned by the session endpoint")
    ),
    request_body = DeleteAccountRequest,
    responses(
        (status = 204, description = "Account anonymized and browser credentials cleared"),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 409, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn delete_account(
    State(state): State<AccountState>,
    headers: HeaderMap,
    payload: Result<Json<DeleteAccountRequest>, JsonRejection>,
) -> Response {
    if !origin_matches(&headers, &state.allowed_web_origin) {
        return csrf_invalid().into_response();
    }
    let session_credential = cookie_value(&headers, SESSION_COOKIE).unwrap_or_default();
    let csrf_cookie = cookie_value(&headers, CSRF_COOKIE);
    let csrf_header = headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok());
    let authenticated = match state
        .authentication
        .authenticate_mutation(&session_credential, csrf_cookie.as_deref(), csrf_header)
        .await
    {
        Ok(authenticated) => authenticated,
        Err(error) => return authentication_problem(error.kind()).into_response(),
    };
    let Ok(Json(payload)) = payload else {
        return invalid_json().into_response();
    };

    match state
        .accounts
        .delete_account(authenticated.user.id, &payload.github_login)
        .await
    {
        Ok(()) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            append_cleared_session_cookies(&mut response);
            response
        }
        Err(error) => account_problem(error.kind()).into_response(),
    }
}

fn invalid_json() -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
        "The request is invalid",
    )
    .with_detail("The request body must contain only the GitHub login confirmation.")
}

fn csrf_invalid() -> Problem {
    Problem::new(
        StatusCode::FORBIDDEN,
        "csrf_invalid",
        "The CSRF token is invalid",
    )
}

fn account_problem(kind: AccountLifecycleErrorKind) -> Problem {
    match kind {
        AccountLifecycleErrorKind::AuthenticationRequired => Problem::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "Authentication is required",
        ),
        AccountLifecycleErrorKind::ConfirmationMismatch => Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            "The request is invalid",
        )
        .with_detail("The GitHub login confirmation does not match the active account.")
        .with_errors(vec![
            ValidationError::new(
                "confirmation_mismatch",
                "must exactly match the current GitHub login",
            )
            .with_pointer("/github_login"),
        ]),
        AccountLifecycleErrorKind::LastOwner => Problem::new(
            StatusCode::CONFLICT,
            "last_owner_required",
            "Every namespace must retain at least one owner",
        )
        .with_detail(
            "Add or promote another owner in every namespace you solely own before deleting your account.",
        ),
        AccountLifecycleErrorKind::Unavailable => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "account_lifecycle_unavailable",
            "Account deletion is temporarily unavailable",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use uuid::Uuid;

    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use axum::http::header::{CONTENT_TYPE, COOKIE, ORIGIN, SET_COOKIE};
    use rux_application::{
        AccountLifecycleError, AuthenticatedSession, AuthenticationError, AuthenticationErrorKind,
        CompletedLogin, LoginStart, UserId, UserRecord,
    };
    use serde_json::Value;
    use time::{Duration, OffsetDateTime};
    use tower::ServiceExt;

    use super::*;

    const SESSION: &str = "session";
    const CSRF: &str = "csrf";

    struct FakeAuthentication;

    #[async_trait]
    impl Authentication for FakeAuthentication {
        async fn begin_github_login(&self) -> Result<LoginStart, AuthenticationError> {
            unreachable!()
        }

        async fn complete_github_login(
            &self,
            _code: &str,
            _callback_state: &str,
            _cookie_state: &str,
        ) -> Result<CompletedLogin, AuthenticationError> {
            unreachable!()
        }

        async fn session(
            &self,
            _session_credential: &str,
            _csrf_credential: Option<&str>,
        ) -> Result<AuthenticatedSession, AuthenticationError> {
            unreachable!()
        }

        async fn authenticate_mutation(
            &self,
            session_credential: &str,
            csrf_cookie: Option<&str>,
            csrf_header: Option<&str>,
        ) -> Result<AuthenticatedSession, AuthenticationError> {
            if session_credential != SESSION {
                return Err(AuthenticationError::new(
                    AuthenticationErrorKind::AuthenticationRequired,
                ));
            }
            if csrf_cookie != Some(CSRF) || csrf_header != Some(CSRF) {
                return Err(AuthenticationError::new(
                    AuthenticationErrorKind::InvalidCsrf,
                ));
            }
            Ok(authenticated_session())
        }

        async fn logout(
            &self,
            _session_credential: &str,
            _csrf_cookie: Option<&str>,
            _csrf_header: Option<&str>,
        ) -> Result<(), AuthenticationError> {
            unreachable!()
        }
    }

    #[derive(Default)]
    struct FakeAccounts {
        calls: Mutex<Vec<(UserId, String)>>,
        error: Option<AccountLifecycleErrorKind>,
    }

    #[async_trait]
    impl AccountLifecycle for FakeAccounts {
        async fn delete_account(
            &self,
            user_id: UserId,
            confirmation: &str,
        ) -> Result<(), AccountLifecycleError> {
            self.calls
                .lock()
                .expect("calls should work")
                .push((user_id, confirmation.into()));
            self.error
                .map_or(Ok(()), |kind| Err(AccountLifecycleError::new(kind)))
        }
    }

    #[tokio::test]
    async fn deletion_requires_origin_session_csrf_and_strict_json() {
        let accounts = Arc::new(FakeAccounts::default());
        let app = test_router(accounts.clone());

        let hostile = app
            .clone()
            .oneshot(request(
                "https://evil.example",
                CSRF,
                r#"{"github_login":"octocat"}"#,
            ))
            .await
            .expect("router should respond");
        assert_eq!(hostile.status(), StatusCode::FORBIDDEN);

        let invalid_csrf = app
            .clone()
            .oneshot(request(
                "https://rux-lang.dev",
                "wrong",
                r#"{"github_login":"octocat"}"#,
            ))
            .await
            .expect("router should respond");
        assert_eq!(invalid_csrf.status(), StatusCode::FORBIDDEN);

        let invalid_json = app
            .oneshot(request(
                "https://rux-lang.dev",
                CSRF,
                r#"{"github_login":"octocat","extra":true}"#,
            ))
            .await
            .expect("router should respond");
        assert_eq!(invalid_json.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert!(accounts.calls.lock().expect("calls should work").is_empty());
    }

    #[tokio::test]
    async fn deletion_returns_no_content_and_clears_both_browser_cookies() {
        let accounts = Arc::new(FakeAccounts::default());
        let response = test_router(accounts.clone())
            .oneshot(request(
                "https://rux-lang.dev",
                CSRF,
                r#"{"github_login":"octocat"}"#,
            ))
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let cookies = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("cookie should be text"))
            .collect::<Vec<_>>();
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("__Host-rux_session=;"))
        );
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("__Host-rux_csrf=;"))
        );
        assert_eq!(
            accounts.calls.lock().expect("calls should work").as_slice(),
            &[(UserId::new(Uuid::from_u128(7)), "octocat".into())]
        );
    }

    #[tokio::test]
    async fn account_failures_have_stable_problem_contracts() {
        for (kind, status, code) in [
            (
                AccountLifecycleErrorKind::ConfirmationMismatch,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                AccountLifecycleErrorKind::LastOwner,
                StatusCode::CONFLICT,
                "last_owner_required",
            ),
            (
                AccountLifecycleErrorKind::Unavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "account_lifecycle_unavailable",
            ),
        ] {
            let accounts = Arc::new(FakeAccounts {
                error: Some(kind),
                ..FakeAccounts::default()
            });
            let response = test_router(accounts)
                .oneshot(request(
                    "https://rux-lang.dev",
                    CSRF,
                    r#"{"github_login":"octocat"}"#,
                ))
                .await
                .expect("router should respond");
            assert_eq!(response.status(), status);
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body should be readable");
            let document: Value = serde_json::from_slice(&body).expect("problem should be JSON");
            assert_eq!(document["code"], code);
            if kind == AccountLifecycleErrorKind::ConfirmationMismatch {
                assert_eq!(document["errors"][0]["pointer"], "/github_login");
            }
        }
    }

    fn request(origin: &str, csrf: &str, body: &'static str) -> Request<Body> {
        Request::builder()
            .method("DELETE")
            .uri("/v1/account")
            .header(ORIGIN, origin)
            .header(
                COOKIE,
                format!("{SESSION_COOKIE}={SESSION}; {CSRF_COOKIE}={CSRF}"),
            )
            .header(CSRF_HEADER, csrf)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .expect("request should build")
    }

    fn test_router(accounts: Arc<FakeAccounts>) -> Router {
        router(
            Arc::new(FakeAuthentication),
            accounts,
            "https://rux-lang.dev".into(),
        )
    }

    fn authenticated_session() -> AuthenticatedSession {
        AuthenticatedSession {
            user: UserRecord {
                id: UserId::new(Uuid::from_u128(7)),
                github_user_id: Some(42),
                github_login: Some("octocat".into()),
                display_name: None,
                avatar_url: None,
                created_at: OffsetDateTime::UNIX_EPOCH,
                updated_at: OffsetDateTime::UNIX_EPOCH,
                anonymized_at: None,
            },
            csrf_credential: CSRF.into(),
            session_credential: None,
            expires_at: OffsetDateTime::UNIX_EPOCH + Duration::days(30),
            remaining_lifetime_seconds: 60,
        }
    }
}
