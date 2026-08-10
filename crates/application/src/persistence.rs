use std::error::Error;
use std::fmt;

use async_trait::async_trait;
use rux_domain::{IdentitySegment, SemanticVersion, TargetOs, VersionRange};
use serde_json::{Map, Value};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Uuid);

        impl $name {
            #[must_use]
            pub const fn new(value: Uuid) -> Self {
                Self(value)
            }

            #[must_use]
            pub const fn get(self) -> Uuid {
                self.0
            }
        }
    };
}

identifier!(UserId);
identifier!(SessionId);
identifier!(NamespaceId);
identifier!(InvitationId);
identifier!(PackageId);
identifier!(PackageVersionId);
identifier!(ApiTokenId);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SecretHash([u8; 32]);

impl SecretHash {
    #[must_use]
    pub const fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ArtifactSha256([u8; 32]);

impl ArtifactSha256 {
    #[must_use]
    pub const fn new(value: [u8; 32]) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub type JsonObject = Map<String, Value>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitHubUserProfile {
    pub github_user_id: u64,
    pub github_login: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserRecord {
    pub id: UserId,
    pub github_user_id: Option<u64>,
    pub github_login: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub anonymized_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewSession {
    pub user_id: UserId,
    pub secret_hash: SecretHash,
    pub csrf_hash: SecretHash,
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub user_id: UserId,
    pub secret_hash: SecretHash,
    pub csrf_hash: SecretHash,
    pub created_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub revoked_at: Option<OffsetDateTime>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NamespaceRole {
    Owner,
    Maintainer,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceRecord {
    pub id: NamespaceId,
    pub name: IdentitySegment,
    pub created_by_user_id: Option<UserId>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceOwnerRecord {
    pub namespace_id: NamespaceId,
    pub user_id: UserId,
    pub role: NamespaceRole,
    pub added_by_user_id: Option<UserId>,
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceMembershipRecord {
    pub namespace: NamespaceRecord,
    pub membership: NamespaceOwnerRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceMemberRecord {
    pub membership: NamespaceOwnerRecord,
    pub user: UserRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewInvitation {
    pub namespace_id: NamespaceId,
    pub invited_user_id: UserId,
    pub invited_by_user_id: Option<UserId>,
    pub role: NamespaceRole,
    pub expires_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvitationResolution {
    Accepted,
    Revoked,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvitationRecord {
    pub id: InvitationId,
    pub namespace_id: NamespaceId,
    pub invited_user_id: UserId,
    pub invited_by_user_id: Option<UserId>,
    pub role: NamespaceRole,
    pub created_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub accepted_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceInvitationRecord {
    pub invitation: InvitationRecord,
    pub namespace: NamespaceRecord,
    pub invited_user: UserRecord,
    pub invited_by_user: Option<UserRecord>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum TokenScope {
    Publish,
    Yank,
    Namespace,
}

impl TokenScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Yank => "yank",
            Self::Namespace => "namespace",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewApiToken {
    pub user_id: UserId,
    pub display_name: String,
    pub token_prefix: String,
    pub secret_hash: SecretHash,
    pub scopes: Vec<TokenScope>,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiTokenRecord {
    pub id: ApiTokenId,
    pub user_id: UserId,
    pub display_name: String,
    pub token_prefix: String,
    pub secret_hash: SecretHash,
    pub scopes: Vec<TokenScope>,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
    pub expires_at: Option<OffsetDateTime>,
    pub revoked_at: Option<OffsetDateTime>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageKind {
    Program,
    Library,
    Source,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockedIdentityKind {
    Namespace,
    Package,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageRecord {
    pub id: PackageId,
    pub namespace_id: NamespaceId,
    pub name: IdentitySegment,
    pub created_by_user_id: Option<UserId>,
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug)]
pub struct DependencyRecord {
    pub alias: IdentitySegment,
    pub target_namespace: IdentitySegment,
    pub target_package: IdentitySegment,
    pub version_range: VersionRange,
    pub target_os: Vec<TargetOs>,
}

#[derive(Clone, Debug)]
pub struct NewPackageVersion {
    pub package_id: PackageId,
    pub version: SemanticVersion,
    pub manifest_schema_version: u16,
    pub min_rux: SemanticVersion,
    pub package_type: PackageKind,
    pub description: Option<String>,
    pub repository_url: Option<String>,
    pub homepage_url: Option<String>,
    pub readme_file: Option<(String, String)>,
    pub license_expression: Option<String>,
    pub license_file: Option<(String, String)>,
    pub normalized_manifest: JsonObject,
    pub artifact_sha256: ArtifactSha256,
    pub artifact_size: u64,
    pub storage_key: String,
    pub artifact_file_count: u32,
    pub artifact_expanded_bytes: u64,
    pub source_file_count: u32,
    pub source_line_count: u64,
    pub published_by_user_id: Option<UserId>,
    pub authors: Vec<String>,
    pub keywords: Vec<IdentitySegment>,
    pub dependencies: Vec<DependencyRecord>,
}

#[derive(Clone, Debug)]
pub struct PackageVersionRecord {
    pub id: PackageVersionId,
    pub package_id: PackageId,
    pub version: SemanticVersion,
    pub manifest_schema_version: u16,
    pub min_rux: SemanticVersion,
    pub package_type: PackageKind,
    pub description: Option<String>,
    pub repository_url: Option<String>,
    pub homepage_url: Option<String>,
    pub readme_file: Option<(String, String)>,
    pub license_expression: Option<String>,
    pub license_file: Option<(String, String)>,
    pub normalized_manifest: JsonObject,
    pub artifact_sha256: ArtifactSha256,
    pub artifact_size: u64,
    pub storage_key: String,
    pub artifact_file_count: u32,
    pub artifact_expanded_bytes: u64,
    pub source_file_count: u32,
    pub source_line_count: u64,
    pub published_by_user_id: Option<UserId>,
    pub published_at: OffsetDateTime,
    pub yanked_at: Option<OffsetDateTime>,
    pub yanked_by_user_id: Option<UserId>,
    pub authors: Vec<String>,
    pub keywords: Vec<IdentitySegment>,
    pub dependencies: Vec<DependencyRecord>,
}

#[derive(Clone, Debug)]
pub struct PackageSummaryRecord {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub created_at: OffsetDateTime,
}

#[derive(Clone, Debug)]
pub struct PackageVersionMetadataRecord {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub version: SemanticVersion,
    pub manifest_schema_version: u16,
    pub min_rux: SemanticVersion,
    pub package_type: PackageKind,
    pub description: Option<String>,
    pub repository_url: Option<String>,
    pub homepage_url: Option<String>,
    pub readme_file: Option<(String, String)>,
    pub license_expression: Option<String>,
    pub license_file: Option<(String, String)>,
    pub normalized_manifest: JsonObject,
    pub artifact_sha256: ArtifactSha256,
    pub artifact_size: u64,
    pub artifact_file_count: u32,
    pub artifact_expanded_bytes: u64,
    pub source_file_count: u32,
    pub source_line_count: u64,
    pub published_at: OffsetDateTime,
    pub yanked: bool,
    pub authors: Vec<String>,
    pub keywords: Vec<IdentitySegment>,
    pub dependencies: Vec<DependencyRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadTargetRecord {
    pub package_version_id: PackageVersionId,
    pub storage_key: String,
}

#[derive(Clone, Debug)]
pub struct ResolverIndexRecord {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub versions: Vec<ResolverVersionRecord>,
}

#[derive(Clone, Debug)]
pub struct ResolverVersionRecord {
    pub version: SemanticVersion,
    pub min_rux: SemanticVersion,
    pub yanked: bool,
    pub dependencies: Vec<DependencyRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageSearchCriteria {
    pub query: Option<String>,
    pub identity_query: Option<String>,
    pub namespace: Option<IdentitySegment>,
    pub keyword: Option<IdentitySegment>,
    pub package_type: Option<PackageKind>,
    pub sort: PackageSortOrder,
}

/// The result ordering a catalog request asks for.
///
/// `Relevance` is only meaningful alongside a query; without one every row
/// scores zero and the order collapses to the normalized-name tiebreak, which
/// is what `Name` asks for outright.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PackageSortOrder {
    #[default]
    Relevance,
    Name,
    Downloads,
    RecentDownloads,
    Updated,
    Created,
}

#[derive(Clone, Debug)]
pub struct PackageSearchRecord {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub version: SemanticVersion,
    pub package_type: PackageKind,
    pub description: Option<String>,
    pub published_at: OffsetDateTime,
    pub yanked: bool,
    pub downloads_total: i64,
    pub downloads_30d: i64,
}

/// One page of catalog rows plus the size of the full result set.
///
/// The total comes from a `COUNT(*) OVER ()` in the same statement, so a page
/// request stays a single round trip.
#[derive(Clone, Debug)]
pub struct PackageSearchPageRecord {
    pub items: Vec<PackageSearchRecord>,
    pub total: u64,
}

#[derive(Clone, Debug)]
pub struct DependentPackageRecord {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub version: SemanticVersion,
    pub package_type: PackageKind,
    pub description: Option<String>,
    pub published_at: OffsetDateTime,
    pub yanked: bool,
    pub requirements: Vec<DependencyRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageIdentityBoundary {
    pub namespace: String,
    pub package: String,
}

#[derive(Clone, Debug)]
pub struct KeywordRecord {
    pub keyword: IdentitySegment,
    pub package_count: u64,
}

/// The orderings the keyword index can be read in.
///
/// `Packages` is the default because the busiest topics are the useful way into
/// an unfamiliar registry; `Name` is for looking one up.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum KeywordSortOrder {
    #[default]
    Packages,
    Name,
}

/// One page of keywords plus the size of the whole index.
#[derive(Clone, Debug)]
pub struct KeywordPageRecord {
    pub items: Vec<KeywordRecord>,
    pub total: u64,
}

#[derive(Clone, Debug)]
pub struct PackageVersionHistoryRecord {
    pub version: SemanticVersion,
    pub min_rux: SemanticVersion,
    pub package_type: PackageKind,
    pub published_at: OffsetDateTime,
    pub yanked: bool,
}

#[derive(Clone, Debug)]
pub struct HighlightPackageRecord {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub version: SemanticVersion,
    pub package_type: PackageKind,
    pub description: Option<String>,
    pub published_at: OffsetDateTime,
    pub downloads: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct PackageHighlightsRecord {
    pub recent: Vec<HighlightPackageRecord>,
    pub popular: Vec<HighlightPackageRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDownloadDayRecord {
    pub date: Date,
    pub downloads: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageDownloadStatisticsRecord {
    pub start_date: Date,
    pub end_date: Date,
    pub total_downloads: u64,
    pub total_all_time: u64,
    pub daily: Vec<PackageDownloadDayRecord>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SitemapEntryKind {
    Keyword,
    Namespace,
    Package,
}

#[derive(Clone, Debug)]
pub struct SitemapEntryRecord {
    pub kind: SitemapEntryKind,
    pub namespace: Option<IdentitySegment>,
    pub package: Option<IdentitySegment>,
    pub keyword: Option<IdentitySegment>,
    pub last_modified: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SitemapBoundary {
    pub kind: SitemapEntryKind,
    pub first_identity: String,
    pub second_identity: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteOutcome {
    Applied,
    NotFound,
    Unchanged,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryConflict {
    GitHubIdentity,
    GitHubLogin,
    SessionSecret,
    NamespaceIdentity,
    PendingInvitation,
    PackageIdentity,
    PackageVersion,
    StorageKey,
    TokenPrefix,
    TokenSecret,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepositoryErrorKind {
    Conflict(RepositoryConflict),
    Retryable,
    Unavailable,
    CorruptData,
    Unexpected,
}

#[derive(Debug)]
pub struct RepositoryError {
    kind: RepositoryErrorKind,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl RepositoryError {
    #[must_use]
    pub const fn new(kind: RepositoryErrorKind) -> Self {
        Self { kind, source: None }
    }

    pub fn with_source(
        kind: RepositoryErrorKind,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            source: Some(Box::new(source)),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> RepositoryErrorKind {
        self.kind
    }
}

impl fmt::Display for RepositoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "repository operation failed: {:?}", self.kind)
    }
}

impl Error for RepositoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

#[async_trait]
pub trait AccountReader: Send + Sync {
    async fn user_by_id(&self, id: UserId) -> Result<Option<UserRecord>, RepositoryError>;
    async fn user_by_github_id(&self, id: u64) -> Result<Option<UserRecord>, RepositoryError>;
    async fn user_by_github_login(
        &self,
        login: &str,
    ) -> Result<Option<UserRecord>, RepositoryError>;
    async fn session_by_secret_hash(
        &self,
        hash: SecretHash,
    ) -> Result<Option<SessionRecord>, RepositoryError>;
}

#[async_trait]
pub trait NamespaceReader: Send + Sync {
    async fn namespace_by_name(
        &self,
        name: &IdentitySegment,
    ) -> Result<Option<NamespaceRecord>, RepositoryError>;
    async fn namespace_role(
        &self,
        namespace_id: NamespaceId,
        user_id: UserId,
    ) -> Result<Option<NamespaceOwnerRecord>, RepositoryError>;
    async fn invitation_by_id(
        &self,
        id: InvitationId,
    ) -> Result<Option<InvitationRecord>, RepositoryError>;
    async fn namespaces_by_user_id(
        &self,
        user_id: UserId,
    ) -> Result<Vec<NamespaceMembershipRecord>, RepositoryError>;
    async fn namespace_members(
        &self,
        namespace_id: NamespaceId,
    ) -> Result<Vec<NamespaceMemberRecord>, RepositoryError>;
    async fn pending_invitations_by_user_id(
        &self,
        user_id: UserId,
        at: OffsetDateTime,
    ) -> Result<Vec<NamespaceInvitationRecord>, RepositoryError>;
    async fn pending_invitations_by_namespace(
        &self,
        namespace_id: NamespaceId,
        at: OffsetDateTime,
    ) -> Result<Vec<NamespaceInvitationRecord>, RepositoryError>;
}

#[async_trait]
pub trait TokenReader: Send + Sync {
    async fn token_by_secret_hash(
        &self,
        hash: SecretHash,
    ) -> Result<Option<ApiTokenRecord>, RepositoryError>;

    async fn tokens_by_user_id(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ApiTokenRecord>, RepositoryError>;
}

#[async_trait]
pub trait CatalogReader: Send + Sync {
    async fn package_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
    ) -> Result<Option<PackageRecord>, RepositoryError>;
    async fn version_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Result<Option<PackageVersionRecord>, RepositoryError>;
}

#[async_trait]
pub trait PackageMetadataReader: Send + Sync {
    async fn package_summary_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
    ) -> Result<Option<PackageSummaryRecord>, RepositoryError>;

    async fn package_version_metadata_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Result<Option<PackageVersionMetadataRecord>, RepositoryError>;
}

#[async_trait]
pub trait ResolverIndexReader: Send + Sync {
    async fn resolver_index_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
    ) -> Result<Option<ResolverIndexRecord>, RepositoryError>;
}

#[async_trait]
pub trait PackageSearchReader: Send + Sync {
    async fn search_packages(
        &self,
        criteria: &PackageSearchCriteria,
        page: u32,
        per_page: u16,
    ) -> Result<PackageSearchPageRecord, RepositoryError>;
}

#[async_trait]
pub trait DiscoveryReader: Send + Sync {
    async fn dependent_packages(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        boundary: Option<&PackageIdentityBoundary>,
        limit: u16,
    ) -> Result<Option<Vec<DependentPackageRecord>>, RepositoryError>;

    async fn keywords(
        &self,
        sort: KeywordSortOrder,
        page: u32,
        per_page: u16,
    ) -> Result<KeywordPageRecord, RepositoryError>;

    async fn package_version_history(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        boundary: Option<&SemanticVersion>,
        limit: u16,
    ) -> Result<Option<Vec<PackageVersionHistoryRecord>>, RepositoryError>;

    async fn package_highlights(
        &self,
        since: OffsetDateTime,
        until: OffsetDateTime,
        limit: u16,
    ) -> Result<PackageHighlightsRecord, RepositoryError>;

    /// Aggregates download events over `[since, until)`, where `until` is the
    /// request instant rather than a day boundary. Daily buckets cover every UTC
    /// calendar day from `since` through the day containing `until` inclusive, so
    /// the final bucket is a partial day.
    async fn package_download_statistics(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        since: OffsetDateTime,
        until: OffsetDateTime,
    ) -> Result<Option<PackageDownloadStatisticsRecord>, RepositoryError>;

    async fn sitemap_entries(
        &self,
        boundary: Option<&SitemapBoundary>,
        limit: u16,
    ) -> Result<Vec<SitemapEntryRecord>, RepositoryError>;
}

#[async_trait]
pub trait DownloadReader: Send + Sync {
    async fn download_target_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Result<Option<DownloadTargetRecord>, RepositoryError>;
}

#[async_trait]
pub trait AccountWriter: Send {
    async fn upsert_github_user(
        &mut self,
        profile: &GitHubUserProfile,
    ) -> Result<UserRecord, RepositoryError>;
    async fn create_session(
        &mut self,
        session: &NewSession,
    ) -> Result<SessionRecord, RepositoryError>;
    async fn touch_session(
        &mut self,
        id: SessionId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError>;
    async fn revoke_session(
        &mut self,
        id: SessionId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError>;
}

#[async_trait]
pub trait AccountTransaction: AccountWriter + AuditWriter + Send {
    async fn lock_session_by_secret_hash(
        &mut self,
        hash: SecretHash,
    ) -> Result<Option<SessionRecord>, RepositoryError>;
    async fn commit(self: Box<Self>) -> Result<(), RepositoryError>;
    async fn rollback(self: Box<Self>) -> Result<(), RepositoryError>;
}

#[async_trait]
pub trait TokenAuthorizationTransaction: TokenWriter + Send {
    async fn lock_user_by_id(&mut self, id: UserId) -> Result<Option<UserRecord>, RepositoryError>;
    async fn lock_token_by_secret_hash(
        &mut self,
        hash: SecretHash,
    ) -> Result<Option<ApiTokenRecord>, RepositoryError>;
    async fn lock_token_by_prefix(
        &mut self,
        user_id: UserId,
        prefix: &str,
    ) -> Result<Option<ApiTokenRecord>, RepositoryError>;
}

#[async_trait]
pub trait TokenTransaction: TokenAuthorizationTransaction + AuditWriter {
    async fn commit(self: Box<Self>) -> Result<(), RepositoryError>;
    async fn rollback(self: Box<Self>) -> Result<(), RepositoryError>;
}

#[async_trait]
pub trait TokenUnitOfWork: Send + Sync {
    async fn begin_tokens(&self) -> Result<Box<dyn TokenTransaction>, RepositoryError>;
}

pub trait TokenRepository: TokenReader + TokenUnitOfWork {}

impl<T> TokenRepository for T where T: TokenReader + TokenUnitOfWork {}

#[async_trait]
pub trait AccountUnitOfWork: Send + Sync {
    async fn begin_account(&self) -> Result<Box<dyn AccountTransaction>, RepositoryError>;
}

pub trait AccountRepository: AccountReader + AccountUnitOfWork {}

impl<T> AccountRepository for T where T: AccountReader + AccountUnitOfWork {}

#[async_trait]
pub trait AccountLifecycleTransaction: AuditWriter + Send {
    async fn lock_user_by_id(&mut self, id: UserId) -> Result<Option<UserRecord>, RepositoryError>;
    async fn lock_memberships_by_user_id(
        &mut self,
        user_id: UserId,
    ) -> Result<Vec<NamespaceMembershipRecord>, RepositoryError>;
    async fn namespace_owner_count(
        &mut self,
        namespace_id: NamespaceId,
    ) -> Result<u64, RepositoryError>;
    async fn revoke_sessions_by_user_id(
        &mut self,
        user_id: UserId,
        at: OffsetDateTime,
    ) -> Result<u64, RepositoryError>;
    async fn revoke_and_scrub_tokens_by_user_id(
        &mut self,
        user_id: UserId,
        at: OffsetDateTime,
        replacement_name: &str,
    ) -> Result<u64, RepositoryError>;
    async fn revoke_incoming_invitations_by_user_id(
        &mut self,
        user_id: UserId,
        at: OffsetDateTime,
    ) -> Result<u64, RepositoryError>;
    async fn remove_memberships_by_user_id(
        &mut self,
        user_id: UserId,
    ) -> Result<u64, RepositoryError>;
    async fn anonymize_user(
        &mut self,
        user_id: UserId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError>;
    async fn commit(self: Box<Self>) -> Result<(), RepositoryError>;
    async fn rollback(self: Box<Self>) -> Result<(), RepositoryError>;
}

#[async_trait]
pub trait AccountLifecycleUnitOfWork: Send + Sync {
    async fn begin_account_lifecycle(
        &self,
    ) -> Result<Box<dyn AccountLifecycleTransaction>, RepositoryError>;
}

pub trait AccountLifecycleRepository: AccountLifecycleUnitOfWork {}

impl<T> AccountLifecycleRepository for T where T: AccountLifecycleUnitOfWork {}

#[async_trait]
pub trait NamespaceWriter: Send {
    async fn create_namespace(
        &mut self,
        name: &IdentitySegment,
        actor: Option<UserId>,
    ) -> Result<NamespaceRecord, RepositoryError>;
    async fn set_namespace_owner(
        &mut self,
        namespace_id: NamespaceId,
        user_id: UserId,
        role: NamespaceRole,
        actor: Option<UserId>,
    ) -> Result<NamespaceOwnerRecord, RepositoryError>;
    async fn remove_namespace_owner(
        &mut self,
        namespace_id: NamespaceId,
        user_id: UserId,
    ) -> Result<WriteOutcome, RepositoryError>;
    async fn create_invitation(
        &mut self,
        invitation: &NewInvitation,
    ) -> Result<InvitationRecord, RepositoryError>;
    async fn resolve_invitation(
        &mut self,
        id: InvitationId,
        resolution: InvitationResolution,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError>;
    async fn touch_namespace(
        &mut self,
        namespace_id: NamespaceId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError>;
}

#[async_trait]
pub trait TokenWriter: Send {
    async fn create_token(
        &mut self,
        token: &NewApiToken,
    ) -> Result<ApiTokenRecord, RepositoryError>;
    async fn touch_token(
        &mut self,
        id: ApiTokenId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError>;
    async fn revoke_token(
        &mut self,
        id: ApiTokenId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError>;
}

#[async_trait]
pub trait CatalogWriter: Send {
    async fn create_package(
        &mut self,
        namespace_id: NamespaceId,
        name: &IdentitySegment,
        actor: Option<UserId>,
    ) -> Result<PackageRecord, RepositoryError>;
    async fn create_package_version(
        &mut self,
        version: &NewPackageVersion,
    ) -> Result<PackageVersionRecord, RepositoryError>;
    async fn set_yank(
        &mut self,
        id: PackageVersionId,
        yank: Option<(OffsetDateTime, UserId)>,
    ) -> Result<WriteOutcome, RepositoryError>;
}

#[async_trait]
pub trait AuditWriter: Send {
    async fn append_audit(&mut self, event: &crate::AuditEvent) -> Result<(), RepositoryError>;
}

#[async_trait]
pub trait DownloadWriter: Send {
    async fn append_download(
        &mut self,
        version_id: PackageVersionId,
        at: OffsetDateTime,
    ) -> Result<(), RepositoryError>;
}

#[async_trait]
pub trait DownloadTransaction: DownloadWriter {
    async fn commit(self: Box<Self>) -> Result<(), RepositoryError>;
    async fn rollback(self: Box<Self>) -> Result<(), RepositoryError>;
}

#[async_trait]
pub trait DownloadUnitOfWork: Send + Sync {
    async fn begin_download(&self) -> Result<Box<dyn DownloadTransaction>, RepositoryError>;
}

pub trait DownloadRepository: DownloadReader + DownloadUnitOfWork {}

impl<T> DownloadRepository for T where T: DownloadReader + DownloadUnitOfWork {}

#[async_trait]
pub trait TransactionReader: Send {
    async fn is_identity_blocked(
        &mut self,
        kind: BlockedIdentityKind,
        name: &IdentitySegment,
    ) -> Result<bool, RepositoryError>;
    async fn lock_namespace_by_name(
        &mut self,
        name: &IdentitySegment,
    ) -> Result<Option<NamespaceRecord>, RepositoryError>;
    async fn lock_namespace_role(
        &mut self,
        namespace_id: NamespaceId,
        user_id: UserId,
    ) -> Result<Option<NamespaceOwnerRecord>, RepositoryError>;
    async fn user_by_github_login_in_transaction(
        &mut self,
        login: &str,
    ) -> Result<Option<UserRecord>, RepositoryError>;
    async fn lock_pending_invitation(
        &mut self,
        namespace_id: NamespaceId,
        invited_user_id: UserId,
    ) -> Result<Option<InvitationRecord>, RepositoryError>;
    async fn namespace_owner_count(
        &mut self,
        namespace_id: NamespaceId,
    ) -> Result<u64, RepositoryError>;
    async fn lock_package_by_name(
        &mut self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
    ) -> Result<Option<PackageRecord>, RepositoryError>;
    async fn lock_version_id_by_name(
        &mut self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Result<Option<PackageVersionId>, RepositoryError>;
}

#[async_trait]
pub trait RegistryTransaction:
    AccountWriter
    + NamespaceWriter
    + TokenAuthorizationTransaction
    + CatalogWriter
    + AuditWriter
    + DownloadWriter
    + TransactionReader
    + Send
{
    async fn commit(self: Box<Self>) -> Result<(), RepositoryError>;
    async fn rollback(self: Box<Self>) -> Result<(), RepositoryError>;
}

#[async_trait]
pub trait UnitOfWork: Send + Sync {
    async fn begin(&self) -> Result<Box<dyn RegistryTransaction>, RepositoryError>;
}

pub trait RegistryRepository:
    AccountReader + NamespaceReader + TokenReader + CatalogReader + UnitOfWork
{
}

impl<T> RegistryRepository for T where
    T: AccountReader + NamespaceReader + TokenReader + CatalogReader + UnitOfWork
{
}
