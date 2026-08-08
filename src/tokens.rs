use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use rux_application::{
    ApiTokenErrorKind, ApiTokenIdentity, ApiTokenStatus, ApiTokenSummary, ApiTokens,
    Authentication, IssueApiToken, IssuedApiToken, TokenScope, UserId,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::ToSchema;

use crate::auth::{
    CSRF_COOKIE, CSRF_HEADER, SESSION_COOKIE, append_rotated_session_cookies,
    authentication_problem, cookie_value, origin_matches,
};
use crate::contract::{DataEnvelope, Problem, ProblemResponse, ValidationError};

#[derive(Clone)]
pub(crate) struct TokenState {
    authentication: Arc<dyn Authentication>,
    tokens: Arc<dyn ApiTokens>,
    allowed_web_origin: String,
}

pub fn router(
    authentication: Arc<dyn Authentication>,
    tokens: Arc<dyn ApiTokens>,
    allowed_web_origin: String,
) -> Router {
    Router::new()
        .route("/v1/me", get(identify))
        .route("/v1/tokens", get(list_tokens).post(issue_token))
        .route("/v1/tokens/{token_prefix}", delete(revoke_token))
        .with_state(TokenState {
            authentication,
            tokens,
            allowed_web_origin,
        })
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TokenScopeDocument {
    Publish,
    Yank,
    Namespace,
}

#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TokenStatusDocument {
    Active,
    Expired,
    Revoked,
}

impl From<ApiTokenStatus> for TokenStatusDocument {
    fn from(value: ApiTokenStatus) -> Self {
        match value {
            ApiTokenStatus::Active => Self::Active,
            ApiTokenStatus::Expired => Self::Expired,
            ApiTokenStatus::Revoked => Self::Revoked,
        }
    }
}

impl From<TokenScopeDocument> for TokenScope {
    fn from(value: TokenScopeDocument) -> Self {
        match value {
            TokenScopeDocument::Publish => Self::Publish,
            TokenScopeDocument::Yank => Self::Yank,
            TokenScopeDocument::Namespace => Self::Namespace,
        }
    }
}

impl From<TokenScope> for TokenScopeDocument {
    fn from(value: TokenScope) -> Self {
        match value {
            TokenScope::Publish => Self::Publish,
            TokenScope::Yank => Self::Yank,
            TokenScope::Namespace => Self::Namespace,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct IssueTokenRequest {
    display_name: String,
    #[schema(min_items = 1, max_items = 3)]
    scopes: Vec<TokenScopeDocument>,
    #[schema(format = "date-time")]
    expires_at: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct TokenDocument {
    display_name: String,
    token_prefix: String,
    scopes: Vec<TokenScopeDocument>,
    #[schema(format = "date-time")]
    created_at: String,
    #[schema(format = "date-time")]
    last_used_at: Option<String>,
    #[schema(format = "date-time")]
    expires_at: Option<String>,
    #[schema(format = "date-time")]
    revoked_at: Option<String>,
    status: TokenStatusDocument,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct IssuedTokenDocument {
    credential: String,
    #[serde(flatten)]
    token: TokenDocument,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct IdentityDocument {
    github_login: String,
    token_prefix: String,
    scopes: Vec<TokenScopeDocument>,
    #[schema(format = "date-time")]
    expires_at: Option<String>,
}

#[utoipa::path(
    get,
    path = "/me",
    security(("bearer_token" = [])),
    responses(
        (status = 200, description = "The identity a bearer token proves", body = DataEnvelope<IdentityDocument>),
        (status = 401, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn identify(State(state): State<TokenState>, headers: HeaderMap) -> Response {
    // The only token endpoint that asserts no scope: a client has to be able to
    // check a credential before relying on it, and every other bearer route
    // demands a scope the credential may legitimately lack.
    let Some(credential) = bearer_credential(&headers) else {
        return token_problem(ApiTokenErrorKind::AuthenticationRequired).into_response();
    };
    match state.tokens.identify(&credential).await {
        Ok(identity) => {
            let mut response = Json(DataEnvelope::new(identity_document(identity))).into_response();
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(error) => token_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/tokens",
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "API token history", body = DataEnvelope<Vec<TokenDocument>>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn list_tokens(State(state): State<TokenState>, headers: HeaderMap) -> Response {
    let authenticated = match authenticate_read(&state, &headers).await {
        Ok(authenticated) => authenticated,
        Err(problem) => return problem.into_response(),
    };
    match state.tokens.list(authenticated.user.id).await {
        Ok(tokens) => {
            let documents = tokens.into_iter().map(token_document).collect::<Vec<_>>();
            let mut response = Json(DataEnvelope::new(documents)).into_response();
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            append_rotated_session_cookies(&mut response, &authenticated);
            response
        }
        Err(error) => token_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/tokens",
    security(("session_cookie" = [], "csrf_cookie" = [], "csrf_header" = [])),
    request_body = IssueTokenRequest,
    responses(
        (status = 201, description = "New API token with its one-time credential", body = DataEnvelope<IssuedTokenDocument>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn issue_token(
    State(state): State<TokenState>,
    headers: HeaderMap,
    payload: Result<Json<IssueTokenRequest>, JsonRejection>,
) -> Response {
    let user_id = match authenticate_mutation(&state, &headers).await {
        Ok(user_id) => user_id,
        Err(problem) => return problem.into_response(),
    };
    let Ok(Json(payload)) = payload else {
        return invalid_json().into_response();
    };
    let expires_at = match payload.expires_at {
        Some(value) => match OffsetDateTime::parse(&value, &Rfc3339) {
            Ok(value) => Some(value),
            Err(_) => {
                return invalid_field(
                    "invalid_expiration",
                    "must be an RFC 3339 timestamp",
                    "/expires_at",
                )
                .into_response();
            }
        },
        None => None,
    };
    let request = IssueApiToken {
        display_name: payload.display_name,
        scopes: payload.scopes.into_iter().map(Into::into).collect(),
        expires_at,
    };
    match state.tokens.issue(user_id, request).await {
        Ok(token) => {
            let mut response = (
                StatusCode::CREATED,
                Json(DataEnvelope::new(issued_token_document(token))),
            )
                .into_response();
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(error) => token_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/tokens/{token_prefix}",
    params(("token_prefix" = String, Path, description = "Safe token display prefix")),
    security(("session_cookie" = [], "csrf_cookie" = [], "csrf_header" = [])),
    responses(
        (status = 204, description = "Token revoked or already absent"),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn revoke_token(
    State(state): State<TokenState>,
    headers: HeaderMap,
    Path(token_prefix): Path<String>,
) -> Response {
    let user_id = match authenticate_mutation(&state, &headers).await {
        Ok(user_id) => user_id,
        Err(problem) => return problem.into_response(),
    };
    match state.tokens.revoke(user_id, &token_prefix).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => token_problem(error.kind()).into_response(),
    }
}

async fn authenticate_read(
    state: &TokenState,
    headers: &HeaderMap,
) -> Result<rux_application::AuthenticatedSession, Problem> {
    if !origin_matches(headers, &state.allowed_web_origin) {
        return Err(csrf_invalid());
    }
    state
        .authentication
        .session(
            &cookie_value(headers, SESSION_COOKIE).unwrap_or_default(),
            cookie_value(headers, CSRF_COOKIE).as_deref(),
        )
        .await
        .map_err(|error| authentication_problem(error.kind()))
}

async fn authenticate_mutation(state: &TokenState, headers: &HeaderMap) -> Result<UserId, Problem> {
    if !origin_matches(headers, &state.allowed_web_origin) {
        return Err(csrf_invalid());
    }
    state
        .authentication
        .authenticate_mutation(
            &cookie_value(headers, SESSION_COOKIE).unwrap_or_default(),
            cookie_value(headers, CSRF_COOKIE).as_deref(),
            headers
                .get(CSRF_HEADER)
                .and_then(|value| value.to_str().ok()),
        )
        .await
        .map(|authenticated| authenticated.user.id)
        .map_err(|error| authentication_problem(error.kind()))
}

fn token_document(token: ApiTokenSummary) -> TokenDocument {
    TokenDocument {
        display_name: token.display_name,
        token_prefix: token.token_prefix,
        scopes: token.scopes.into_iter().map(Into::into).collect(),
        created_at: timestamp(token.created_at),
        last_used_at: token.last_used_at.map(timestamp),
        expires_at: token.expires_at.map(timestamp),
        revoked_at: token.revoked_at.map(timestamp),
        status: token.status.into(),
    }
}

fn identity_document(identity: ApiTokenIdentity) -> IdentityDocument {
    IdentityDocument {
        github_login: identity.github_login,
        token_prefix: identity.token_prefix,
        scopes: identity.scopes.into_iter().map(Into::into).collect(),
        expires_at: identity.expires_at.map(timestamp),
    }
}

/// The raw credential from an `Authorization: Bearer` header, if the header
/// carries one that could plausibly be a token.
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

fn issued_token_document(token: IssuedApiToken) -> IssuedTokenDocument {
    IssuedTokenDocument {
        credential: token.credential,
        token: token_document(token.token),
    }
}

fn timestamp(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .expect("UTC token timestamps should format as RFC 3339")
}

fn csrf_invalid() -> Problem {
    Problem::new(
        StatusCode::FORBIDDEN,
        "csrf_invalid",
        "The CSRF token is invalid",
    )
}

fn invalid_json() -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
        "The request is invalid",
    )
    .with_detail("The request body must match the API token schema.")
}

fn invalid_field(code: &str, detail: &str, pointer: &str) -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
        "The request is invalid",
    )
    .with_detail("One or more fields are invalid.")
    .with_errors(vec![
        ValidationError::new(code, detail).with_pointer(pointer),
    ])
}

fn token_problem(kind: ApiTokenErrorKind) -> Problem {
    match kind {
        ApiTokenErrorKind::InvalidDisplayName => invalid_field(
            "invalid_display_name",
            "must contain between 1 and 100 UTF-8 bytes after trimming",
            "/display_name",
        ),
        ApiTokenErrorKind::InvalidScopes => invalid_field(
            "invalid_scopes",
            "must contain one to three unique supported scopes",
            "/scopes",
        ),
        ApiTokenErrorKind::InvalidExpiration => invalid_field(
            "invalid_expiration",
            "must be later than the current time",
            "/expires_at",
        ),
        ApiTokenErrorKind::AuthenticationRequired => Problem::new(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
            "Authentication is required",
        ),
        ApiTokenErrorKind::InsufficientScope => Problem::new(
            StatusCode::FORBIDDEN,
            "insufficient_scope",
            "The API token does not grant the required scope",
        ),
        ApiTokenErrorKind::Unavailable => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "token_service_unavailable",
            "API token management is temporarily unavailable",
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use uuid::Uuid;

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use axum::http::header::{CONTENT_TYPE, COOKIE, ORIGIN};
    use rux_application::{
        ApiTokenError, AuthenticatedSession, AuthenticationError, AuthenticationErrorKind,
        CompletedLogin, IssuedApiToken, LoginStart, UserRecord,
    };
    use time::Duration;
    use tower::ServiceExt;

    use super::*;

    const SESSION: &str = "session";
    const CSRF: &str = "csrf";
    const BEARER: &str = "rux_pat_AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE";

    #[derive(Default)]
    struct FakeAuthentication;

    #[async_trait]
    impl Authentication for FakeAuthentication {
        async fn begin_github_login(&self) -> Result<LoginStart, AuthenticationError> {
            Err(AuthenticationError::new(
                AuthenticationErrorKind::AuthenticationUnavailable,
            ))
        }

        async fn complete_github_login(
            &self,
            _code: &str,
            _callback_state: &str,
            _cookie_state: &str,
        ) -> Result<CompletedLogin, AuthenticationError> {
            Err(AuthenticationError::new(
                AuthenticationErrorKind::AuthenticationUnavailable,
            ))
        }

        async fn session(
            &self,
            session_credential: &str,
            _csrf_credential: Option<&str>,
        ) -> Result<AuthenticatedSession, AuthenticationError> {
            if session_credential != SESSION {
                return Err(AuthenticationError::new(
                    AuthenticationErrorKind::AuthenticationRequired,
                ));
            }
            Ok(authenticated_session())
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
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeTokens {
        revoked: Mutex<Vec<(UserId, String)>>,
    }

    #[async_trait]
    impl ApiTokens for FakeTokens {
        async fn issue(
            &self,
            _user_id: UserId,
            request: IssueApiToken,
        ) -> Result<IssuedApiToken, ApiTokenError> {
            Ok(IssuedApiToken {
                credential: "rux_pat_AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE".into(),
                token: ApiTokenSummary {
                    display_name: request.display_name,
                    token_prefix: "rux_pat_AQEBAQEB".into(),
                    scopes: request.scopes,
                    created_at: OffsetDateTime::UNIX_EPOCH,
                    last_used_at: None,
                    expires_at: request.expires_at,
                    revoked_at: None,
                    status: ApiTokenStatus::Active,
                },
            })
        }

        async fn list(&self, _user_id: UserId) -> Result<Vec<ApiTokenSummary>, ApiTokenError> {
            Ok(vec![ApiTokenSummary {
                display_name: "Old release token".into(),
                token_prefix: "rux_pat_AQEBAQEB".into(),
                scopes: vec![TokenScope::Publish],
                created_at: OffsetDateTime::UNIX_EPOCH,
                last_used_at: Some(OffsetDateTime::UNIX_EPOCH),
                expires_at: None,
                revoked_at: Some(OffsetDateTime::UNIX_EPOCH + Duration::hours(1)),
                status: ApiTokenStatus::Revoked,
            }])
        }

        async fn revoke(&self, user_id: UserId, prefix: &str) -> Result<(), ApiTokenError> {
            self.revoked
                .lock()
                .expect("revoke list should work")
                .push((user_id, prefix.into()));
            Ok(())
        }

        async fn identify(&self, credential: &str) -> Result<ApiTokenIdentity, ApiTokenError> {
            if credential != BEARER {
                return Err(ApiTokenError::new(
                    ApiTokenErrorKind::AuthenticationRequired,
                ));
            }
            Ok(ApiTokenIdentity {
                github_login: "octocat".into(),
                token_prefix: "rux_pat_AQEBAQEB".into(),
                scopes: vec![TokenScope::Publish],
                expires_at: None,
            })
        }
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

    fn test_router(tokens: Arc<FakeTokens>) -> Router {
        router(
            Arc::new(FakeAuthentication),
            tokens,
            "https://rux-lang.dev".into(),
        )
    }

    fn browser_request(method: &str, uri: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(ORIGIN, "https://rux-lang.dev")
            .header(
                COOKIE,
                format!("{SESSION_COOKIE}={SESSION}; {CSRF_COOKIE}={CSRF}"),
            )
            .header(CSRF_HEADER, CSRF)
    }

    #[tokio::test]
    async fn identify_reports_the_owner_and_scopes_of_a_bearer_token() {
        let response = test_router(Arc::new(FakeTokens::default()))
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/me")
                    .header(AUTHORIZATION, format!("Bearer {BEARER}"))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let document: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(document["data"]["github_login"], "octocat");
        assert_eq!(document["data"]["scopes"][0], "publish");
        assert_eq!(document["data"]["token_prefix"], "rux_pat_AQEBAQEB");
        // The endpoint proves a credential; it must never echo one back.
        assert!(document["data"].get("credential").is_none());
    }

    #[tokio::test]
    async fn identify_needs_a_bearer_token_and_no_session_or_origin() {
        let missing = test_router(Arc::new(FakeTokens::default()))
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/me")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let unknown = test_router(Arc::new(FakeTokens::default()))
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/me")
                    .header(AUTHORIZATION, "Bearer rux_pat_not_a_real_credential")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(unknown.status(), StatusCode::UNAUTHORIZED);

        // A session cookie is not a substitute: this route is bearer-only, which
        // is what makes it reachable from the CLI with no browser involved.
        let session_only = test_router(Arc::new(FakeTokens::default()))
            .oneshot(
                browser_request("GET", "/v1/me")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(session_only.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_returns_secret_free_history_without_caching() {
        let response = test_router(Arc::new(FakeTokens::default()))
            .oneshot(
                browser_request("GET", "/v1/tokens")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let document: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(document["data"][0]["status"], "revoked");
        assert!(document["data"][0].get("credential").is_none());
    }

    #[tokio::test]
    async fn issue_returns_the_credential_and_validates_timestamp_shape() {
        let response = test_router(Arc::new(FakeTokens::default()))
            .oneshot(
                browser_request("POST", "/v1/tokens")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"display_name":"CI","scopes":["publish"],"expires_at":null}"#,
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let document: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert!(
            document["data"]["credential"]
                .as_str()
                .expect("credential")
                .starts_with("rux_pat_")
        );

        let invalid = test_router(Arc::new(FakeTokens::default()))
            .oneshot(
                browser_request("POST", "/v1/tokens")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"display_name":"CI","scopes":["publish"],"expires_at":"tomorrow"}"#,
                    ))
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn revoke_requires_the_exact_origin_and_csrf_then_remains_idempotent() {
        let tokens = Arc::new(FakeTokens::default());
        let hostile = test_router(tokens.clone())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/tokens/rux_pat_AQEBAQEB")
                    .header(ORIGIN, "https://evil.example")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(hostile.status(), StatusCode::FORBIDDEN);
        assert!(
            tokens
                .revoked
                .lock()
                .expect("revoke list should work")
                .is_empty()
        );

        for _ in 0..2 {
            let response = test_router(tokens.clone())
                .oneshot(
                    browser_request("DELETE", "/v1/tokens/rux_pat_AQEBAQEB")
                        .body(Body::empty())
                        .expect("request should build"),
                )
                .await
                .expect("router should respond");
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }
        assert_eq!(
            tokens
                .revoked
                .lock()
                .expect("revoke list should work")
                .len(),
            2
        );
    }
}
