use std::sync::Arc;

use axum::extract::State;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rux_application::{
    Authentication, DashboardActivity, DashboardActivityKind, DashboardErrorKind,
    DashboardInvitation, DashboardNamespace, DashboardPackage, DashboardSnapshot, DashboardUser,
    Dashboards,
};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use utoipa::ToSchema;

use crate::auth::{
    CSRF_COOKIE, SESSION_COOKIE, append_rotated_session_cookies, authentication_problem,
    cookie_value, origin_matches,
};
use crate::contract::{DataEnvelope, Problem, ProblemResponse};
use crate::namespaces::NamespaceRoleDocument;
use crate::paths::{canonical_package_path, canonical_version_path};

#[derive(Clone)]
pub(crate) struct DashboardState {
    authentication: Arc<dyn Authentication>,
    dashboards: Arc<dyn Dashboards>,
    allowed_web_origin: String,
}

pub fn router(
    authentication: Arc<dyn Authentication>,
    dashboards: Arc<dyn Dashboards>,
    allowed_web_origin: String,
) -> Router {
    Router::new()
        .route("/v1/dashboard", get(dashboard))
        .with_state(DashboardState {
            authentication,
            dashboards,
            allowed_web_origin,
        })
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DashboardCountsDocument {
    namespaces: u64,
    packages: u64,
    invitations: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DashboardUserDocument {
    github_login: String,
    display_name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DashboardNamespaceDocument {
    namespace: String,
    role: NamespaceRoleDocument,
    package_count: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DashboardPackageDocument {
    namespace: String,
    package: String,
    version: String,
    #[schema(format = "date-time")]
    published_at: String,
    yanked: bool,
    version_count: u64,
    package_url: String,
    version_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DashboardInvitationDocument {
    namespace: String,
    invited_by: Option<DashboardUserDocument>,
    role: NamespaceRoleDocument,
    #[schema(format = "date-time")]
    created_at: String,
    #[schema(format = "date-time")]
    expires_at: String,
}

#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DashboardActivityKindDocument {
    NamespaceCreated,
    NamespaceMemberRoleChanged,
    NamespaceMemberRemoved,
    NamespaceInvitationCreated,
    NamespaceInvitationAccepted,
    NamespaceInvitationRevoked,
    PackageVersionPublished,
    PackageVersionYanked,
    PackageVersionUnyanked,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DashboardActivityDocument {
    kind: DashboardActivityKindDocument,
    actor: Option<DashboardUserDocument>,
    namespace: String,
    package: Option<String>,
    version: Option<String>,
    target_user: Option<DashboardUserDocument>,
    previous_role: Option<NamespaceRoleDocument>,
    role: Option<NamespaceRoleDocument>,
    #[schema(format = "date-time")]
    occurred_at: String,
    package_url: Option<String>,
    version_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DashboardDownloadLeaderDocument {
    namespace: String,
    package: String,
    downloads_30d: u64,
    package_url: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DashboardDownloadsDocument {
    window_days: u8,
    total_30d: u64,
    total_all_time: u64,
    top_packages: Vec<DashboardDownloadLeaderDocument>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct DashboardDocument {
    counts: DashboardCountsDocument,
    namespaces: Vec<DashboardNamespaceDocument>,
    packages: Vec<DashboardPackageDocument>,
    invitations: Vec<DashboardInvitationDocument>,
    activity: Vec<DashboardActivityDocument>,
    downloads: DashboardDownloadsDocument,
}

#[utoipa::path(
    get,
    path = "/dashboard",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = DataEnvelope<DashboardDocument>),
        (status = 401, response = ProblemResponse),
        (status = 403, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn dashboard(State(state): State<DashboardState>, headers: HeaderMap) -> Response {
    if !origin_matches(&headers, &state.allowed_web_origin) {
        return Problem::new(
            StatusCode::FORBIDDEN,
            "csrf_invalid",
            "The CSRF token is invalid",
        )
        .into_response();
    }
    let session = match state
        .authentication
        .session(
            &cookie_value(&headers, SESSION_COOKIE).unwrap_or_default(),
            cookie_value(&headers, CSRF_COOKIE).as_deref(),
        )
        .await
    {
        Ok(session) => session,
        Err(error) => return authentication_problem(error.kind()).into_response(),
    };
    match state.dashboards.get(session.user.id).await {
        Ok(snapshot) => {
            let mut response =
                Json(DataEnvelope::new(dashboard_document(snapshot))).into_response();
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            append_rotated_session_cookies(&mut response, &session);
            response
        }
        Err(error) => dashboard_problem(error.kind()).into_response(),
    }
}

fn dashboard_document(snapshot: DashboardSnapshot) -> DashboardDocument {
    DashboardDocument {
        counts: DashboardCountsDocument {
            namespaces: snapshot.namespace_count,
            packages: snapshot.package_count,
            invitations: snapshot.invitation_count,
        },
        namespaces: snapshot.namespaces.iter().map(namespace_document).collect(),
        packages: snapshot.packages.iter().map(package_document).collect(),
        invitations: snapshot
            .invitations
            .into_iter()
            .map(invitation_document)
            .collect(),
        activity: snapshot
            .activity
            .into_iter()
            .map(activity_document)
            .collect(),
        downloads: DashboardDownloadsDocument {
            window_days: 30,
            total_30d: snapshot.downloads.total_30d,
            total_all_time: snapshot.downloads.total_all_time,
            top_packages: snapshot
                .downloads
                .top_packages
                .into_iter()
                .map(|leader| {
                    let namespace = leader.namespace.to_string();
                    let package = leader.package.to_string();
                    DashboardDownloadLeaderDocument {
                        package_url: canonical_package_path(
                            leader.namespace.normalized(),
                            leader.package.normalized(),
                        ),
                        namespace,
                        package,
                        downloads_30d: leader.downloads_30d,
                    }
                })
                .collect(),
        },
    }
}

fn namespace_document(namespace: &DashboardNamespace) -> DashboardNamespaceDocument {
    DashboardNamespaceDocument {
        namespace: namespace.namespace.to_string(),
        role: namespace.role.into(),
        package_count: namespace.package_count,
    }
}

fn package_document(package: &DashboardPackage) -> DashboardPackageDocument {
    let namespace_name = package.namespace.to_string();
    let package_name = package.package.to_string();
    let version = package.version.to_string();
    DashboardPackageDocument {
        package_url: canonical_package_path(
            package.namespace.normalized(),
            package.package.normalized(),
        ),
        version_url: canonical_version_path(
            package.namespace.normalized(),
            package.package.normalized(),
            package.version.as_str(),
        ),
        namespace: namespace_name,
        package: package_name,
        version,
        published_at: timestamp(package.published_at),
        yanked: package.yanked,
        version_count: package.version_count,
    }
}

fn invitation_document(invitation: DashboardInvitation) -> DashboardInvitationDocument {
    DashboardInvitationDocument {
        namespace: invitation.namespace.to_string(),
        invited_by: invitation.invited_by.map(user_document),
        role: invitation.role.into(),
        created_at: timestamp(invitation.created_at),
        expires_at: timestamp(invitation.expires_at),
    }
}

fn activity_document(activity: DashboardActivity) -> DashboardActivityDocument {
    let package_url = activity.package.as_ref().map(|package| {
        canonical_package_path(activity.namespace.normalized(), package.normalized())
    });
    let version_url = activity
        .package
        .as_ref()
        .zip(activity.version.as_ref())
        .map(|(package, version)| {
            canonical_version_path(
                activity.namespace.normalized(),
                package.normalized(),
                version.as_str(),
            )
        });
    DashboardActivityDocument {
        kind: activity.kind.into(),
        actor: activity.actor.map(user_document),
        namespace: activity.namespace.to_string(),
        package: activity.package.map(|value| value.to_string()),
        version: activity.version.map(|value| value.to_string()),
        target_user: activity.target_user.map(user_document),
        previous_role: activity.previous_role.map(Into::into),
        role: activity.role.map(Into::into),
        occurred_at: timestamp(activity.occurred_at),
        package_url,
        version_url,
    }
}

fn user_document(user: DashboardUser) -> DashboardUserDocument {
    DashboardUserDocument {
        github_login: user.github_login,
        display_name: user.display_name,
        avatar_url: user.avatar_url,
    }
}

impl From<DashboardActivityKind> for DashboardActivityKindDocument {
    fn from(value: DashboardActivityKind) -> Self {
        match value {
            DashboardActivityKind::NamespaceCreated => Self::NamespaceCreated,
            DashboardActivityKind::NamespaceMemberRoleChanged => Self::NamespaceMemberRoleChanged,
            DashboardActivityKind::NamespaceMemberRemoved => Self::NamespaceMemberRemoved,
            DashboardActivityKind::NamespaceInvitationCreated => Self::NamespaceInvitationCreated,
            DashboardActivityKind::NamespaceInvitationAccepted => Self::NamespaceInvitationAccepted,
            DashboardActivityKind::NamespaceInvitationRevoked => Self::NamespaceInvitationRevoked,
            DashboardActivityKind::PackageVersionPublished => Self::PackageVersionPublished,
            DashboardActivityKind::PackageVersionYanked => Self::PackageVersionYanked,
            DashboardActivityKind::PackageVersionUnyanked => Self::PackageVersionUnyanked,
        }
    }
}

fn timestamp(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .expect("UTC dashboard timestamps should format as RFC 3339")
}

fn dashboard_problem(kind: DashboardErrorKind) -> Problem {
    match kind {
        DashboardErrorKind::Unavailable => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "dashboard_unavailable",
            "The owner dashboard is unavailable",
        ),
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::Request;
    use axum::http::header::{COOKIE, ORIGIN};
    use rux_application::{
        AuthenticatedSession, AuthenticationError, AuthenticationErrorKind, CompletedLogin,
        DashboardDownloads, DashboardError, LoginStart, UserId, UserRecord,
    };
    use serde_json::Value;
    use tower::ServiceExt;
    use uuid::Uuid;

    use super::*;

    const WEB_ORIGIN: &str = "https://rux-lang.dev";

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
            session_credential: &str,
            _csrf_credential: Option<&str>,
        ) -> Result<AuthenticatedSession, AuthenticationError> {
            if session_credential != "session" {
                return Err(AuthenticationError::new(
                    AuthenticationErrorKind::AuthenticationRequired,
                ));
            }
            Ok(authenticated_session())
        }

        async fn authenticate_mutation(
            &self,
            _session_credential: &str,
            _csrf_cookie: Option<&str>,
            _csrf_header: Option<&str>,
        ) -> Result<AuthenticatedSession, AuthenticationError> {
            unreachable!()
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

    struct FakeDashboards {
        fails: bool,
    }

    #[async_trait]
    impl Dashboards for FakeDashboards {
        async fn get(&self, _user_id: UserId) -> Result<DashboardSnapshot, DashboardError> {
            if self.fails {
                Err(DashboardError::new(DashboardErrorKind::Unavailable))
            } else {
                Ok(empty_snapshot())
            }
        }
    }

    #[tokio::test]
    async fn returns_the_non_cacheable_session_dashboard_contract() {
        let response = test_router(false)
            .oneshot(browser_request().body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "data": {
                    "counts": { "namespaces": 0, "packages": 0, "invitations": 0 },
                    "namespaces": [],
                    "packages": [],
                    "invitations": [],
                    "activity": [],
                    "downloads": {
                        "window_days": 30,
                        "total_30d": 0,
                        "total_all_time": 0,
                        "top_packages": []
                    }
                }
            })
        );
    }

    #[tokio::test]
    async fn rejects_missing_origin_or_session_and_maps_read_failures() {
        let no_origin = test_router(false)
            .oneshot(
                Request::builder()
                    .uri("/v1/dashboard")
                    .header(COOKIE, format!("{SESSION_COOKIE}=session"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(no_origin.status(), StatusCode::FORBIDDEN);

        let no_session = test_router(false)
            .oneshot(
                Request::builder()
                    .uri("/v1/dashboard")
                    .header(ORIGIN, WEB_ORIGIN)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(no_session.status(), StatusCode::UNAUTHORIZED);

        let unavailable = test_router(true)
            .oneshot(browser_request().body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    fn test_router(fails: bool) -> Router {
        router(
            Arc::new(FakeAuthentication),
            Arc::new(FakeDashboards { fails }),
            WEB_ORIGIN.into(),
        )
    }

    fn browser_request() -> axum::http::request::Builder {
        Request::builder()
            .uri("/v1/dashboard")
            .header(ORIGIN, WEB_ORIGIN)
            .header(
                COOKIE,
                format!("{SESSION_COOKIE}=session; {CSRF_COOKIE}=csrf"),
            )
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

    fn empty_snapshot() -> DashboardSnapshot {
        DashboardSnapshot {
            namespace_count: 0,
            package_count: 0,
            invitation_count: 0,
            namespaces: Vec::new(),
            packages: Vec::new(),
            invitations: Vec::new(),
            activity: Vec::new(),
            downloads: DashboardDownloads {
                total_30d: 0,
                total_all_time: 0,
                top_packages: Vec::new(),
            },
        }
    }
}
