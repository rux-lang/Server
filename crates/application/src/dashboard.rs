use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rux_domain::{IdentitySegment, SemanticVersion};
use time::{Duration, OffsetDateTime};

use crate::{Clock, NamespaceRole, RepositoryError, UserId};

pub const DASHBOARD_PACKAGE_LIMIT: u16 = 10;
pub const DASHBOARD_ACTIVITY_LIMIT: u16 = 10;
pub const DASHBOARD_DOWNLOAD_LEADER_LIMIT: u16 = 5;
pub const DASHBOARD_DOWNLOAD_WINDOW_DAYS: i64 = 30;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardUser {
    pub github_login: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardNamespace {
    pub namespace: IdentitySegment,
    pub role: NamespaceRole,
    pub package_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardPackage {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub version: SemanticVersion,
    pub published_at: OffsetDateTime,
    pub yanked: bool,
    pub version_count: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardInvitation {
    pub namespace: IdentitySegment,
    pub invited_by: Option<DashboardUser>,
    pub role: NamespaceRole,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardActivityKind {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardActivity {
    pub kind: DashboardActivityKind,
    pub actor: Option<DashboardUser>,
    pub namespace: IdentitySegment,
    pub package: Option<IdentitySegment>,
    pub version: Option<SemanticVersion>,
    pub target_user: Option<DashboardUser>,
    pub previous_role: Option<NamespaceRole>,
    pub role: Option<NamespaceRole>,
    pub occurred_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardDownloadLeader {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub downloads_30d: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardDownloads {
    pub total_30d: u64,
    pub total_all_time: u64,
    pub top_packages: Vec<DashboardDownloadLeader>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardSnapshot {
    pub namespace_count: u64,
    pub package_count: u64,
    pub invitation_count: u64,
    pub namespaces: Vec<DashboardNamespace>,
    pub packages: Vec<DashboardPackage>,
    pub invitations: Vec<DashboardInvitation>,
    pub activity: Vec<DashboardActivity>,
    pub downloads: DashboardDownloads,
}

#[async_trait]
pub trait DashboardReader: Send + Sync {
    async fn dashboard_snapshot(
        &self,
        user_id: UserId,
        window_start: OffsetDateTime,
        window_end: OffsetDateTime,
        package_limit: u16,
        activity_limit: u16,
        download_leader_limit: u16,
    ) -> Result<DashboardSnapshot, RepositoryError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DashboardErrorKind {
    Unavailable,
}

#[derive(Debug)]
pub struct DashboardError {
    kind: DashboardErrorKind,
}

impl DashboardError {
    #[must_use]
    pub const fn new(kind: DashboardErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> DashboardErrorKind {
        self.kind
    }
}

impl fmt::Display for DashboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "dashboard read failed: {:?}", self.kind)
    }
}

impl Error for DashboardError {}

#[async_trait]
pub trait Dashboards: Send + Sync {
    async fn get(&self, user_id: UserId) -> Result<DashboardSnapshot, DashboardError>;
}

pub struct DashboardService {
    reader: Arc<dyn DashboardReader>,
    clock: Arc<dyn Clock>,
}

impl DashboardService {
    #[must_use]
    pub fn new(reader: Arc<dyn DashboardReader>, clock: Arc<dyn Clock>) -> Self {
        Self { reader, clock }
    }
}

#[async_trait]
impl Dashboards for DashboardService {
    async fn get(&self, user_id: UserId) -> Result<DashboardSnapshot, DashboardError> {
        let window_end = self.clock.now();
        let window_start = window_end - Duration::days(DASHBOARD_DOWNLOAD_WINDOW_DAYS);
        self.reader
            .dashboard_snapshot(
                user_id,
                window_start,
                window_end,
                DASHBOARD_PACKAGE_LIMIT,
                DASHBOARD_ACTIVITY_LIMIT,
                DASHBOARD_DOWNLOAD_LEADER_LIMIT,
            )
            .await
            .map_err(|_| DashboardError::new(DashboardErrorKind::Unavailable))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use uuid::Uuid;

    use super::*;

    struct FixedClock(OffsetDateTime);

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            self.0
        }
    }

    type ReadArguments = (UserId, OffsetDateTime, OffsetDateTime, u16, u16, u16);

    struct StubReader {
        fails: bool,
        arguments: Mutex<Option<ReadArguments>>,
    }

    #[async_trait]
    impl DashboardReader for StubReader {
        async fn dashboard_snapshot(
            &self,
            user_id: UserId,
            window_start: OffsetDateTime,
            window_end: OffsetDateTime,
            package_limit: u16,
            activity_limit: u16,
            download_leader_limit: u16,
        ) -> Result<DashboardSnapshot, RepositoryError> {
            *self.arguments.lock().expect("arguments lock should work") = Some((
                user_id,
                window_start,
                window_end,
                package_limit,
                activity_limit,
                download_leader_limit,
            ));
            if self.fails {
                Err(RepositoryError::new(
                    crate::RepositoryErrorKind::Unavailable,
                ))
            } else {
                Ok(empty_snapshot())
            }
        }
    }

    #[tokio::test]
    async fn requests_the_fixed_bounded_owner_snapshot() {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        let reader = Arc::new(StubReader {
            fails: false,
            arguments: Mutex::new(None),
        });
        let service = DashboardService::new(reader.clone(), Arc::new(FixedClock(now)));

        assert_eq!(
            service.get(UserId::new(Uuid::from_u128(7))).await.unwrap(),
            empty_snapshot()
        );
        assert_eq!(
            *reader.arguments.lock().unwrap(),
            Some((
                UserId::new(Uuid::from_u128(7)),
                now - Duration::days(30),
                now,
                10,
                10,
                5,
            ))
        );
    }

    #[tokio::test]
    async fn hides_repository_failures() {
        let reader = Arc::new(StubReader {
            fails: true,
            arguments: Mutex::new(None),
        });
        let service =
            DashboardService::new(reader, Arc::new(FixedClock(OffsetDateTime::UNIX_EPOCH)));

        assert_eq!(
            service
                .get(UserId::new(Uuid::from_u128(7)))
                .await
                .unwrap_err()
                .kind(),
            DashboardErrorKind::Unavailable
        );
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
