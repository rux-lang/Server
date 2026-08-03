use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rux_application::{
    AuthenticatedSession, Authentication, NamespaceActor, NamespaceErrorKind, NamespaceInvitation,
    NamespaceMember, NamespaceRole, NamespaceSummary, NamespaceUser, Namespaces,
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
pub(crate) struct NamespaceState {
    authentication: Arc<dyn Authentication>,
    namespaces: Arc<dyn Namespaces>,
    allowed_web_origin: String,
}

pub fn router(
    authentication: Arc<dyn Authentication>,
    namespaces: Arc<dyn Namespaces>,
    allowed_web_origin: String,
) -> Router {
    Router::new()
        .route("/v1/namespaces", get(list_namespaces).post(claim_namespace))
        .route("/v1/namespaces/{namespace}/members", get(list_members))
        .route(
            "/v1/namespaces/{namespace}/members/{github_login}",
            axum::routing::patch(set_member_role).delete(remove_member),
        )
        .route(
            "/v1/namespaces/{namespace}/invitations",
            get(list_namespace_invitations).post(invite_member),
        )
        .route(
            "/v1/namespaces/{namespace}/invitations/{github_login}",
            axum::routing::delete(revoke_invitation),
        )
        .route("/v1/invitations", get(list_my_invitations))
        .route(
            "/v1/invitations/{namespace}/accept",
            post(accept_invitation),
        )
        .with_state(NamespaceState {
            authentication,
            namespaces,
            allowed_web_origin,
        })
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NamespaceRoleDocument {
    Owner,
    Maintainer,
}

impl From<NamespaceRoleDocument> for NamespaceRole {
    fn from(value: NamespaceRoleDocument) -> Self {
        match value {
            NamespaceRoleDocument::Owner => Self::Owner,
            NamespaceRoleDocument::Maintainer => Self::Maintainer,
        }
    }
}

impl From<NamespaceRole> for NamespaceRoleDocument {
    fn from(value: NamespaceRole) -> Self {
        match value {
            NamespaceRole::Owner => Self::Owner,
            NamespaceRole::Maintainer => Self::Maintainer,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaimNamespaceRequest {
    name: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct InviteNamespaceMemberRequest {
    github_login: String,
    role: NamespaceRoleDocument,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateNamespaceMemberRequest {
    role: NamespaceRoleDocument,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct NamespaceUserDocument {
    github_login: String,
    display_name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct NamespaceDocument {
    name: String,
    role: NamespaceRoleDocument,
    #[schema(format = "date-time")]
    created_at: String,
    #[schema(format = "date-time")]
    updated_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct NamespaceMemberDocument {
    user: NamespaceUserDocument,
    role: NamespaceRoleDocument,
    #[schema(format = "date-time")]
    created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct NamespaceInvitationDocument {
    namespace: String,
    invited_user: NamespaceUserDocument,
    invited_by: Option<NamespaceUserDocument>,
    role: NamespaceRoleDocument,
    #[schema(format = "date-time")]
    created_at: String,
    #[schema(format = "date-time")]
    expires_at: String,
}

#[utoipa::path(
    get,
    path = "/namespaces",
    security(("session_cookie" = []), ("bearer_token" = [])),
    responses(
        (status = 200, body = DataEnvelope<Vec<NamespaceDocument>>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn list_namespaces(
    State(state): State<NamespaceState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authenticate_read(&state, &headers).await {
        Ok(auth) => auth,
        Err(problem) => return problem.into_response(),
    };
    match state.namespaces.list(auth.actor.clone()).await {
        Ok(namespaces) => read_json(
            namespaces
                .into_iter()
                .map(|namespace| namespace_document(&namespace))
                .collect::<Vec<_>>(),
            &auth,
        ),
        Err(error) => namespace_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/namespaces",
    security(
        ("session_cookie" = [], "csrf_cookie" = [], "csrf_header" = []),
        ("bearer_token" = [])
    ),
    request_body = ClaimNamespaceRequest,
    responses(
        (status = 201, body = DataEnvelope<NamespaceDocument>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 409, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn claim_namespace(
    State(state): State<NamespaceState>,
    headers: HeaderMap,
    payload: Result<Json<ClaimNamespaceRequest>, JsonRejection>,
) -> Response {
    let actor = match authenticate_mutation(&state, &headers).await {
        Ok(actor) => actor,
        Err(problem) => return problem.into_response(),
    };
    let Ok(Json(payload)) = payload else {
        return invalid_json("namespace claim").into_response();
    };
    match state.namespaces.claim(actor, &payload.name).await {
        Ok(namespace) => (
            StatusCode::CREATED,
            Json(DataEnvelope::new(namespace_document(&namespace))),
        )
            .into_response(),
        Err(error) => namespace_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/namespaces/{namespace}/members",
    params(("namespace" = String, Path)),
    security(("session_cookie" = []), ("bearer_token" = [])),
    responses(
        (status = 200, body = DataEnvelope<Vec<NamespaceMemberDocument>>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 404, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn list_members(
    State(state): State<NamespaceState>,
    headers: HeaderMap,
    Path(namespace): Path<String>,
) -> Response {
    let auth = match authenticate_read(&state, &headers).await {
        Ok(auth) => auth,
        Err(problem) => return problem.into_response(),
    };
    match state
        .namespaces
        .members(auth.actor.clone(), &namespace)
        .await
    {
        Ok(members) => read_json(
            members.into_iter().map(member_document).collect::<Vec<_>>(),
            &auth,
        ),
        Err(error) => namespace_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    patch,
    path = "/namespaces/{namespace}/members/{github_login}",
    params(("namespace" = String, Path), ("github_login" = String, Path)),
    security(
        ("session_cookie" = [], "csrf_cookie" = [], "csrf_header" = []),
        ("bearer_token" = [])
    ),
    request_body = UpdateNamespaceMemberRequest,
    responses(
        (status = 200, body = DataEnvelope<NamespaceMemberDocument>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 404, response = ProblemResponse),
        (status = 409, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn set_member_role(
    State(state): State<NamespaceState>,
    headers: HeaderMap,
    Path((namespace, github_login)): Path<(String, String)>,
    payload: Result<Json<UpdateNamespaceMemberRequest>, JsonRejection>,
) -> Response {
    let actor = match authenticate_mutation(&state, &headers).await {
        Ok(actor) => actor,
        Err(problem) => return problem.into_response(),
    };
    let Ok(Json(payload)) = payload else {
        return invalid_json("membership update").into_response();
    };
    match state
        .namespaces
        .set_member_role(actor, &namespace, &github_login, payload.role.into())
        .await
    {
        Ok(member) => Json(DataEnvelope::new(member_document(member))).into_response(),
        Err(error) => namespace_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/namespaces/{namespace}/members/{github_login}",
    params(("namespace" = String, Path), ("github_login" = String, Path)),
    security(
        ("session_cookie" = [], "csrf_cookie" = [], "csrf_header" = []),
        ("bearer_token" = [])
    ),
    responses(
        (status = 204),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 404, response = ProblemResponse),
        (status = 409, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn remove_member(
    State(state): State<NamespaceState>,
    headers: HeaderMap,
    Path((namespace, github_login)): Path<(String, String)>,
) -> Response {
    let actor = match authenticate_mutation(&state, &headers).await {
        Ok(actor) => actor,
        Err(problem) => return problem.into_response(),
    };
    match state
        .namespaces
        .remove_member(actor, &namespace, &github_login)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => namespace_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/namespaces/{namespace}/invitations",
    params(("namespace" = String, Path)),
    security(("session_cookie" = []), ("bearer_token" = [])),
    responses(
        (status = 200, body = DataEnvelope<Vec<NamespaceInvitationDocument>>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 404, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn list_namespace_invitations(
    State(state): State<NamespaceState>,
    headers: HeaderMap,
    Path(namespace): Path<String>,
) -> Response {
    let auth = match authenticate_read(&state, &headers).await {
        Ok(auth) => auth,
        Err(problem) => return problem.into_response(),
    };
    match state
        .namespaces
        .invitations(auth.actor.clone(), &namespace)
        .await
    {
        Ok(invitations) => read_json(
            invitations
                .into_iter()
                .map(invitation_document)
                .collect::<Vec<_>>(),
            &auth,
        ),
        Err(error) => namespace_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/namespaces/{namespace}/invitations",
    params(("namespace" = String, Path)),
    security(
        ("session_cookie" = [], "csrf_cookie" = [], "csrf_header" = []),
        ("bearer_token" = [])
    ),
    request_body = InviteNamespaceMemberRequest,
    responses(
        (status = 201, body = DataEnvelope<NamespaceInvitationDocument>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 404, response = ProblemResponse),
        (status = 409, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn invite_member(
    State(state): State<NamespaceState>,
    headers: HeaderMap,
    Path(namespace): Path<String>,
    payload: Result<Json<InviteNamespaceMemberRequest>, JsonRejection>,
) -> Response {
    let actor = match authenticate_mutation(&state, &headers).await {
        Ok(actor) => actor,
        Err(problem) => return problem.into_response(),
    };
    let Ok(Json(payload)) = payload else {
        return invalid_json("namespace invitation").into_response();
    };
    match state
        .namespaces
        .invite(
            actor,
            &namespace,
            &payload.github_login,
            payload.role.into(),
        )
        .await
    {
        Ok(invitation) => (
            StatusCode::CREATED,
            Json(DataEnvelope::new(invitation_document(invitation))),
        )
            .into_response(),
        Err(error) => namespace_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/namespaces/{namespace}/invitations/{github_login}",
    params(("namespace" = String, Path), ("github_login" = String, Path)),
    security(
        ("session_cookie" = [], "csrf_cookie" = [], "csrf_header" = []),
        ("bearer_token" = [])
    ),
    responses(
        (status = 204),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 404, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn revoke_invitation(
    State(state): State<NamespaceState>,
    headers: HeaderMap,
    Path((namespace, github_login)): Path<(String, String)>,
) -> Response {
    let actor = match authenticate_mutation(&state, &headers).await {
        Ok(actor) => actor,
        Err(problem) => return problem.into_response(),
    };
    match state
        .namespaces
        .revoke_invitation(actor, &namespace, &github_login)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => namespace_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    get,
    path = "/invitations",
    security(("session_cookie" = []), ("bearer_token" = [])),
    responses(
        (status = 200, body = DataEnvelope<Vec<NamespaceInvitationDocument>>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn list_my_invitations(
    State(state): State<NamespaceState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authenticate_read(&state, &headers).await {
        Ok(auth) => auth,
        Err(problem) => return problem.into_response(),
    };
    match state.namespaces.my_invitations(auth.actor.clone()).await {
        Ok(invitations) => read_json(
            invitations
                .into_iter()
                .map(invitation_document)
                .collect::<Vec<_>>(),
            &auth,
        ),
        Err(error) => namespace_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/invitations/{namespace}/accept",
    params(("namespace" = String, Path)),
    security(
        ("session_cookie" = [], "csrf_cookie" = [], "csrf_header" = []),
        ("bearer_token" = [])
    ),
    responses(
        (status = 200, body = DataEnvelope<NamespaceMemberDocument>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 404, response = ProblemResponse),
        (status = 410, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn accept_invitation(
    State(state): State<NamespaceState>,
    headers: HeaderMap,
    Path(namespace): Path<String>,
) -> Response {
    let actor = match authenticate_mutation(&state, &headers).await {
        Ok(actor) => actor,
        Err(problem) => return problem.into_response(),
    };
    match state.namespaces.accept_invitation(actor, &namespace).await {
        Ok(member) => Json(DataEnvelope::new(member_document(member))).into_response(),
        Err(error) => namespace_problem(error.kind()).into_response(),
    }
}

#[derive(Clone)]
struct ReadAuthentication {
    actor: NamespaceActor,
    session: Option<Arc<AuthenticatedSession>>,
}

async fn authenticate_read(
    state: &NamespaceState,
    headers: &HeaderMap,
) -> Result<ReadAuthentication, Problem> {
    if let Some(actor) = bearer_actor(headers) {
        return Ok(ReadAuthentication {
            actor,
            session: None,
        });
    }
    if !origin_matches(headers, &state.allowed_web_origin) {
        return Err(csrf_invalid());
    }
    let session = state
        .authentication
        .session(
            &cookie_value(headers, SESSION_COOKIE).unwrap_or_default(),
            cookie_value(headers, CSRF_COOKIE).as_deref(),
        )
        .await
        .map_err(|error| authentication_problem(error.kind()))?;
    let actor = NamespaceActor::Session(session.user.id);
    Ok(ReadAuthentication {
        actor,
        session: Some(Arc::new(session)),
    })
}

async fn authenticate_mutation(
    state: &NamespaceState,
    headers: &HeaderMap,
) -> Result<NamespaceActor, Problem> {
    if let Some(actor) = bearer_actor(headers) {
        return Ok(actor);
    }
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
        .map(|authenticated| NamespaceActor::Session(authenticated.user.id))
        .map_err(|error| authentication_problem(error.kind()))
}

fn bearer_actor(headers: &HeaderMap) -> Option<NamespaceActor> {
    let value = headers.get(AUTHORIZATION)?;
    let credential = value
        .to_str()
        .ok()
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|credential| {
            !credential.is_empty() && !credential.bytes().any(|byte| byte.is_ascii_whitespace())
        })
        .unwrap_or_default();
    Some(NamespaceActor::Bearer(credential.to_owned()))
}

fn read_json<T: Serialize>(data: T, authentication: &ReadAuthentication) -> Response {
    let mut response = Json(DataEnvelope::new(data)).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(session) = &authentication.session {
        append_rotated_session_cookies(&mut response, session);
    }
    response
}

fn namespace_document(namespace: &NamespaceSummary) -> NamespaceDocument {
    NamespaceDocument {
        name: namespace.name.to_string(),
        role: namespace.role.into(),
        created_at: timestamp(namespace.created_at),
        updated_at: timestamp(namespace.updated_at),
    }
}

fn user_document(user: NamespaceUser) -> NamespaceUserDocument {
    NamespaceUserDocument {
        github_login: user.github_login,
        display_name: user.display_name,
        avatar_url: user.avatar_url,
    }
}

fn member_document(member: NamespaceMember) -> NamespaceMemberDocument {
    NamespaceMemberDocument {
        user: user_document(member.user),
        role: member.role.into(),
        created_at: timestamp(member.created_at),
    }
}

fn invitation_document(invitation: NamespaceInvitation) -> NamespaceInvitationDocument {
    NamespaceInvitationDocument {
        namespace: invitation.namespace.to_string(),
        invited_user: user_document(invitation.invited_user),
        invited_by: invitation.invited_by.map(user_document),
        role: invitation.role.into(),
        created_at: timestamp(invitation.created_at),
        expires_at: timestamp(invitation.expires_at),
    }
}

fn timestamp(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .expect("UTC namespace timestamps should format as RFC 3339")
}

fn csrf_invalid() -> Problem {
    Problem::new(
        StatusCode::FORBIDDEN,
        "csrf_invalid",
        "The CSRF token is invalid",
    )
}

fn authentication_required() -> Problem {
    Problem::new(
        StatusCode::UNAUTHORIZED,
        "authentication_required",
        "Authentication is required",
    )
}

fn invalid_json(operation: &str) -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
        "The request is invalid",
    )
    .with_detail(format!(
        "The request body must match the {operation} schema."
    ))
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

fn namespace_problem(kind: NamespaceErrorKind) -> Problem {
    match kind {
        NamespaceErrorKind::InvalidNamespace => invalid_field(
            "invalid_namespace",
            "must satisfy the registry identity-segment syntax",
            "/name",
        ),
        NamespaceErrorKind::InvalidGitHubLogin => invalid_field(
            "invalid_github_login",
            "must be a valid GitHub login",
            "/github_login",
        ),
        NamespaceErrorKind::AuthenticationRequired => authentication_required(),
        NamespaceErrorKind::InsufficientScope => Problem::new(
            StatusCode::FORBIDDEN,
            "insufficient_scope",
            "The API token does not grant the required scope",
        ),
        NamespaceErrorKind::Forbidden => Problem::new(
            StatusCode::FORBIDDEN,
            "namespace_forbidden",
            "The caller cannot perform this namespace operation",
        ),
        NamespaceErrorKind::NamespaceNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "namespace_not_found",
            "The namespace was not found",
        ),
        NamespaceErrorKind::UserNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "user_not_found",
            "The registry user was not found",
        ),
        NamespaceErrorKind::MemberNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "namespace_member_not_found",
            "The namespace member was not found",
        ),
        NamespaceErrorKind::InvitationNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "invitation_not_found",
            "An actionable invitation was not found",
        ),
        NamespaceErrorKind::InvitationExpired => Problem::new(
            StatusCode::GONE,
            "invitation_expired",
            "The namespace invitation has expired",
        ),
        NamespaceErrorKind::NamespaceConflict => Problem::new(
            StatusCode::CONFLICT,
            "namespace_conflict",
            "The namespace identity is already claimed",
        ),
        NamespaceErrorKind::MemberExists => Problem::new(
            StatusCode::CONFLICT,
            "namespace_member_exists",
            "The user is already a namespace member",
        ),
        NamespaceErrorKind::PendingInvitation => Problem::new(
            StatusCode::CONFLICT,
            "invitation_pending",
            "An actionable invitation already exists",
        ),
        NamespaceErrorKind::CannotInviteSelf => Problem::new(
            StatusCode::CONFLICT,
            "cannot_invite_self",
            "A namespace owner cannot invite themselves",
        ),
        NamespaceErrorKind::LastOwner => Problem::new(
            StatusCode::CONFLICT,
            "last_owner_required",
            "Every namespace must retain at least one owner",
        ),
        NamespaceErrorKind::Unavailable => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "namespace_service_unavailable",
            "Namespace management is temporarily unavailable",
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
        AuthenticationError, AuthenticationErrorKind, CompletedLogin, LoginStart, NamespaceError,
        UserId, UserRecord,
    };
    use tower::ServiceExt;

    use super::*;

    const WEB_ORIGIN: &str = "https://rux-lang.dev";

    #[derive(Default)]
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
            Ok(authenticated_session())
        }

        async fn authenticate_mutation(
            &self,
            session_credential: &str,
            csrf_cookie: Option<&str>,
            csrf_header: Option<&str>,
        ) -> Result<AuthenticatedSession, AuthenticationError> {
            if session_credential == "session"
                && csrf_cookie == Some("csrf")
                && csrf_header == Some("csrf")
            {
                Ok(authenticated_session())
            } else {
                Err(AuthenticationError::new(
                    AuthenticationErrorKind::InvalidCsrf,
                ))
            }
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
    struct FakeNamespaces {
        actors: Mutex<Vec<NamespaceActor>>,
    }

    impl FakeNamespaces {
        fn record(&self, actor: NamespaceActor) {
            self.actors
                .lock()
                .expect("actors should not be poisoned")
                .push(actor);
        }
    }

    #[async_trait]
    impl Namespaces for FakeNamespaces {
        async fn list(
            &self,
            actor: NamespaceActor,
        ) -> Result<Vec<NamespaceSummary>, NamespaceError> {
            self.record(actor);
            Ok(vec![])
        }

        async fn claim(
            &self,
            actor: NamespaceActor,
            _name: &str,
        ) -> Result<NamespaceSummary, NamespaceError> {
            self.record(actor);
            unreachable!()
        }

        async fn members(
            &self,
            actor: NamespaceActor,
            _namespace: &str,
        ) -> Result<Vec<NamespaceMember>, NamespaceError> {
            self.record(actor);
            Ok(vec![])
        }

        async fn set_member_role(
            &self,
            actor: NamespaceActor,
            _namespace: &str,
            _github_login: &str,
            _role: NamespaceRole,
        ) -> Result<NamespaceMember, NamespaceError> {
            self.record(actor);
            Err(NamespaceError::new(NamespaceErrorKind::LastOwner))
        }

        async fn remove_member(
            &self,
            actor: NamespaceActor,
            _namespace: &str,
            _github_login: &str,
        ) -> Result<(), NamespaceError> {
            self.record(actor);
            Ok(())
        }

        async fn invitations(
            &self,
            actor: NamespaceActor,
            _namespace: &str,
        ) -> Result<Vec<NamespaceInvitation>, NamespaceError> {
            self.record(actor);
            Ok(vec![])
        }

        async fn invite(
            &self,
            actor: NamespaceActor,
            _namespace: &str,
            _github_login: &str,
            _role: NamespaceRole,
        ) -> Result<NamespaceInvitation, NamespaceError> {
            self.record(actor);
            unreachable!()
        }

        async fn my_invitations(
            &self,
            actor: NamespaceActor,
        ) -> Result<Vec<NamespaceInvitation>, NamespaceError> {
            self.record(actor);
            Ok(vec![])
        }

        async fn accept_invitation(
            &self,
            actor: NamespaceActor,
            _namespace: &str,
        ) -> Result<NamespaceMember, NamespaceError> {
            self.record(actor);
            unreachable!()
        }

        async fn revoke_invitation(
            &self,
            actor: NamespaceActor,
            _namespace: &str,
            _github_login: &str,
        ) -> Result<(), NamespaceError> {
            self.record(actor);
            Ok(())
        }
    }

    #[tokio::test]
    async fn bearer_authentication_takes_precedence_without_browser_csrf() {
        let namespaces = Arc::new(FakeNamespaces::default());
        let response = router(
            Arc::new(FakeAuthentication),
            namespaces.clone(),
            WEB_ORIGIN.into(),
        )
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/v1/namespaces")
                .header(AUTHORIZATION, "Bearer rux_pat_credential")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            namespaces
                .actors
                .lock()
                .expect("actors should not be poisoned")
                .as_slice(),
            &[NamespaceActor::Bearer("rux_pat_credential".into())]
        );
    }

    #[tokio::test]
    async fn browser_mutations_require_origin_and_matching_csrf() {
        let namespaces = Arc::new(FakeNamespaces::default());
        let response = router(
            Arc::new(FakeAuthentication),
            namespaces.clone(),
            WEB_ORIGIN.into(),
        )
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/namespaces")
                .header(ORIGIN, WEB_ORIGIN)
                .header(COOKIE, "__Host-rux_session=session; __Host-rux_csrf=csrf")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"Rux_Tools"}"#))
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(
            namespaces
                .actors
                .lock()
                .expect("actors should not be poisoned")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn last_owner_errors_use_the_stable_conflict_problem() {
        let response = router(
            Arc::new(FakeAuthentication),
            Arc::new(FakeNamespaces::default()),
            WEB_ORIGIN.into(),
        )
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/v1/namespaces/rux/members/owner")
                .header(AUTHORIZATION, "Bearer rux_pat_credential")
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"role":"maintainer"}"#))
                .expect("request should build"),
        )
        .await
        .expect("router should respond");
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    fn authenticated_session() -> AuthenticatedSession {
        AuthenticatedSession {
            user: UserRecord {
                id: UserId::new(Uuid::from_u128(1)),
                github_user_id: Some(1),
                github_login: Some("owner".into()),
                display_name: None,
                avatar_url: None,
                created_at: OffsetDateTime::UNIX_EPOCH,
                updated_at: OffsetDateTime::UNIX_EPOCH,
                anonymized_at: None,
            },
            csrf_credential: "csrf".into(),
            session_credential: None,
            expires_at: OffsetDateTime::UNIX_EPOCH + time::Duration::days(30),
            remaining_lifetime_seconds: 60,
        }
    }
}
