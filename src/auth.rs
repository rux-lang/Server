use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::header::{CACHE_CONTROL, COOKIE, LOCATION, ORIGIN, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use rux_application::{
    AuthenticatedSession, Authentication, AuthenticationErrorKind, OAUTH_STATE_LIFETIME_SECONDS,
    SESSION_LIFETIME_SECONDS, oauth_states_match,
};
use serde::Serialize;
use time::format_description::well_known::Rfc3339;
use url::Url;
use utoipa::ToSchema;

use crate::contract::{DataEnvelope, Problem, ProblemResponse};

const OAUTH_STATE_COOKIE: &str = "__Host-rux_oauth_state";
pub(crate) const SESSION_COOKIE: &str = "__Host-rux_session";
pub(crate) const CSRF_COOKIE: &str = "__Host-rux_csrf";
pub(crate) const CSRF_HEADER: &str = "x-csrf-token";

#[derive(Clone)]
pub(crate) struct AuthState {
    authentication: Arc<dyn Authentication>,
    web_callback_url: Url,
    allowed_web_origin: String,
}

pub fn router(
    authentication: Arc<dyn Authentication>,
    web_callback_url: Url,
    allowed_web_origin: String,
) -> Router {
    Router::new()
        .route("/v1/auth/github", get(begin_github_login))
        .route("/v1/auth/github/callback", get(github_callback))
        .route("/v1/auth/session", get(session))
        .route("/v1/auth/logout", post(logout))
        .with_state(AuthState {
            authentication,
            web_callback_url,
            allowed_web_origin,
        })
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SessionDocument {
    user: SessionUser,
    #[schema(format = "date-time")]
    expires_at: String,
    csrf_token: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct SessionUser {
    github_login: String,
    display_name: Option<String>,
    #[schema(format = "uri")]
    avatar_url: Option<String>,
}

#[utoipa::path(
    get,
    path = "/auth/github",
    responses(
        (status = 302, description = "Redirect to GitHub with a browser-bound OAuth state cookie"),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn begin_github_login(State(state): State<AuthState>) -> Response {
    match state.authentication.begin_github_login().await {
        Ok(start) => response_with_headers(
            StatusCode::FOUND,
            &start.authorization_url,
            &[state_cookie(&start.state)],
        ),
        Err(_) => authentication_unavailable().into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/auth/github/callback",
    params(
        ("code" = Option<String>, Query, description = "GitHub authorization code"),
        ("state" = Option<String>, Query, description = "Browser-bound OAuth state"),
        ("error" = Option<String>, Query, description = "GitHub authorization error code")
    ),
    responses(
        (status = 303, description = "Redirect to the fixed web authentication callback")
    )
)]
pub(crate) async fn github_callback(
    State(state): State<AuthState>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let parameters = CallbackParameters::parse(raw_query.as_deref().unwrap_or_default());
    let cookie_state = cookie_value(&headers, OAUTH_STATE_COOKIE);
    let callback_state = parameters.state.as_deref();

    if callback_state.is_none()
        || cookie_state.is_none()
        || !oauth_states_match(
            callback_state.unwrap_or_default(),
            cookie_state.as_deref().unwrap_or_default(),
        )
    {
        return callback_error(&state.web_callback_url, "oauth_state_invalid");
    }
    if parameters.invalid {
        return callback_error(&state.web_callback_url, "oauth_callback_invalid");
    }
    if let Some(error) = parameters.error.as_deref() {
        return callback_error(
            &state.web_callback_url,
            if error == "access_denied" {
                "oauth_access_denied"
            } else {
                "oauth_authorization_failed"
            },
        );
    }
    let Some(code) = parameters.code.as_deref() else {
        return callback_error(&state.web_callback_url, "oauth_callback_invalid");
    };

    match state
        .authentication
        .complete_github_login(
            code,
            callback_state.unwrap_or_default(),
            cookie_state.as_deref().unwrap_or_default(),
        )
        .await
    {
        Ok(completed) => response_with_headers(
            StatusCode::SEE_OTHER,
            state.web_callback_url.as_str(),
            &[
                clear_cookie(OAUTH_STATE_COOKIE),
                session_cookie(&completed.session_credential, SESSION_LIFETIME_SECONDS),
                csrf_cookie(&completed.csrf_credential, SESSION_LIFETIME_SECONDS),
            ],
        ),
        Err(error) => callback_error(&state.web_callback_url, callback_error_code(error.kind())),
    }
}

#[utoipa::path(
    get,
    path = "/auth/session",
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "Active browser session and CSRF token", body = DataEnvelope<SessionDocument>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn session(State(state): State<AuthState>, headers: HeaderMap) -> Response {
    if !origin_matches(&headers, &state.allowed_web_origin) {
        return csrf_invalid().into_response();
    }
    let session_credential = cookie_value(&headers, SESSION_COOKIE).unwrap_or_default();
    let csrf_credential = cookie_value(&headers, CSRF_COOKIE);
    match state
        .authentication
        .session(&session_credential, csrf_credential.as_deref())
        .await
    {
        Ok(authenticated) => session_response(&authenticated),
        Err(error) => authentication_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/auth/logout",
    security(("session_cookie" = [], "csrf_cookie" = [], "csrf_header" = [])),
    params(
        ("X-CSRF-Token" = String, Header, description = "CSRF token returned by the session endpoint")
    ),
    responses(
        (status = 204, description = "Session revoked and cookie cleared"),
        (status = 403, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn logout(State(state): State<AuthState>, headers: HeaderMap) -> Response {
    if !origin_matches(&headers, &state.allowed_web_origin) {
        return csrf_invalid().into_response();
    }
    let credential = cookie_value(&headers, SESSION_COOKIE).unwrap_or_default();
    let csrf_cookie = cookie_value(&headers, CSRF_COOKIE);
    let csrf_header = headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok());
    match state
        .authentication
        .logout(&credential, csrf_cookie.as_deref(), csrf_header)
        .await
    {
        Ok(()) => {
            let mut response = StatusCode::NO_CONTENT.into_response();
            append_cookie(&mut response, &clear_cookie(SESSION_COOKIE));
            append_cookie(&mut response, &clear_cookie(CSRF_COOKIE));
            response
        }
        Err(error) => authentication_problem(error.kind()).into_response(),
    }
}

struct CallbackParameters {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    invalid: bool,
}

impl CallbackParameters {
    fn parse(query: &str) -> Self {
        let mut parsed = Self {
            code: None,
            state: None,
            error: None,
            invalid: false,
        };
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            match name.as_ref() {
                "code" => insert_once(
                    &mut parsed.code,
                    value.into_owned(),
                    2048,
                    &mut parsed.invalid,
                ),
                "state" => insert_once(
                    &mut parsed.state,
                    value.into_owned(),
                    128,
                    &mut parsed.invalid,
                ),
                "error" => insert_once(
                    &mut parsed.error,
                    value.into_owned(),
                    128,
                    &mut parsed.invalid,
                ),
                _ => {}
            }
        }
        if parsed.code.is_some() == parsed.error.is_some() {
            parsed.invalid = true;
        }
        parsed
    }
}

fn insert_once(
    target: &mut Option<String>,
    value: String,
    maximum_length: usize,
    invalid: &mut bool,
) {
    if target.is_some() || value.is_empty() || value.len() > maximum_length {
        *invalid = true;
    } else {
        *target = Some(value);
    }
}

fn callback_error(web_callback_url: &Url, code: &str) -> Response {
    let mut target = web_callback_url.clone();
    target.query_pairs_mut().append_pair("error", code);
    response_with_headers(
        StatusCode::SEE_OTHER,
        target.as_str(),
        &[clear_cookie(OAUTH_STATE_COOKIE)],
    )
}

fn callback_error_code(kind: AuthenticationErrorKind) -> &'static str {
    match kind {
        AuthenticationErrorKind::InvalidState => "oauth_state_invalid",
        AuthenticationErrorKind::AccountConflict => "account_conflict",
        AuthenticationErrorKind::AuthorizationFailed => "oauth_authorization_failed",
        AuthenticationErrorKind::ProviderUnavailable => "oauth_provider_unavailable",
        AuthenticationErrorKind::AuthenticationRequired
        | AuthenticationErrorKind::InvalidCsrf
        | AuthenticationErrorKind::AuthenticationUnavailable => "authentication_unavailable",
    }
}

fn response_with_headers(status: StatusCode, location: &str, cookies: &[String]) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(LOCATION, location);
    for cookie in cookies {
        builder = builder.header(SET_COOKIE, cookie);
    }
    builder
        .body(Body::empty())
        .expect("validated redirects and fixed cookies produce a valid response")
}

fn state_cookie(value: &str) -> String {
    secure_cookie(OAUTH_STATE_COOKIE, value, OAUTH_STATE_LIFETIME_SECONDS)
}

fn session_cookie(value: &str, max_age: i64) -> String {
    secure_cookie(SESSION_COOKIE, value, max_age)
}

fn csrf_cookie(value: &str, max_age: i64) -> String {
    secure_cookie(CSRF_COOKIE, value, max_age)
}

fn secure_cookie(name: &str, value: &str, max_age: i64) -> String {
    format!("{name}={value}; Path=/; Max-Age={max_age}; Secure; HttpOnly; SameSite=Lax")
}

fn clear_cookie(name: &str) -> String {
    secure_cookie(name, "", 0)
}

pub(crate) fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut found = None;
    for header in headers.get_all(COOKIE) {
        let Ok(header) = header.to_str() else {
            return None;
        };
        for cookie in header.split(';') {
            let Some((cookie_name, value)) = cookie.trim().split_once('=') else {
                continue;
            };
            if cookie_name == name {
                if found.is_some() || value.is_empty() {
                    return None;
                }
                found = Some(value.to_owned());
            }
        }
    }
    found
}

fn authentication_unavailable() -> Problem {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "authentication_unavailable",
        "Authentication is temporarily unavailable",
    )
}

fn authentication_required() -> Problem {
    Problem::new(
        StatusCode::UNAUTHORIZED,
        "authentication_required",
        "Authentication is required",
    )
}

fn csrf_invalid() -> Problem {
    Problem::new(
        StatusCode::FORBIDDEN,
        "csrf_invalid",
        "The CSRF token is invalid",
    )
}

pub(crate) fn authentication_problem(kind: AuthenticationErrorKind) -> Problem {
    match kind {
        AuthenticationErrorKind::AuthenticationRequired => authentication_required(),
        AuthenticationErrorKind::InvalidCsrf => csrf_invalid(),
        AuthenticationErrorKind::InvalidState
        | AuthenticationErrorKind::AccountConflict
        | AuthenticationErrorKind::AuthorizationFailed
        | AuthenticationErrorKind::ProviderUnavailable
        | AuthenticationErrorKind::AuthenticationUnavailable => authentication_unavailable(),
    }
}

pub(crate) fn origin_matches(headers: &HeaderMap, allowed_origin: &str) -> bool {
    let mut origins = headers.get_all(ORIGIN).iter();
    let matches = origins
        .next()
        .and_then(|origin| origin.to_str().ok())
        .is_some_and(|origin| origin == allowed_origin);
    matches && origins.next().is_none()
}

fn append_cookie(response: &mut Response, cookie: &str) {
    response.headers_mut().append(
        SET_COOKIE,
        HeaderValue::from_str(cookie).expect("the fixed cookie header is valid"),
    );
}

pub(crate) fn append_cleared_session_cookies(response: &mut Response) {
    append_cookie(response, &clear_cookie(SESSION_COOKIE));
    append_cookie(response, &clear_cookie(CSRF_COOKIE));
}

pub(crate) fn append_rotated_session_cookies(
    response: &mut Response,
    authenticated: &AuthenticatedSession,
) {
    if let Some(session_credential) = authenticated.session_credential.as_deref() {
        append_cookie(
            response,
            &session_cookie(session_credential, authenticated.remaining_lifetime_seconds),
        );
        append_cookie(
            response,
            &csrf_cookie(
                &authenticated.csrf_credential,
                authenticated.remaining_lifetime_seconds,
            ),
        );
    }
}

fn session_response(authenticated: &AuthenticatedSession) -> Response {
    let expires_at = authenticated
        .expires_at
        .format(&Rfc3339)
        .expect("UTC session expiry should format as RFC 3339");
    let document = SessionDocument {
        user: SessionUser {
            github_login: authenticated
                .user
                .github_login
                .clone()
                .expect("authenticated users retain a GitHub login"),
            display_name: authenticated.user.display_name.clone(),
            avatar_url: authenticated.user.avatar_url.clone(),
        },
        expires_at,
        csrf_token: authenticated.csrf_credential.clone(),
    };
    let mut response = axum::Json(DataEnvelope::new(document)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    append_rotated_session_cookies(&mut response, authenticated);
    response
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use uuid::Uuid;

    use async_trait::async_trait;
    use axum::http::Request;
    use rux_application::{AuthenticationError, CompletedLogin, LoginStart, UserId, UserRecord};
    use time::OffsetDateTime;
    use tower::ServiceExt;

    use super::*;

    const STATE: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE";
    const SESSION: &str = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI";
    const CSRF: &str = "AwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwM";

    #[derive(Default)]
    struct FakeAuthentication {
        completed: Mutex<usize>,
        logged_out: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Authentication for FakeAuthentication {
        async fn begin_github_login(&self) -> Result<LoginStart, AuthenticationError> {
            Ok(LoginStart {
                authorization_url: format!(
                    "https://github.com/login/oauth/authorize?client_id=test&state={STATE}"
                ),
                state: STATE.into(),
            })
        }

        async fn complete_github_login(
            &self,
            _code: &str,
            _callback_state: &str,
            _cookie_state: &str,
        ) -> Result<CompletedLogin, AuthenticationError> {
            *self.completed.lock().expect("counter should work") += 1;
            Ok(CompletedLogin {
                session_credential: SESSION.into(),
                csrf_credential: CSRF.into(),
                expires_at: OffsetDateTime::UNIX_EPOCH,
            })
        }

        async fn session(
            &self,
            _session_credential: &str,
            csrf_credential: Option<&str>,
        ) -> Result<AuthenticatedSession, AuthenticationError> {
            Ok(AuthenticatedSession {
                user: UserRecord {
                    id: UserId::new(Uuid::from_u128(7)),
                    github_user_id: Some(42),
                    github_login: Some("octocat".into()),
                    display_name: Some("The Octocat".into()),
                    avatar_url: Some("https://avatars.example/octocat".into()),
                    created_at: OffsetDateTime::UNIX_EPOCH,
                    updated_at: OffsetDateTime::UNIX_EPOCH,
                    anonymized_at: None,
                },
                csrf_credential: csrf_credential.unwrap_or(CSRF).into(),
                session_credential: None,
                expires_at: OffsetDateTime::UNIX_EPOCH,
                remaining_lifetime_seconds: SESSION_LIFETIME_SECONDS,
            })
        }

        async fn authenticate_mutation(
            &self,
            _session_credential: &str,
            csrf_cookie: Option<&str>,
            csrf_header: Option<&str>,
        ) -> Result<AuthenticatedSession, AuthenticationError> {
            if csrf_cookie != Some(CSRF) || csrf_header != Some(CSRF) {
                return Err(AuthenticationError::new(
                    AuthenticationErrorKind::InvalidCsrf,
                ));
            }
            self.session(SESSION, csrf_cookie).await
        }

        async fn logout(
            &self,
            session_credential: &str,
            csrf_cookie: Option<&str>,
            csrf_header: Option<&str>,
        ) -> Result<(), AuthenticationError> {
            if csrf_cookie != Some(CSRF) || csrf_header != Some(CSRF) {
                return Err(AuthenticationError::new(
                    AuthenticationErrorKind::InvalidCsrf,
                ));
            }
            self.logged_out
                .lock()
                .expect("logout list should work")
                .push(session_credential.into());
            Ok(())
        }
    }

    fn test_router(authentication: Arc<FakeAuthentication>) -> Router {
        router(
            authentication,
            Url::parse("https://rux-lang.dev/packages/-/auth/callback")
                .expect("callback should parse"),
            "https://rux-lang.dev".into(),
        )
    }

    #[tokio::test]
    async fn login_sets_hardened_state_cookie_and_redirects_to_github() {
        let response = test_router(Arc::new(FakeAuthentication::default()))
            .oneshot(
                Request::builder()
                    .uri("/v1/auth/github")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers()[LOCATION],
            format!("https://github.com/login/oauth/authorize?client_id=test&state={STATE}")
        );
        let cookie = response.headers()[SET_COOKIE]
            .to_str()
            .expect("cookie should be text");
        assert!(cookie.starts_with(&format!("{OAUTH_STATE_COOKIE}={STATE};")));
        assert!(cookie.contains("Max-Age=600"));
        assert!(cookie.contains("Secure; HttpOnly; SameSite=Lax"));
        assert!(!cookie.contains("Domain="));
    }

    #[tokio::test]
    async fn callback_sets_session_clears_state_and_uses_exact_web_redirect() {
        let authentication = Arc::new(FakeAuthentication::default());
        let response = test_router(authentication.clone())
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/auth/github/callback?code=provider-code&state={STATE}"
                    ))
                    .header(COOKIE, format!("{OAUTH_STATE_COOKIE}={STATE}"))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()[LOCATION],
            "https://rux-lang.dev/packages/-/auth/callback"
        );
        let cookies: Vec<_> = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("cookie should be text"))
            .collect();
        assert_eq!(cookies.len(), 3);
        assert!(cookies[0].contains(&format!("{OAUTH_STATE_COOKIE}=;")));
        assert!(cookies[0].contains("Max-Age=0"));
        assert!(cookies[1].starts_with(&format!("{SESSION_COOKIE}={SESSION};")));
        assert!(cookies[1].contains("Max-Age=2592000"));
        assert!(cookies[2].starts_with(&format!("{CSRF_COOKIE}={CSRF};")));
        assert_eq!(
            *authentication
                .completed
                .lock()
                .expect("counter should work"),
            1
        );
    }

    #[tokio::test]
    async fn callback_errors_are_fixed_and_never_reflect_provider_details() {
        let response = test_router(Arc::new(FakeAuthentication::default()))
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/auth/github/callback?error=access_denied&error_description=secret&state={STATE}"
                    ))
                    .header(COOKIE, format!("{OAUTH_STATE_COOKIE}={STATE}"))
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()[LOCATION],
            "https://rux-lang.dev/packages/-/auth/callback?error=oauth_access_denied"
        );
        assert!(
            !response.headers()[LOCATION]
                .to_str()
                .expect("location should be text")
                .contains("secret")
        );
    }

    #[tokio::test]
    async fn mismatched_state_never_completes_login() {
        let authentication = Arc::new(FakeAuthentication::default());
        let response = test_router(authentication.clone())
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/auth/github/callback?code=code&state={STATE}"))
                    .header(
                        COOKIE,
                        format!("{OAUTH_STATE_COOKIE}=AwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwM"),
                    )
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(
            response.headers()[LOCATION],
            "https://rux-lang.dev/packages/-/auth/callback?error=oauth_state_invalid"
        );
        assert_eq!(
            *authentication
                .completed
                .lock()
                .expect("counter should work"),
            0
        );
    }

    #[tokio::test]
    async fn logout_is_idempotent_and_clears_the_cookie() {
        let authentication = Arc::new(FakeAuthentication::default());
        let response = test_router(authentication.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/logout")
                    .header(ORIGIN, "https://rux-lang.dev")
                    .header(
                        COOKIE,
                        format!("{SESSION_COOKIE}={SESSION}; {CSRF_COOKIE}={CSRF}"),
                    )
                    .header(CSRF_HEADER, CSRF)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            response.headers()[SET_COOKIE]
                .to_str()
                .expect("cookie should be text")
                .contains("Max-Age=0")
        );
        assert_eq!(
            authentication
                .logged_out
                .lock()
                .expect("logout list should work")
                .as_slice(),
            [SESSION]
        );
    }

    #[tokio::test]
    async fn session_returns_user_expiry_and_csrf_without_caching() {
        let response = test_router(Arc::new(FakeAuthentication::default()))
            .oneshot(
                Request::builder()
                    .uri("/v1/auth/session")
                    .header(ORIGIN, "https://rux-lang.dev")
                    .header(
                        COOKIE,
                        format!("{SESSION_COOKIE}={SESSION}; {CSRF_COOKIE}={CSRF}"),
                    )
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("session body should be readable");
        let document: serde_json::Value =
            serde_json::from_slice(&body).expect("session should be JSON");
        assert_eq!(document["data"]["user"]["github_login"], "octocat");
        assert_eq!(document["data"]["csrf_token"], CSRF);
        assert_eq!(document["data"]["expires_at"], "1970-01-01T00:00:00Z");
    }

    #[tokio::test]
    async fn browser_session_routes_reject_hostile_origins_and_invalid_csrf() {
        let authentication = Arc::new(FakeAuthentication::default());
        let hostile = test_router(authentication.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/logout")
                    .header(ORIGIN, "https://evil.example")
                    .header(
                        COOKIE,
                        format!("{SESSION_COOKIE}={SESSION}; {CSRF_COOKIE}={CSRF}"),
                    )
                    .header(CSRF_HEADER, CSRF)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(hostile.status(), StatusCode::FORBIDDEN);
        assert!(hostile.headers().get(SET_COOKIE).is_none());

        let invalid_csrf = test_router(authentication.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/auth/logout")
                    .header(ORIGIN, "https://rux-lang.dev")
                    .header(
                        COOKIE,
                        format!("{SESSION_COOKIE}={SESSION}; {CSRF_COOKIE}={CSRF}"),
                    )
                    .header(CSRF_HEADER, SESSION)
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(invalid_csrf.status(), StatusCode::FORBIDDEN);
        assert!(invalid_csrf.headers().get(SET_COOKIE).is_none());
        assert!(
            authentication
                .logged_out
                .lock()
                .expect("logout list should work")
                .is_empty()
        );
    }
}
