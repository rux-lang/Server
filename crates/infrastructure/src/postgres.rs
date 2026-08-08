use async_trait::async_trait;
use rux_application::{
    AccountLifecycleTransaction, AccountLifecycleUnitOfWork, AccountReader, AccountTransaction,
    AccountUnitOfWork, AccountWriter, ApiTokenId, ApiTokenRecord, ArtifactReferenceReader,
    ArtifactSha256, AuditEvent, AuditWriter, BlockedIdentityKind, CatalogReader, CatalogWriter,
    DashboardActivity, DashboardActivityKind, DashboardDownloadLeader, DashboardDownloads,
    DashboardInvitation, DashboardNamespace, DashboardPackage, DashboardReader, DashboardSnapshot,
    DashboardUser, DependencyRecord, DependentPackageRecord, DiscoveryReader, DownloadReader,
    DownloadTargetRecord, DownloadTransaction, DownloadUnitOfWork, DownloadWriter,
    GitHubUserProfile, HighlightPackageRecord, InvitationId, InvitationRecord,
    InvitationResolution, KeywordBoundary, KeywordRecord, NamespaceId, NamespaceInvitationRecord,
    NamespaceMemberRecord, NamespaceMembershipRecord, NamespaceOwnerRecord, NamespaceReader,
    NamespaceRecord, NamespaceRole, NamespaceWriter, NewApiToken, NewInvitation, NewPackageVersion,
    NewSession, PackageDownloadDayRecord, PackageDownloadStatisticsRecord, PackageHighlightsRecord,
    PackageId, PackageIdentityBoundary, PackageKind, PackageMetadataReader, PackageRecord,
    PackageSearchBoundary, PackageSearchCriteria, PackageSearchReader, PackageSearchRecord,
    PackageSummaryRecord, PackageVersionHistoryRecord, PackageVersionId,
    PackageVersionMetadataRecord, PackageVersionRecord, RegistryTransaction, RepositoryConflict,
    RepositoryError, RepositoryErrorKind, ResolverIndexReader, ResolverIndexRecord,
    ResolverVersionRecord, SecretHash, SessionId, SessionRecord, SitemapBoundary, SitemapEntryKind,
    SitemapEntryRecord, TokenAuthorizationTransaction, TokenReader, TokenScope, TokenTransaction,
    TokenUnitOfWork, TokenWriter, TransactionReader, UnitOfWork, UserId, UserRecord, WriteOutcome,
};
use rux_domain::{IdentitySegment, SemanticVersion, VersionRange};
use serde_json::Value;
use sqlx::{FromRow, PgPool, Postgres, Row, Transaction, query, query_as, query_scalar};
use thiserror::Error;
use time::{Date, OffsetDateTime};
use uuid::Uuid;

#[derive(Clone)]
pub struct PostgresRepository {
    pool: PgPool,
}

impl PostgresRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) const fn pool(&self) -> &PgPool {
        &self.pool
    }
}

pub struct PostgresTransaction {
    transaction: Transaction<'static, Postgres>,
}

#[derive(Debug, Error)]
#[error("stored registry data is invalid: {0}")]
struct StoredDataError(String);

#[derive(FromRow)]
struct UserRow {
    id: Uuid,
    github_user_id: Option<String>,
    github_login: Option<String>,
    display_name: Option<String>,
    avatar_url: Option<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    anonymized_at: Option<OffsetDateTime>,
}

#[derive(FromRow)]
struct SessionRow {
    id: Uuid,
    user_id: Uuid,
    secret_hash: Vec<u8>,
    csrf_hash: Vec<u8>,
    created_at: OffsetDateTime,
    last_seen_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    revoked_at: Option<OffsetDateTime>,
}

#[derive(FromRow)]
struct NamespaceRow {
    id: Uuid,
    display_name: String,
    created_by_user_id: Option<Uuid>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

#[derive(FromRow)]
struct OwnerRow {
    namespace_id: Uuid,
    user_id: Uuid,
    role: String,
    added_by_user_id: Option<Uuid>,
    created_at: OffsetDateTime,
}

#[derive(FromRow)]
struct InvitationRow {
    id: Uuid,
    namespace_id: Uuid,
    invited_user_id: Uuid,
    invited_by_user_id: Option<Uuid>,
    role: String,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    accepted_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
}

#[derive(FromRow)]
struct MembershipRow {
    namespace_id: Uuid,
    namespace_display_name: String,
    namespace_created_by_user_id: Option<Uuid>,
    namespace_created_at: OffsetDateTime,
    namespace_updated_at: OffsetDateTime,
    user_id: Uuid,
    role: String,
    added_by_user_id: Option<Uuid>,
    membership_created_at: OffsetDateTime,
}

#[derive(FromRow)]
struct MemberRow {
    namespace_id: Uuid,
    user_id: Uuid,
    role: String,
    added_by_user_id: Option<Uuid>,
    membership_created_at: OffsetDateTime,
    github_user_id: Option<String>,
    github_login: Option<String>,
    display_name: Option<String>,
    avatar_url: Option<String>,
    user_created_at: OffsetDateTime,
    user_updated_at: OffsetDateTime,
    anonymized_at: Option<OffsetDateTime>,
}

#[derive(FromRow)]
struct InvitationDetailsRow {
    invitation_id: Uuid,
    namespace_id: Uuid,
    invited_user_id: Uuid,
    invited_by_user_id: Option<Uuid>,
    role: String,
    invitation_created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    accepted_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
    namespace_display_name: String,
    namespace_created_by_user_id: Option<Uuid>,
    namespace_created_at: OffsetDateTime,
    namespace_updated_at: OffsetDateTime,
    invited_github_user_id: Option<String>,
    invited_github_login: Option<String>,
    invited_display_name: Option<String>,
    invited_avatar_url: Option<String>,
    invited_created_at: OffsetDateTime,
    invited_updated_at: OffsetDateTime,
    invited_anonymized_at: Option<OffsetDateTime>,
    inviter_id: Option<Uuid>,
    inviter_github_user_id: Option<String>,
    inviter_github_login: Option<String>,
    inviter_display_name: Option<String>,
    inviter_avatar_url: Option<String>,
    inviter_created_at: Option<OffsetDateTime>,
    inviter_updated_at: Option<OffsetDateTime>,
    inviter_anonymized_at: Option<OffsetDateTime>,
}

#[derive(FromRow)]
struct DashboardCountsRow {
    namespaces: i64,
    packages: i64,
    invitations: i64,
}

#[derive(FromRow)]
struct DashboardNamespaceRow {
    namespace_display_name: String,
    role: String,
    package_count: i64,
}

#[derive(FromRow)]
struct DashboardPackageRow {
    namespace_display_name: String,
    package_display_name: String,
    version: String,
    published_at: OffsetDateTime,
    yanked: bool,
    version_count: i64,
}

#[derive(FromRow)]
struct DashboardInvitationRow {
    namespace_display_name: String,
    inviter_github_login: Option<String>,
    inviter_display_name: Option<String>,
    inviter_avatar_url: Option<String>,
    role: String,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
}

#[derive(FromRow)]
struct DashboardActivityRow {
    action: String,
    namespace_display_name: String,
    actor_github_login: Option<String>,
    actor_display_name: Option<String>,
    actor_avatar_url: Option<String>,
    package_display_name: Option<String>,
    version: Option<String>,
    target_github_login: Option<String>,
    target_display_name: Option<String>,
    target_avatar_url: Option<String>,
    previous_role: Option<String>,
    role: Option<String>,
    occurred_at: OffsetDateTime,
}

#[derive(FromRow)]
struct DashboardDownloadTotalsRow {
    total_30d: i64,
    total_all_time: i64,
}

#[derive(FromRow)]
struct DashboardDownloadLeaderRow {
    namespace_display_name: String,
    package_display_name: String,
    downloads_30d: i64,
}

#[derive(FromRow)]
struct TokenRow {
    id: Uuid,
    user_id: Uuid,
    display_name: String,
    token_prefix: String,
    secret_hash: Vec<u8>,
    created_at: OffsetDateTime,
    last_used_at: Option<OffsetDateTime>,
    expires_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
}

#[derive(FromRow)]
struct PackageRow {
    id: Uuid,
    namespace_id: Uuid,
    display_name: String,
    created_by_user_id: Option<Uuid>,
    created_at: OffsetDateTime,
}

#[derive(FromRow)]
struct PackageSummaryRow {
    namespace_display_name: String,
    package_display_name: String,
    created_at: OffsetDateTime,
}

#[derive(FromRow)]
struct DownloadTargetRow {
    package_version_id: Uuid,
    storage_key: String,
}

#[derive(FromRow)]
struct VersionRow {
    id: Uuid,
    package_id: Uuid,
    version: String,
    manifest_schema_version: i16,
    min_rux: String,
    package_type: String,
    description: Option<String>,
    repository_url: Option<String>,
    homepage_url: Option<String>,
    readme_file_path: Option<String>,
    readme_file_text: Option<String>,
    license_expression: Option<String>,
    license_file_path: Option<String>,
    license_file_text: Option<String>,
    normalized_manifest: Value,
    artifact_sha256: Vec<u8>,
    artifact_size: i64,
    storage_key: String,
    artifact_file_count: i32,
    artifact_expanded_bytes: i64,
    source_file_count: i32,
    source_line_count: i64,
    published_by_user_id: Option<Uuid>,
    published_at: OffsetDateTime,
    yanked_at: Option<OffsetDateTime>,
    yanked_by_user_id: Option<Uuid>,
}

#[derive(FromRow)]
struct ResolverIndexRow {
    namespace_display_name: String,
    package_display_name: String,
    package_version_id: Option<Uuid>,
    version: Option<String>,
    min_rux: Option<String>,
    yanked: bool,
    dependency_alias: Option<String>,
    dependency_target_namespace: Option<String>,
    dependency_target_package: Option<String>,
    dependency_version_range: Option<String>,
}

#[derive(FromRow)]
struct PackageSearchRow {
    namespace_display_name: String,
    package_display_name: String,
    version: String,
    package_type: String,
    description: Option<String>,
    published_at: OffsetDateTime,
    yanked: bool,
    match_class: i32,
    relevance: i64,
}

#[derive(FromRow)]
struct DependentPackageRow {
    package_version_id: Uuid,
    namespace_display_name: String,
    package_display_name: String,
    version: String,
    package_type: String,
    description: Option<String>,
    published_at: OffsetDateTime,
    yanked: bool,
    dependency_alias: String,
    dependency_target_namespace: String,
    dependency_target_package: String,
    dependency_version_range: String,
}

#[derive(FromRow)]
struct KeywordDiscoveryRow {
    keyword_display_name: String,
    package_count: i64,
}

#[derive(FromRow)]
struct VersionHistoryRow {
    version: String,
    min_rux: String,
    package_type: String,
    published_at: OffsetDateTime,
    yanked: bool,
}

#[derive(FromRow)]
struct HighlightPackageRow {
    namespace_display_name: String,
    package_display_name: String,
    version: String,
    package_type: String,
    description: Option<String>,
    published_at: OffsetDateTime,
    downloads: Option<i64>,
}

#[derive(FromRow)]
struct PackageDownloadDayRow {
    date: Date,
    downloads: i64,
    total_downloads: i64,
    total_all_time: i64,
}

#[derive(FromRow)]
struct SitemapEntryRow {
    kind: String,
    namespace_display_name: Option<String>,
    package_display_name: Option<String>,
    keyword_display_name: Option<String>,
    last_modified: OffsetDateTime,
}

const USER_COLUMNS: &str = "id, github_user_id::text AS github_user_id, github_login, display_name, avatar_url, created_at, updated_at, anonymized_at";
const SESSION_COLUMNS: &str =
    "id, user_id, secret_hash, csrf_hash, created_at, last_seen_at, expires_at, revoked_at";
const NAMESPACE_COLUMNS: &str = "id, display_name, created_by_user_id, created_at, updated_at";
const INVITATION_COLUMNS: &str = "id, namespace_id, invited_user_id, invited_by_user_id, role, created_at, expires_at, accepted_at, revoked_at";
const TOKEN_COLUMNS: &str = "id, user_id, display_name, token_prefix, secret_hash, created_at, last_used_at, expires_at, revoked_at";
const PACKAGE_COLUMNS: &str = "id, namespace_id, display_name, created_by_user_id, created_at";
const QUALIFIED_PACKAGE_COLUMNS: &str =
    "p.id, p.namespace_id, p.display_name, p.created_by_user_id, p.created_at";
const VERSION_COLUMNS: &str = "v.id, v.package_id, v.version, v.manifest_schema_version, v.min_rux, v.package_type, v.description, v.repository_url, v.homepage_url, v.readme_file_path, v.readme_file_text, v.license_expression, v.license_file_path, v.license_file_text, v.normalized_manifest, v.artifact_sha256, v.artifact_size, v.storage_key, v.artifact_file_count, v.artifact_expanded_bytes, v.source_file_count, v.source_line_count, v.published_by_user_id, v.published_at, v.yanked_at, v.yanked_by_user_id";
const REPRESENTATIVE_CTE: &str = "representative AS (
    SELECT n.display_name AS namespace_display_name,
           n.normalized_name AS namespace_normalized_name,
           p.id AS package_id,
           p.display_name AS package_display_name,
           p.normalized_name AS package_normalized_name,
           v.id AS package_version_id,
           v.version,
           v.package_type,
           v.description,
           v.published_at,
           (v.yanked_at IS NOT NULL) AS yanked
    FROM namespaces n
    JOIN packages p ON p.namespace_id = n.id
    CROSS JOIN LATERAL (
        SELECT candidate.*
        FROM package_versions candidate
        WHERE candidate.package_id = p.id
        ORDER BY
            (candidate.yanked_at IS NULL) DESC,
            (candidate.prerelease IS NULL) DESC,
            candidate.major DESC,
            candidate.minor DESC,
            candidate.patch DESC,
            candidate.prerelease_sort_key DESC,
            (candidate.build_metadata IS NOT NULL) DESC,
            candidate.build_metadata_sort_key DESC,
            candidate.id DESC
        LIMIT 1
    ) v
)";

const ACTIVE_REPRESENTATIVE_CTE: &str = "active_representative AS (
    SELECT n.display_name AS namespace_display_name,
           n.normalized_name AS namespace_normalized_name,
           p.id AS package_id,
           p.display_name AS package_display_name,
           p.normalized_name AS package_normalized_name,
           v.version,
           v.package_type,
           v.description,
           v.published_at
    FROM namespaces n
    JOIN packages p ON p.namespace_id = n.id
    CROSS JOIN LATERAL (
        SELECT candidate.*
        FROM package_versions candidate
        WHERE candidate.package_id = p.id AND candidate.yanked_at IS NULL
        ORDER BY
            (candidate.prerelease IS NULL) DESC,
            candidate.major DESC,
            candidate.minor DESC,
            candidate.patch DESC,
            candidate.prerelease_sort_key DESC,
            (candidate.build_metadata IS NOT NULL) DESC,
            candidate.build_metadata_sort_key DESC,
            candidate.id DESC
        LIMIT 1
    ) v
)";
const PACKAGE_SEARCH_SQL: &str = "WITH representative AS (
     SELECT n.display_name AS namespace_display_name,
            n.normalized_name AS namespace_normalized_name,
            p.display_name AS package_display_name,
            p.normalized_name AS package_normalized_name,
            v.id AS package_version_id,
            v.version,
            v.package_type,
            v.description,
            v.published_at,
            (v.yanked_at IS NOT NULL) AS yanked,
            v.search_vector
     FROM namespaces n
     JOIN packages p ON p.namespace_id = n.id
     CROSS JOIN LATERAL (
         SELECT candidate.*
         FROM package_versions candidate
         WHERE candidate.package_id = p.id
         ORDER BY
             (candidate.yanked_at IS NULL) DESC,
             (candidate.prerelease IS NULL) DESC,
             candidate.major DESC,
             candidate.minor DESC,
             candidate.patch DESC,
             candidate.prerelease_sort_key DESC,
             (candidate.build_metadata IS NOT NULL) DESC,
             candidate.build_metadata_sort_key DESC,
             candidate.id DESC
         LIMIT 1
     ) v
     WHERE ($4::TEXT IS NULL OR n.normalized_name = $4)
       AND ($5::TEXT IS NULL OR EXISTS (
           SELECT 1
           FROM package_version_keywords filter_keyword
           WHERE filter_keyword.package_version_id = v.id
             AND filter_keyword.normalized_name = $5
       ))
       AND ($6::TEXT IS NULL OR v.package_type = $6)
 ), scored AS (
     SELECT r.*,
            CASE
                WHEN $1::TEXT IS NULL THEN 0
                WHEN namespace_normalized_name || '/' || package_normalized_name = $2 THEN 5
                WHEN package_normalized_name = $2 THEN 4
                WHEN EXISTS (
                    SELECT 1 FROM package_version_keywords exact_keyword
                    WHERE exact_keyword.package_version_id = r.package_version_id
                      AND exact_keyword.normalized_name = $2
                ) THEN 3
                WHEN namespace_normalized_name = $2 THEN 2
                ELSE 1
            END AS match_class,
            CASE WHEN $1::TEXT IS NULL THEN 0::BIGINT ELSE
                round(1000000 * GREATEST(
                    similarity(namespace_normalized_name, $2),
                    similarity(package_normalized_name, $2),
                    similarity(namespace_normalized_name || '/' || package_normalized_name, $2),
                    COALESCE((
                        SELECT max(similarity(rank_keyword.normalized_name, $2))
                        FROM package_version_keywords rank_keyword
                        WHERE rank_keyword.package_version_id = r.package_version_id
                    ), 0)
                ))::BIGINT
                + round(1000000 * ts_rank_cd(
                    search_vector,
                    plainto_tsquery('simple', $1),
                    32
                ))::BIGINT
            END AS relevance
     FROM representative r
     WHERE $1::TEXT IS NULL
        OR namespace_normalized_name || '/' || package_normalized_name = $2
        OR namespace_normalized_name LIKE $3 ESCAPE '\\'
        OR package_normalized_name LIKE $3 ESCAPE '\\'
        OR EXISTS (
            SELECT 1 FROM package_version_keywords match_keyword
            WHERE match_keyword.package_version_id = r.package_version_id
              AND match_keyword.normalized_name LIKE $3 ESCAPE '\\'
        )
        OR search_vector @@ plainto_tsquery('simple', $1)
 )
 SELECT namespace_display_name,
        package_display_name,
        version,
        package_type,
        description,
        published_at,
        yanked,
        match_class,
        relevance
 FROM scored
 WHERE $7::INTEGER IS NULL
    OR match_class < $7
    OR (match_class = $7 AND relevance < $8)
    OR (
        match_class = $7 AND relevance = $8
        AND (namespace_normalized_name, package_normalized_name) > ($9, $10)
    )
 ORDER BY match_class DESC,
          relevance DESC,
          namespace_normalized_name,
          package_normalized_name
 LIMIT $11";

fn data_error(message: impl Into<String>) -> RepositoryError {
    RepositoryError::with_source(
        RepositoryErrorKind::CorruptData,
        StoredDataError(message.into()),
    )
}

fn map_sqlx(error: sqlx::Error) -> RepositoryError {
    let kind = match &error {
        sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::Io(_)
        | sqlx::Error::Tls(_) => RepositoryErrorKind::Unavailable,
        sqlx::Error::Database(database)
            if matches!(database.code().as_deref(), Some("40001" | "40P01")) =>
        {
            RepositoryErrorKind::Retryable
        }
        sqlx::Error::Database(database) => match database.constraint() {
            Some("users_github_user_id_key") => {
                RepositoryErrorKind::Conflict(RepositoryConflict::GitHubIdentity)
            }
            Some("users_normalized_github_login_unique") => {
                RepositoryErrorKind::Conflict(RepositoryConflict::GitHubLogin)
            }
            Some("sessions_secret_hash_key") => {
                RepositoryErrorKind::Conflict(RepositoryConflict::SessionSecret)
            }
            Some("namespaces_normalized_name_unique") => {
                RepositoryErrorKind::Conflict(RepositoryConflict::NamespaceIdentity)
            }
            Some("namespace_invitations_one_pending_idx") => {
                RepositoryErrorKind::Conflict(RepositoryConflict::PendingInvitation)
            }
            Some("packages_namespace_normalized_name_unique") => {
                RepositoryErrorKind::Conflict(RepositoryConflict::PackageIdentity)
            }
            Some("package_versions_package_version_unique") => {
                RepositoryErrorKind::Conflict(RepositoryConflict::PackageVersion)
            }
            Some("package_versions_storage_key_key") => {
                RepositoryErrorKind::Conflict(RepositoryConflict::StorageKey)
            }
            Some("api_tokens_token_prefix_key") => {
                RepositoryErrorKind::Conflict(RepositoryConflict::TokenPrefix)
            }
            Some("api_tokens_secret_hash_key") => {
                RepositoryErrorKind::Conflict(RepositoryConflict::TokenSecret)
            }
            _ => RepositoryErrorKind::Unexpected,
        },
        _ => RepositoryErrorKind::Unexpected,
    };
    RepositoryError::with_source(kind, error)
}

fn fixed_hash(bytes: Vec<u8>, field: &str) -> Result<SecretHash, RepositoryError> {
    bytes
        .try_into()
        .map(SecretHash::new)
        .map_err(|bytes: Vec<u8>| {
            data_error(format!("{field} is {} bytes instead of 32", bytes.len()))
        })
}

fn checksum(bytes: Vec<u8>) -> Result<ArtifactSha256, RepositoryError> {
    bytes
        .try_into()
        .map(ArtifactSha256::new)
        .map_err(|bytes: Vec<u8>| {
            data_error(format!(
                "artifact checksum is {} bytes instead of 32",
                bytes.len()
            ))
        })
}

fn parse_u64(value: &str, field: &str) -> Result<u64, RepositoryError> {
    value
        .parse()
        .map_err(|_| data_error(format!("{field} is outside the u64 range")))
}

fn nonnegative_u64(value: i64, field: &str) -> Result<u64, RepositoryError> {
    u64::try_from(value).map_err(|_| data_error(format!("{field} is negative")))
}

fn nonnegative_u32(value: i32, field: &str) -> Result<u32, RepositoryError> {
    u32::try_from(value).map_err(|_| data_error(format!("{field} is negative")))
}

fn role(value: &str) -> Result<NamespaceRole, RepositoryError> {
    match value {
        "owner" => Ok(NamespaceRole::Owner),
        "maintainer" => Ok(NamespaceRole::Maintainer),
        _ => Err(data_error(format!("unknown namespace role {value:?}"))),
    }
}

const fn role_text(value: NamespaceRole) -> &'static str {
    match value {
        NamespaceRole::Owner => "owner",
        NamespaceRole::Maintainer => "maintainer",
    }
}

fn scope(value: &str) -> Result<TokenScope, RepositoryError> {
    match value {
        "publish" => Ok(TokenScope::Publish),
        "yank" => Ok(TokenScope::Yank),
        "namespace" => Ok(TokenScope::Namespace),
        _ => Err(data_error(format!("unknown token scope {value:?}"))),
    }
}

const fn scope_text(value: TokenScope) -> &'static str {
    match value {
        TokenScope::Publish => "publish",
        TokenScope::Yank => "yank",
        TokenScope::Namespace => "namespace",
    }
}

fn package_kind(value: &str) -> Result<PackageKind, RepositoryError> {
    match value {
        "program" => Ok(PackageKind::Program),
        "library" => Ok(PackageKind::Library),
        "source" => Ok(PackageKind::Source),
        _ => Err(data_error(format!("unknown package type {value:?}"))),
    }
}

const fn package_kind_text(value: PackageKind) -> &'static str {
    match value {
        PackageKind::Program => "program",
        PackageKind::Library => "library",
        PackageKind::Source => "source",
    }
}

fn user_record(row: UserRow) -> Result<UserRecord, RepositoryError> {
    Ok(UserRecord {
        id: UserId::new(row.id),
        github_user_id: row
            .github_user_id
            .as_deref()
            .map(|id| parse_u64(id, "github user id"))
            .transpose()?,
        github_login: row.github_login,
        display_name: row.display_name,
        avatar_url: row.avatar_url,
        created_at: row.created_at,
        updated_at: row.updated_at,
        anonymized_at: row.anonymized_at,
    })
}

fn session_record(row: SessionRow) -> Result<SessionRecord, RepositoryError> {
    Ok(SessionRecord {
        id: SessionId::new(row.id),
        user_id: UserId::new(row.user_id),
        secret_hash: fixed_hash(row.secret_hash, "session secret hash")?,
        csrf_hash: fixed_hash(row.csrf_hash, "session csrf hash")?,
        created_at: row.created_at,
        last_seen_at: row.last_seen_at,
        expires_at: row.expires_at,
        revoked_at: row.revoked_at,
    })
}

fn namespace_record(row: NamespaceRow) -> Result<NamespaceRecord, RepositoryError> {
    Ok(NamespaceRecord {
        id: NamespaceId::new(row.id),
        name: IdentitySegment::new(row.display_name).map_err(|error| {
            RepositoryError::with_source(RepositoryErrorKind::CorruptData, error)
        })?,
        created_by_user_id: row.created_by_user_id.map(UserId::new),
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn owner_record(row: &OwnerRow) -> Result<NamespaceOwnerRecord, RepositoryError> {
    Ok(NamespaceOwnerRecord {
        namespace_id: NamespaceId::new(row.namespace_id),
        user_id: UserId::new(row.user_id),
        role: role(&row.role)?,
        added_by_user_id: row.added_by_user_id.map(UserId::new),
        created_at: row.created_at,
    })
}

fn invitation_record(row: &InvitationRow) -> Result<InvitationRecord, RepositoryError> {
    Ok(InvitationRecord {
        id: InvitationId::new(row.id),
        namespace_id: NamespaceId::new(row.namespace_id),
        invited_user_id: UserId::new(row.invited_user_id),
        invited_by_user_id: row.invited_by_user_id.map(UserId::new),
        role: role(&row.role)?,
        created_at: row.created_at,
        expires_at: row.expires_at,
        accepted_at: row.accepted_at,
        revoked_at: row.revoked_at,
    })
}

fn membership_record(row: MembershipRow) -> Result<NamespaceMembershipRecord, RepositoryError> {
    Ok(NamespaceMembershipRecord {
        namespace: namespace_record(NamespaceRow {
            id: row.namespace_id,
            display_name: row.namespace_display_name,
            created_by_user_id: row.namespace_created_by_user_id,
            created_at: row.namespace_created_at,
            updated_at: row.namespace_updated_at,
        })?,
        membership: owner_record(&OwnerRow {
            namespace_id: row.namespace_id,
            user_id: row.user_id,
            role: row.role,
            added_by_user_id: row.added_by_user_id,
            created_at: row.membership_created_at,
        })?,
    })
}

fn member_record(row: MemberRow) -> Result<NamespaceMemberRecord, RepositoryError> {
    Ok(NamespaceMemberRecord {
        membership: owner_record(&OwnerRow {
            namespace_id: row.namespace_id,
            user_id: row.user_id,
            role: row.role,
            added_by_user_id: row.added_by_user_id,
            created_at: row.membership_created_at,
        })?,
        user: user_record(UserRow {
            id: row.user_id,
            github_user_id: row.github_user_id,
            github_login: row.github_login,
            display_name: row.display_name,
            avatar_url: row.avatar_url,
            created_at: row.user_created_at,
            updated_at: row.user_updated_at,
            anonymized_at: row.anonymized_at,
        })?,
    })
}

fn invitation_details_record(
    row: InvitationDetailsRow,
) -> Result<NamespaceInvitationRecord, RepositoryError> {
    let invited_by_user = match row.inviter_id {
        Some(id) => Some(user_record(UserRow {
            id,
            github_user_id: row.inviter_github_user_id,
            github_login: row.inviter_github_login,
            display_name: row.inviter_display_name,
            avatar_url: row.inviter_avatar_url,
            created_at: row
                .inviter_created_at
                .ok_or_else(|| data_error("inviter creation timestamp is missing"))?,
            updated_at: row
                .inviter_updated_at
                .ok_or_else(|| data_error("inviter update timestamp is missing"))?,
            anonymized_at: row.inviter_anonymized_at,
        })?),
        None => None,
    };
    Ok(NamespaceInvitationRecord {
        invitation: invitation_record(&InvitationRow {
            id: row.invitation_id,
            namespace_id: row.namespace_id,
            invited_user_id: row.invited_user_id,
            invited_by_user_id: row.invited_by_user_id,
            role: row.role,
            created_at: row.invitation_created_at,
            expires_at: row.expires_at,
            accepted_at: row.accepted_at,
            revoked_at: row.revoked_at,
        })?,
        namespace: namespace_record(NamespaceRow {
            id: row.namespace_id,
            display_name: row.namespace_display_name,
            created_by_user_id: row.namespace_created_by_user_id,
            created_at: row.namespace_created_at,
            updated_at: row.namespace_updated_at,
        })?,
        invited_user: user_record(UserRow {
            id: row.invited_user_id,
            github_user_id: row.invited_github_user_id,
            github_login: row.invited_github_login,
            display_name: row.invited_display_name,
            avatar_url: row.invited_avatar_url,
            created_at: row.invited_created_at,
            updated_at: row.invited_updated_at,
            anonymized_at: row.invited_anonymized_at,
        })?,
        invited_by_user,
    })
}

fn dashboard_user(
    github_login: Option<String>,
    display_name: Option<String>,
    avatar_url: Option<String>,
) -> Option<DashboardUser> {
    github_login.map(|github_login| DashboardUser {
        github_login,
        display_name,
        avatar_url,
    })
}

fn dashboard_namespace(row: DashboardNamespaceRow) -> Result<DashboardNamespace, RepositoryError> {
    Ok(DashboardNamespace {
        namespace: dashboard_identity(row.namespace_display_name, "dashboard namespace")?,
        role: role(&row.role)?,
        package_count: nonnegative_u64(row.package_count, "dashboard namespace package count")?,
    })
}

fn dashboard_package(row: DashboardPackageRow) -> Result<DashboardPackage, RepositoryError> {
    Ok(DashboardPackage {
        namespace: dashboard_identity(row.namespace_display_name, "dashboard package namespace")?,
        package: dashboard_identity(row.package_display_name, "dashboard package")?,
        version: dashboard_version(row.version, "dashboard package version")?,
        published_at: row.published_at,
        yanked: row.yanked,
        version_count: nonnegative_u64(row.version_count, "dashboard package version count")?,
    })
}

fn dashboard_invitation(
    row: DashboardInvitationRow,
) -> Result<DashboardInvitation, RepositoryError> {
    Ok(DashboardInvitation {
        namespace: dashboard_identity(
            row.namespace_display_name,
            "dashboard invitation namespace",
        )?,
        invited_by: dashboard_user(
            row.inviter_github_login,
            row.inviter_display_name,
            row.inviter_avatar_url,
        ),
        role: role(&row.role)?,
        created_at: row.created_at,
        expires_at: row.expires_at,
    })
}

fn dashboard_activity(row: DashboardActivityRow) -> Result<DashboardActivity, RepositoryError> {
    Ok(DashboardActivity {
        kind: dashboard_activity_kind(&row.action)?,
        actor: dashboard_user(
            row.actor_github_login,
            row.actor_display_name,
            row.actor_avatar_url,
        ),
        namespace: dashboard_identity(row.namespace_display_name, "dashboard activity namespace")?,
        package: row
            .package_display_name
            .map(|value| dashboard_identity(value, "dashboard activity package"))
            .transpose()?,
        version: row
            .version
            .map(|value| dashboard_version(value, "dashboard activity version"))
            .transpose()?,
        target_user: dashboard_user(
            row.target_github_login,
            row.target_display_name,
            row.target_avatar_url,
        ),
        previous_role: row.previous_role.as_deref().map(role).transpose()?,
        role: row.role.as_deref().map(role).transpose()?,
        occurred_at: row.occurred_at,
    })
}

fn dashboard_download_leader(
    row: DashboardDownloadLeaderRow,
) -> Result<DashboardDownloadLeader, RepositoryError> {
    Ok(DashboardDownloadLeader {
        namespace: dashboard_identity(row.namespace_display_name, "download leader namespace")?,
        package: dashboard_identity(row.package_display_name, "download leader package")?,
        downloads_30d: nonnegative_u64(row.downloads_30d, "download leader count")?,
    })
}

fn dashboard_identity(value: String, field: &str) -> Result<IdentitySegment, RepositoryError> {
    IdentitySegment::new(value).map_err(|error| {
        RepositoryError::with_source(
            RepositoryErrorKind::CorruptData,
            StoredDataError(format!("invalid {field}: {error}")),
        )
    })
}

fn dashboard_version(value: String, field: &str) -> Result<SemanticVersion, RepositoryError> {
    SemanticVersion::new(value).map_err(|error| {
        RepositoryError::with_source(
            RepositoryErrorKind::CorruptData,
            StoredDataError(format!("invalid {field}: {error}")),
        )
    })
}

fn dashboard_activity_kind(value: &str) -> Result<DashboardActivityKind, RepositoryError> {
    match value {
        "namespace_created" => Ok(DashboardActivityKind::NamespaceCreated),
        "namespace_member_role_changed" => Ok(DashboardActivityKind::NamespaceMemberRoleChanged),
        "namespace_member_removed" => Ok(DashboardActivityKind::NamespaceMemberRemoved),
        "namespace_invitation_created" => Ok(DashboardActivityKind::NamespaceInvitationCreated),
        "namespace_invitation_accepted" => Ok(DashboardActivityKind::NamespaceInvitationAccepted),
        "namespace_invitation_revoked" => Ok(DashboardActivityKind::NamespaceInvitationRevoked),
        "package_version_published" => Ok(DashboardActivityKind::PackageVersionPublished),
        "package_version_yanked" => Ok(DashboardActivityKind::PackageVersionYanked),
        "package_version_unyanked" => Ok(DashboardActivityKind::PackageVersionUnyanked),
        _ => Err(data_error(format!("unknown dashboard activity {value:?}"))),
    }
}

fn package_record(row: PackageRow) -> Result<PackageRecord, RepositoryError> {
    Ok(PackageRecord {
        id: PackageId::new(row.id),
        namespace_id: NamespaceId::new(row.namespace_id),
        name: IdentitySegment::new(row.display_name).map_err(|error| {
            RepositoryError::with_source(RepositoryErrorKind::CorruptData, error)
        })?,
        created_by_user_id: row.created_by_user_id.map(UserId::new),
        created_at: row.created_at,
    })
}

fn package_summary_record(row: PackageSummaryRow) -> Result<PackageSummaryRecord, RepositoryError> {
    Ok(PackageSummaryRecord {
        namespace: stored_identity(row.namespace_display_name)?,
        package: stored_identity(row.package_display_name)?,
        created_at: row.created_at,
    })
}

async fn pending_invitations(
    pool: &PgPool,
    filter: &str,
    id: Uuid,
    at: OffsetDateTime,
) -> Result<Vec<NamespaceInvitationRecord>, RepositoryError> {
    let sql = format!(
        "SELECT i.id AS invitation_id, i.namespace_id, i.invited_user_id,
                i.invited_by_user_id, i.role, i.created_at AS invitation_created_at,
                i.expires_at, i.accepted_at, i.revoked_at,
                n.display_name AS namespace_display_name,
                n.created_by_user_id AS namespace_created_by_user_id,
                n.created_at AS namespace_created_at, n.updated_at AS namespace_updated_at,
                invited.github_user_id::text AS invited_github_user_id,
                invited.github_login AS invited_github_login,
                invited.display_name AS invited_display_name,
                invited.avatar_url AS invited_avatar_url,
                invited.created_at AS invited_created_at,
                invited.updated_at AS invited_updated_at,
                invited.anonymized_at AS invited_anonymized_at,
                inviter.id AS inviter_id,
                inviter.github_user_id::text AS inviter_github_user_id,
                inviter.github_login AS inviter_github_login,
                inviter.display_name AS inviter_display_name,
                inviter.avatar_url AS inviter_avatar_url,
                inviter.created_at AS inviter_created_at,
                inviter.updated_at AS inviter_updated_at,
                inviter.anonymized_at AS inviter_anonymized_at
         FROM namespace_invitations i
         JOIN namespaces n ON n.id = i.namespace_id
         JOIN users invited ON invited.id = i.invited_user_id
             AND invited.anonymized_at IS NULL
         LEFT JOIN users inviter ON inviter.id = i.invited_by_user_id
             AND inviter.anonymized_at IS NULL
         WHERE {filter} AND i.accepted_at IS NULL AND i.revoked_at IS NULL
             AND i.expires_at > $2
         ORDER BY i.created_at DESC, i.id DESC"
    );
    query_as::<_, InvitationDetailsRow>(&sql)
        .bind(id)
        .bind(at)
        .fetch_all(pool)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .map(invitation_details_record)
        .collect()
}

async fn token_scopes<'e, E>(executor: E, id: Uuid) -> Result<Vec<TokenScope>, RepositoryError>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    query("SELECT scope FROM api_token_scopes WHERE api_token_id = $1 ORDER BY CASE scope WHEN 'publish' THEN 1 WHEN 'yank' THEN 2 WHEN 'namespace' THEN 3 END")
        .bind(id)
        .fetch_all(executor)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .map(|row| scope(row.get::<&str, _>("scope")))
        .collect()
}

async fn token_record_pool(
    pool: &PgPool,
    row: TokenRow,
) -> Result<ApiTokenRecord, RepositoryError> {
    let scopes = token_scopes(pool, row.id).await?;
    Ok(ApiTokenRecord {
        id: ApiTokenId::new(row.id),
        user_id: UserId::new(row.user_id),
        display_name: row.display_name,
        token_prefix: row.token_prefix,
        secret_hash: fixed_hash(row.secret_hash, "token secret hash")?,
        scopes,
        created_at: row.created_at,
        last_used_at: row.last_used_at,
        expires_at: row.expires_at,
        revoked_at: row.revoked_at,
    })
}

fn json_object(
    value: Value,
    field: &str,
) -> Result<serde_json::Map<String, Value>, RepositoryError> {
    match value {
        Value::Object(object) => Ok(object),
        _ => Err(data_error(format!("{field} is not an object"))),
    }
}

async fn version_record(
    pool: &PgPool,
    row: VersionRow,
) -> Result<PackageVersionRecord, RepositoryError> {
    let authors = query(
        "SELECT author FROM package_version_authors WHERE package_version_id = $1 ORDER BY ordinal",
    )
    .bind(row.id)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx)?
    .into_iter()
    .map(|item| item.get::<String, _>("author"))
    .collect();
    let keywords = query("SELECT display_name FROM package_version_keywords WHERE package_version_id = $1 ORDER BY ordinal")
        .bind(row.id).fetch_all(pool).await.map_err(map_sqlx)?
        .into_iter().map(|item| IdentitySegment::new(item.get::<String, _>("display_name")).map_err(|error| RepositoryError::with_source(RepositoryErrorKind::CorruptData, error))).collect::<Result<Vec<_>, _>>()?;
    let dependencies = query("SELECT display_alias, target_namespace_display_name, target_package_display_name, version_range FROM dependencies WHERE package_version_id = $1 ORDER BY normalized_alias")
        .bind(row.id).fetch_all(pool).await.map_err(map_sqlx)?
        .into_iter().map(|item| {
            Ok(DependencyRecord {
                alias: IdentitySegment::new(item.get::<String, _>("display_alias")).map_err(|error| RepositoryError::with_source(RepositoryErrorKind::CorruptData, error))?,
                target_namespace: IdentitySegment::new(item.get::<String, _>("target_namespace_display_name")).map_err(|error| RepositoryError::with_source(RepositoryErrorKind::CorruptData, error))?,
                target_package: IdentitySegment::new(item.get::<String, _>("target_package_display_name")).map_err(|error| RepositoryError::with_source(RepositoryErrorKind::CorruptData, error))?,
                version_range: VersionRange::new(item.get::<String, _>("version_range")).map_err(|error| RepositoryError::with_source(RepositoryErrorKind::CorruptData, error))?,
            })
        }).collect::<Result<Vec<_>, RepositoryError>>()?;
    let readme_file = pair(row.readme_file_path, row.readme_file_text, "readme_file")?;
    let license_file = pair(row.license_file_path, row.license_file_text, "license file")?;
    Ok(PackageVersionRecord {
        id: PackageVersionId::new(row.id),
        package_id: PackageId::new(row.package_id),
        version: SemanticVersion::new(row.version).map_err(|error| {
            RepositoryError::with_source(RepositoryErrorKind::CorruptData, error)
        })?,
        manifest_schema_version: u16::try_from(row.manifest_schema_version)
            .map_err(|_| data_error("negative manifest schema version"))?,
        min_rux: SemanticVersion::new(row.min_rux).map_err(|error| {
            RepositoryError::with_source(RepositoryErrorKind::CorruptData, error)
        })?,
        package_type: package_kind(&row.package_type)?,
        description: row.description,
        repository_url: row.repository_url,
        homepage_url: row.homepage_url,
        readme_file,
        license_expression: row.license_expression,
        license_file,
        normalized_manifest: json_object(row.normalized_manifest, "normalized manifest")?,
        artifact_sha256: checksum(row.artifact_sha256)?,
        artifact_size: nonnegative_u64(row.artifact_size, "artifact size")?,
        storage_key: row.storage_key,
        artifact_file_count: nonnegative_u32(row.artifact_file_count, "artifact file count")?,
        artifact_expanded_bytes: nonnegative_u64(
            row.artifact_expanded_bytes,
            "artifact expanded bytes",
        )?,
        source_file_count: nonnegative_u32(row.source_file_count, "source file count")?,
        source_line_count: nonnegative_u64(row.source_line_count, "source line count")?,
        published_by_user_id: row.published_by_user_id.map(UserId::new),
        published_at: row.published_at,
        yanked_at: row.yanked_at,
        yanked_by_user_id: row.yanked_by_user_id.map(UserId::new),
        authors,
        keywords,
        dependencies,
    })
}

fn package_version_metadata_record(
    summary: PackageSummaryRecord,
    version: PackageVersionRecord,
) -> PackageVersionMetadataRecord {
    PackageVersionMetadataRecord {
        namespace: summary.namespace,
        package: summary.package,
        version: version.version,
        manifest_schema_version: version.manifest_schema_version,
        min_rux: version.min_rux,
        package_type: version.package_type,
        description: version.description,
        repository_url: version.repository_url,
        homepage_url: version.homepage_url,
        readme_file: version.readme_file,
        license_expression: version.license_expression,
        license_file: version.license_file,
        normalized_manifest: version.normalized_manifest,
        artifact_sha256: version.artifact_sha256,
        artifact_size: version.artifact_size,
        artifact_file_count: version.artifact_file_count,
        artifact_expanded_bytes: version.artifact_expanded_bytes,
        source_file_count: version.source_file_count,
        source_line_count: version.source_line_count,
        published_at: version.published_at,
        yanked: version.yanked_at.is_some(),
        authors: version.authors,
        keywords: version.keywords,
        dependencies: version.dependencies,
    }
}

fn pair(
    path: Option<String>,
    text: Option<String>,
    name: &str,
) -> Result<Option<(String, String)>, RepositoryError> {
    match (path, text) {
        (None, None) => Ok(None),
        (Some(path), Some(text)) => Ok(Some((path, text))),
        _ => Err(data_error(format!(
            "{name} path/text state is inconsistent"
        ))),
    }
}

#[async_trait]
impl AccountReader for PostgresRepository {
    async fn user_by_id(&self, id: UserId) -> Result<Option<UserRecord>, RepositoryError> {
        query_as::<_, UserRow>(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = $1"))
            .bind(id.get())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?
            .map(user_record)
            .transpose()
    }

    async fn user_by_github_id(&self, id: u64) -> Result<Option<UserRecord>, RepositoryError> {
        query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM users WHERE github_user_id = $1::numeric"
        ))
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .map(user_record)
        .transpose()
    }

    async fn user_by_github_login(
        &self,
        login: &str,
    ) -> Result<Option<UserRecord>, RepositoryError> {
        query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM users WHERE normalized_github_login = lower($1)"
        ))
        .bind(login)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .map(user_record)
        .transpose()
    }

    async fn session_by_secret_hash(
        &self,
        hash: SecretHash,
    ) -> Result<Option<SessionRecord>, RepositoryError> {
        query_as::<_, SessionRow>(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE secret_hash = $1"
        ))
        .bind(hash.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .map(session_record)
        .transpose()
    }
}

#[async_trait]
impl NamespaceReader for PostgresRepository {
    async fn namespace_by_name(
        &self,
        name: &IdentitySegment,
    ) -> Result<Option<NamespaceRecord>, RepositoryError> {
        query_as::<_, NamespaceRow>(&format!(
            "SELECT {NAMESPACE_COLUMNS} FROM namespaces WHERE normalized_name = $1"
        ))
        .bind(name.normalized())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .map(namespace_record)
        .transpose()
    }

    async fn namespace_role(
        &self,
        namespace_id: NamespaceId,
        user_id: UserId,
    ) -> Result<Option<NamespaceOwnerRecord>, RepositoryError> {
        query_as::<_, OwnerRow>("SELECT namespace_id, user_id, role, added_by_user_id, created_at FROM namespace_owners WHERE namespace_id = $1 AND user_id = $2").bind(namespace_id.get()).bind(user_id.get()).fetch_optional(&self.pool).await.map_err(map_sqlx)?.map(|row| owner_record(&row)).transpose()
    }

    async fn invitation_by_id(
        &self,
        id: InvitationId,
    ) -> Result<Option<InvitationRecord>, RepositoryError> {
        query_as::<_, InvitationRow>(&format!(
            "SELECT {INVITATION_COLUMNS} FROM namespace_invitations WHERE id = $1"
        ))
        .bind(id.get())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .map(|row| invitation_record(&row))
        .transpose()
    }

    async fn namespaces_by_user_id(
        &self,
        user_id: UserId,
    ) -> Result<Vec<NamespaceMembershipRecord>, RepositoryError> {
        query_as::<_, MembershipRow>(
            "SELECT n.id AS namespace_id, n.display_name AS namespace_display_name,
                    n.created_by_user_id AS namespace_created_by_user_id,
                    n.created_at AS namespace_created_at, n.updated_at AS namespace_updated_at,
                    o.user_id, o.role, o.added_by_user_id,
                    o.created_at AS membership_created_at
             FROM namespace_owners o
             JOIN namespaces n ON n.id = o.namespace_id
             WHERE o.user_id = $1
             ORDER BY n.normalized_name",
        )
        .bind(user_id.get())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .map(membership_record)
        .collect()
    }

    async fn namespace_members(
        &self,
        namespace_id: NamespaceId,
    ) -> Result<Vec<NamespaceMemberRecord>, RepositoryError> {
        query_as::<_, MemberRow>(
            "SELECT o.namespace_id, o.user_id, o.role, o.added_by_user_id,
                    o.created_at AS membership_created_at,
                    u.github_user_id::text AS github_user_id, u.github_login,
                    u.display_name, u.avatar_url, u.created_at AS user_created_at,
                    u.updated_at AS user_updated_at, u.anonymized_at
             FROM namespace_owners o
             JOIN users u ON u.id = o.user_id
             WHERE o.namespace_id = $1 AND u.anonymized_at IS NULL
             ORDER BY CASE o.role WHEN 'owner' THEN 0 ELSE 1 END,
                      u.normalized_github_login",
        )
        .bind(namespace_id.get())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .map(member_record)
        .collect()
    }

    async fn pending_invitations_by_user_id(
        &self,
        user_id: UserId,
        at: OffsetDateTime,
    ) -> Result<Vec<NamespaceInvitationRecord>, RepositoryError> {
        pending_invitations(&self.pool, "i.invited_user_id = $1", user_id.get(), at).await
    }

    async fn pending_invitations_by_namespace(
        &self,
        namespace_id: NamespaceId,
        at: OffsetDateTime,
    ) -> Result<Vec<NamespaceInvitationRecord>, RepositoryError> {
        pending_invitations(&self.pool, "i.namespace_id = $1", namespace_id.get(), at).await
    }
}

#[async_trait]
impl TokenReader for PostgresRepository {
    async fn token_by_secret_hash(
        &self,
        hash: SecretHash,
    ) -> Result<Option<ApiTokenRecord>, RepositoryError> {
        let row = query_as::<_, TokenRow>(&format!(
            "SELECT {TOKEN_COLUMNS} FROM api_tokens WHERE secret_hash = $1"
        ))
        .bind(hash.as_bytes().as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?;
        match row {
            Some(row) => token_record_pool(&self.pool, row).await.map(Some),
            None => Ok(None),
        }
    }

    async fn tokens_by_user_id(
        &self,
        user_id: UserId,
    ) -> Result<Vec<ApiTokenRecord>, RepositoryError> {
        let rows = query_as::<_, TokenRow>(&format!(
            "SELECT {TOKEN_COLUMNS} FROM api_tokens WHERE user_id = $1 ORDER BY created_at DESC, id DESC"
        ))
        .bind(user_id.get())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        let mut tokens = Vec::with_capacity(rows.len());
        for row in rows {
            tokens.push(token_record_pool(&self.pool, row).await?);
        }
        Ok(tokens)
    }
}

#[async_trait]
impl CatalogReader for PostgresRepository {
    async fn package_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
    ) -> Result<Option<PackageRecord>, RepositoryError> {
        query_as::<_, PackageRow>(&format!("SELECT {QUALIFIED_PACKAGE_COLUMNS} FROM packages p JOIN namespaces n ON n.id = p.namespace_id WHERE n.normalized_name = $1 AND p.normalized_name = $2"))
            .bind(namespace.normalized()).bind(package.normalized()).fetch_optional(&self.pool).await.map_err(map_sqlx)?.map(package_record).transpose()
    }

    async fn version_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Result<Option<PackageVersionRecord>, RepositoryError> {
        let sql = format!(
            "SELECT {VERSION_COLUMNS} FROM package_versions v JOIN packages p ON p.id = v.package_id JOIN namespaces n ON n.id = p.namespace_id WHERE n.normalized_name = $1 AND p.normalized_name = $2 AND v.version = $3"
        );
        let row = query_as::<_, VersionRow>(&sql)
            .bind(namespace.normalized())
            .bind(package.normalized())
            .bind(version.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_sqlx)?;
        match row {
            Some(row) => version_record(&self.pool, row).await.map(Some),
            None => Ok(None),
        }
    }
}

#[async_trait]
impl PackageMetadataReader for PostgresRepository {
    async fn package_summary_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
    ) -> Result<Option<PackageSummaryRecord>, RepositoryError> {
        query_as::<_, PackageSummaryRow>(
            "SELECT n.display_name AS namespace_display_name,
                    p.display_name AS package_display_name,
                    p.created_at
             FROM packages p
             JOIN namespaces n ON n.id = p.namespace_id
             WHERE n.normalized_name = $1 AND p.normalized_name = $2",
        )
        .bind(namespace.normalized())
        .bind(package.normalized())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)?
        .map(package_summary_record)
        .transpose()
    }

    async fn package_version_metadata_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Result<Option<PackageVersionMetadataRecord>, RepositoryError> {
        let Some(summary) = self.package_summary_by_name(namespace, package).await? else {
            return Ok(None);
        };
        let Some(version) = self.version_by_name(namespace, package, version).await? else {
            return Ok(None);
        };
        Ok(Some(package_version_metadata_record(summary, version)))
    }
}

#[async_trait]
impl PackageSearchReader for PostgresRepository {
    async fn search_packages(
        &self,
        criteria: &PackageSearchCriteria,
        boundary: Option<&PackageSearchBoundary>,
        limit: u16,
    ) -> Result<Vec<PackageSearchRecord>, RepositoryError> {
        let namespace = criteria.namespace.as_ref().map(IdentitySegment::normalized);
        let keyword = criteria.keyword.as_ref().map(IdentitySegment::normalized);
        let package_type = criteria.package_type.map(package_kind_name);
        let pattern = criteria.identity_query.as_deref().map(literal_pattern);
        let boundary_class = boundary.map(|boundary| i32::from(boundary.match_class));
        let boundary_relevance = boundary.map(|boundary| boundary.relevance);
        let boundary_namespace = boundary.map(|boundary| boundary.namespace.as_str());
        let boundary_package = boundary.map(|boundary| boundary.package.as_str());
        let rows = query_as::<_, PackageSearchRow>(PACKAGE_SEARCH_SQL)
            .bind(criteria.query.as_deref())
            .bind(criteria.identity_query.as_deref())
            .bind(pattern.as_deref())
            .bind(namespace)
            .bind(keyword)
            .bind(package_type)
            .bind(boundary_class)
            .bind(boundary_relevance)
            .bind(boundary_namespace)
            .bind(boundary_package)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx)?;

        rows.into_iter().map(package_search_record).collect()
    }
}

async fn dashboard_counts(
    pool: &PgPool,
    user_id: UserId,
    at: OffsetDateTime,
) -> Result<DashboardCountsRow, RepositoryError> {
    query_as(
        "SELECT
             (SELECT count(*) FROM namespace_owners WHERE user_id = $1) AS namespaces,
             (SELECT count(*) FROM packages p
                JOIN namespace_owners o ON o.namespace_id = p.namespace_id
               WHERE o.user_id = $1) AS packages,
             (SELECT count(*) FROM namespace_invitations i
               WHERE i.invited_user_id = $1
                 AND i.accepted_at IS NULL AND i.revoked_at IS NULL
                 AND i.expires_at > $2) AS invitations",
    )
    .bind(user_id.get())
    .bind(at)
    .fetch_one(pool)
    .await
    .map_err(map_sqlx)
}

async fn dashboard_namespaces(
    pool: &PgPool,
    user_id: UserId,
) -> Result<Vec<DashboardNamespaceRow>, RepositoryError> {
    query_as(
        "SELECT n.display_name AS namespace_display_name, o.role, count(p.id) AS package_count
           FROM namespace_owners o
           JOIN namespaces n ON n.id = o.namespace_id
           LEFT JOIN packages p ON p.namespace_id = n.id
          WHERE o.user_id = $1
          GROUP BY n.id, n.display_name, n.normalized_name, o.role
          ORDER BY n.normalized_name",
    )
    .bind(user_id.get())
    .fetch_all(pool)
    .await
    .map_err(map_sqlx)
}

async fn dashboard_packages(
    pool: &PgPool,
    user_id: UserId,
    limit: u16,
) -> Result<Vec<DashboardPackageRow>, RepositoryError> {
    query_as(
        "SELECT n.display_name AS namespace_display_name,
                p.display_name AS package_display_name,
                latest.version, latest.published_at,
                latest.yanked_at IS NOT NULL AS yanked, versions.version_count
           FROM namespace_owners o
           JOIN namespaces n ON n.id = o.namespace_id
           JOIN packages p ON p.namespace_id = n.id
           JOIN LATERAL (
                SELECT pv.version, pv.published_at, pv.yanked_at
                  FROM package_versions pv WHERE pv.package_id = p.id
                 ORDER BY pv.published_at DESC, pv.id DESC LIMIT 1
           ) latest ON TRUE
           JOIN LATERAL (
                SELECT count(*) AS version_count
                  FROM package_versions pv WHERE pv.package_id = p.id
           ) versions ON TRUE
          WHERE o.user_id = $1
          ORDER BY latest.published_at DESC, n.normalized_name, p.normalized_name
          LIMIT $2",
    )
    .bind(user_id.get())
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    .map_err(map_sqlx)
}

async fn dashboard_invitations(
    pool: &PgPool,
    user_id: UserId,
    at: OffsetDateTime,
) -> Result<Vec<DashboardInvitationRow>, RepositoryError> {
    query_as(
        "SELECT n.display_name AS namespace_display_name,
                inviter.github_login AS inviter_github_login,
                inviter.display_name AS inviter_display_name,
                inviter.avatar_url AS inviter_avatar_url,
                i.role, i.created_at, i.expires_at
           FROM namespace_invitations i
           JOIN namespaces n ON n.id = i.namespace_id
           LEFT JOIN users inviter
             ON inviter.id = i.invited_by_user_id AND inviter.anonymized_at IS NULL
          WHERE i.invited_user_id = $1
            AND i.accepted_at IS NULL AND i.revoked_at IS NULL
            AND i.expires_at > $2
          ORDER BY i.created_at DESC, i.id DESC",
    )
    .bind(user_id.get())
    .bind(at)
    .fetch_all(pool)
    .await
    .map_err(map_sqlx)
}

async fn dashboard_activity_rows(
    pool: &PgPool,
    user_id: UserId,
    limit: u16,
) -> Result<Vec<DashboardActivityRow>, RepositoryError> {
    query_as(
        "SELECT a.action, n.display_name AS namespace_display_name,
                actor.github_login AS actor_github_login,
                actor.display_name AS actor_display_name,
                actor.avatar_url AS actor_avatar_url,
                a.metadata ->> 'package' AS package_display_name,
                a.metadata ->> 'version' AS version,
                target.github_login AS target_github_login,
                target.display_name AS target_display_name,
                target.avatar_url AS target_avatar_url,
                a.metadata ->> 'previous_role' AS previous_role,
                a.metadata ->> 'role' AS role, a.occurred_at
           FROM namespace_owners o
           JOIN namespaces n ON n.id = o.namespace_id
           JOIN audit_records a ON a.namespace_key = n.normalized_name
           LEFT JOIN users actor ON actor.id = a.actor_user_id AND actor.anonymized_at IS NULL
           LEFT JOIN users target
             ON target.id = CASE WHEN a.metadata ->> 'target_user_id' ~
                       '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                  THEN (a.metadata ->> 'target_user_id')::UUID ELSE NULL END
            AND target.anonymized_at IS NULL
          WHERE o.user_id = $1 AND a.action = ANY($2::TEXT[])
            AND (o.role = 'owner' OR a.action = ANY($3::TEXT[])
                 OR a.actor_user_id = $1
                 OR a.metadata ->> 'target_user_id' = $1::TEXT)
          ORDER BY a.occurred_at DESC, a.id DESC LIMIT $4",
    )
    .bind(user_id.get())
    .bind([
        "namespace_created",
        "namespace_member_role_changed",
        "namespace_member_removed",
        "namespace_invitation_created",
        "namespace_invitation_accepted",
        "namespace_invitation_revoked",
        "package_version_published",
        "package_version_yanked",
        "package_version_unyanked",
    ])
    .bind([
        "namespace_created",
        "package_version_published",
        "package_version_yanked",
        "package_version_unyanked",
    ])
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    .map_err(map_sqlx)
}

async fn dashboard_download_totals(
    pool: &PgPool,
    user_id: UserId,
    start: OffsetDateTime,
    end: OffsetDateTime,
) -> Result<DashboardDownloadTotalsRow, RepositoryError> {
    query_as(
        "SELECT count(*) FILTER (
                    WHERE d.occurred_at >= $2 AND d.occurred_at <= $3
                ) AS total_30d,
                count(*) FILTER (WHERE d.occurred_at <= $3) AS total_all_time
           FROM namespace_owners o
           JOIN packages p ON p.namespace_id = o.namespace_id
           JOIN package_versions pv ON pv.package_id = p.id
           JOIN download_events d ON d.package_version_id = pv.id
          WHERE o.user_id = $1",
    )
    .bind(user_id.get())
    .bind(start)
    .bind(end)
    .fetch_one(pool)
    .await
    .map_err(map_sqlx)
}

async fn dashboard_download_leaders(
    pool: &PgPool,
    user_id: UserId,
    start: OffsetDateTime,
    end: OffsetDateTime,
    limit: u16,
) -> Result<Vec<DashboardDownloadLeaderRow>, RepositoryError> {
    query_as(
        "SELECT n.display_name AS namespace_display_name,
                p.display_name AS package_display_name, count(*) AS downloads_30d
           FROM namespace_owners o
           JOIN namespaces n ON n.id = o.namespace_id
           JOIN packages p ON p.namespace_id = n.id
           JOIN package_versions pv ON pv.package_id = p.id
           JOIN download_events d ON d.package_version_id = pv.id
          WHERE o.user_id = $1 AND d.occurred_at >= $2 AND d.occurred_at <= $3
          GROUP BY n.id, n.display_name, n.normalized_name,
                   p.id, p.display_name, p.normalized_name
          ORDER BY downloads_30d DESC, n.normalized_name, p.normalized_name
          LIMIT $4",
    )
    .bind(user_id.get())
    .bind(start)
    .bind(end)
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await
    .map_err(map_sqlx)
}

#[async_trait]
impl DashboardReader for PostgresRepository {
    async fn dashboard_snapshot(
        &self,
        user_id: UserId,
        window_start: OffsetDateTime,
        window_end: OffsetDateTime,
        package_limit: u16,
        activity_limit: u16,
        download_leader_limit: u16,
    ) -> Result<DashboardSnapshot, RepositoryError> {
        let counts = dashboard_counts(&self.pool, user_id, window_end).await?;
        let namespace_rows = dashboard_namespaces(&self.pool, user_id).await?;
        let package_rows = dashboard_packages(&self.pool, user_id, package_limit).await?;
        let invitation_rows = dashboard_invitations(&self.pool, user_id, window_end).await?;

        let activity_rows = dashboard_activity_rows(&self.pool, user_id, activity_limit).await?;
        let download_totals =
            dashboard_download_totals(&self.pool, user_id, window_start, window_end).await?;
        let download_leader_rows = dashboard_download_leaders(
            &self.pool,
            user_id,
            window_start,
            window_end,
            download_leader_limit,
        )
        .await?;

        Ok(DashboardSnapshot {
            namespace_count: nonnegative_u64(counts.namespaces, "dashboard namespace count")?,
            package_count: nonnegative_u64(counts.packages, "dashboard package count")?,
            invitation_count: nonnegative_u64(counts.invitations, "dashboard invitation count")?,
            namespaces: namespace_rows
                .into_iter()
                .map(dashboard_namespace)
                .collect::<Result<_, _>>()?,
            packages: package_rows
                .into_iter()
                .map(dashboard_package)
                .collect::<Result<_, _>>()?,
            invitations: invitation_rows
                .into_iter()
                .map(dashboard_invitation)
                .collect::<Result<_, _>>()?,
            activity: activity_rows
                .into_iter()
                .map(dashboard_activity)
                .collect::<Result<_, _>>()?,
            downloads: DashboardDownloads {
                total_30d: nonnegative_u64(
                    download_totals.total_30d,
                    "dashboard 30-day download count",
                )?,
                total_all_time: nonnegative_u64(
                    download_totals.total_all_time,
                    "dashboard all-time download count",
                )?,
                top_packages: download_leader_rows
                    .into_iter()
                    .map(dashboard_download_leader)
                    .collect::<Result<_, _>>()?,
            },
        })
    }
}

#[async_trait]
impl DiscoveryReader for PostgresRepository {
    async fn dependent_packages(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        boundary: Option<&PackageIdentityBoundary>,
        limit: u16,
    ) -> Result<Option<Vec<DependentPackageRecord>>, RepositoryError> {
        if !self.package_exists(namespace, package).await? {
            return Ok(None);
        }
        let sql = format!(
            "WITH {REPRESENTATIVE_CTE}, dependent_page AS (
                 SELECT DISTINCT r.*
                 FROM representative r
                 JOIN dependencies d ON d.package_version_id = r.package_version_id
                 WHERE d.target_namespace_normalized_name = $1
                   AND d.target_package_normalized_name = $2
                   AND ($3::TEXT IS NULL OR
                        (r.namespace_normalized_name, r.package_normalized_name) > ($3, $4))
                 ORDER BY r.namespace_normalized_name, r.package_normalized_name
                 LIMIT $5
             )
             SELECT page.package_version_id,
                    page.namespace_display_name,
                    page.package_display_name,
                    page.version,
                    page.package_type,
                    page.description,
                    page.published_at,
                    page.yanked,
                    d.display_alias AS dependency_alias,
                    d.target_namespace_display_name AS dependency_target_namespace,
                    d.target_package_display_name AS dependency_target_package,
                    d.version_range AS dependency_version_range
             FROM dependent_page page
             JOIN dependencies d ON d.package_version_id = page.package_version_id
             WHERE d.target_namespace_normalized_name = $1
               AND d.target_package_normalized_name = $2
             ORDER BY page.namespace_normalized_name,
                      page.package_normalized_name,
                      d.normalized_alias"
        );
        let rows = query_as::<_, DependentPackageRow>(&sql)
            .bind(namespace.normalized())
            .bind(package.normalized())
            .bind(boundary.map(|value| value.namespace.as_str()))
            .bind(boundary.map(|value| value.package.as_str()))
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx)?;
        dependent_package_records(rows).map(Some)
    }

    async fn keywords(
        &self,
        boundary: Option<&KeywordBoundary>,
        limit: u16,
    ) -> Result<Vec<KeywordRecord>, RepositoryError> {
        let sql = format!(
            "WITH {REPRESENTATIVE_CTE}, keyword_counts AS (
                 SELECT k.normalized_name,
                        (array_agg(
                            k.display_name
                            ORDER BY r.published_at DESC,
                                     r.namespace_normalized_name,
                                     r.package_normalized_name
                        ))[1] AS keyword_display_name,
                        count(*)::BIGINT AS package_count
                 FROM representative r
                 JOIN package_version_keywords k
                   ON k.package_version_id = r.package_version_id
                 GROUP BY k.normalized_name
             )
             SELECT keyword_display_name, package_count
             FROM keyword_counts
             WHERE $1::BIGINT IS NULL
                OR package_count < $1
                OR (package_count = $1 AND normalized_name > $2)
             ORDER BY package_count DESC, normalized_name
             LIMIT $3"
        );
        let boundary_count = boundary
            .map(|value| i64::try_from(value.package_count))
            .transpose()
            .map_err(|_| data_error("keyword package count exceeds PostgreSQL BIGINT"))?;
        let rows = query_as::<_, KeywordDiscoveryRow>(&sql)
            .bind(boundary_count)
            .bind(boundary.map(|value| value.keyword.as_str()))
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx)?;
        rows.into_iter().map(keyword_record).collect()
    }

    async fn package_version_history(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        boundary: Option<&SemanticVersion>,
        limit: u16,
    ) -> Result<Option<Vec<PackageVersionHistoryRecord>>, RepositoryError> {
        if !self.package_exists(namespace, package).await? {
            return Ok(None);
        }
        let sql = "WITH target AS (
                 SELECT p.id
                 FROM packages p
                 JOIN namespaces n ON n.id = p.namespace_id
                 WHERE n.normalized_name = $1 AND p.normalized_name = $2
             ), boundary AS (
                 SELECT major, minor, patch,
                        (prerelease IS NULL) AS stable,
                        coalesce(prerelease_sort_key, ''::BYTEA) AS prerelease_key,
                        (build_metadata IS NOT NULL) AS has_build,
                        coalesce(build_metadata_sort_key, ''::BYTEA) AS build_key
                 FROM package_versions
                 WHERE package_id = (SELECT id FROM target) AND version = $3
             )
             SELECT v.version,
                    v.min_rux,
                    v.package_type,
                    v.published_at,
                    (v.yanked_at IS NOT NULL) AS yanked
             FROM package_versions v
             WHERE v.package_id = (SELECT id FROM target)
               AND ($3::TEXT IS NULL OR ROW(
                    v.major,
                    v.minor,
                    v.patch,
                    (v.prerelease IS NULL),
                    coalesce(v.prerelease_sort_key, ''::BYTEA),
                    (v.build_metadata IS NOT NULL),
                    coalesce(v.build_metadata_sort_key, ''::BYTEA)
               ) < (SELECT
                    major, minor, patch, stable, prerelease_key, has_build, build_key
               FROM boundary))
             ORDER BY v.major DESC,
                      v.minor DESC,
                      v.patch DESC,
                      (v.prerelease IS NULL) DESC,
                      v.prerelease_sort_key DESC,
                      (v.build_metadata IS NOT NULL) DESC,
                      v.build_metadata_sort_key DESC
             LIMIT $4";
        let rows = query_as::<_, VersionHistoryRow>(sql)
            .bind(namespace.normalized())
            .bind(package.normalized())
            .bind(boundary.map(SemanticVersion::as_str))
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx)?;
        rows.into_iter()
            .map(|row| package_version_history_record(&row))
            .collect::<Result<Vec<_>, _>>()
            .map(Some)
    }

    async fn package_highlights(
        &self,
        since: OffsetDateTime,
        until: OffsetDateTime,
        limit: u16,
    ) -> Result<PackageHighlightsRecord, RepositoryError> {
        let recent_sql = format!(
            "WITH {ACTIVE_REPRESENTATIVE_CTE}
             SELECT namespace_display_name,
                    package_display_name,
                    version,
                    package_type,
                    description,
                    published_at,
                    NULL::BIGINT AS downloads
             FROM active_representative
             WHERE published_at <= $1
             ORDER BY published_at DESC,
                      namespace_normalized_name,
                      package_normalized_name
             LIMIT $2"
        );
        let popular_sql = format!(
            "WITH {ACTIVE_REPRESENTATIVE_CTE}, download_counts AS (
                 SELECT v.package_id, count(*)::BIGINT AS downloads
                 FROM download_events event
                 JOIN package_versions v ON v.id = event.package_version_id
                 WHERE event.occurred_at >= $1 AND event.occurred_at <= $2
                 GROUP BY v.package_id
             )
             SELECT r.namespace_display_name,
                    r.package_display_name,
                    r.version,
                    r.package_type,
                    r.description,
                    r.published_at,
                    counts.downloads
             FROM active_representative r
             JOIN download_counts counts ON counts.package_id = r.package_id
             WHERE r.published_at <= $2
             ORDER BY counts.downloads DESC,
                      r.namespace_normalized_name,
                      r.package_normalized_name
             LIMIT $3"
        );
        let recent_rows = query_as::<_, HighlightPackageRow>(&recent_sql)
            .bind(until)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx)?;
        let popular_rows = query_as::<_, HighlightPackageRow>(&popular_sql)
            .bind(since)
            .bind(until)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx)?;
        Ok(PackageHighlightsRecord {
            recent: recent_rows
                .into_iter()
                .map(highlight_package_record)
                .collect::<Result<_, _>>()?,
            popular: popular_rows
                .into_iter()
                .map(highlight_package_record)
                .collect::<Result<_, _>>()?,
        })
    }

    async fn package_download_statistics(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        since: OffsetDateTime,
        until: OffsetDateTime,
    ) -> Result<Option<PackageDownloadStatisticsRecord>, RepositoryError> {
        if !self.package_exists(namespace, package).await? {
            return Ok(None);
        }
        let rows = query_as::<_, PackageDownloadDayRow>(
            "WITH target AS (
                 SELECT p.id
                 FROM packages p
                 JOIN namespaces n ON n.id = p.namespace_id
                 WHERE n.normalized_name = $1 AND p.normalized_name = $2
             ), days AS (
                 SELECT generate_series($3, $4 - INTERVAL '1 day', INTERVAL '1 day') AS day
             ), daily AS (
                 SELECT (event.occurred_at AT TIME ZONE 'UTC')::DATE AS date,
                        count(*)::BIGINT AS downloads
                 FROM package_versions version
                 JOIN download_events event ON event.package_version_id = version.id
                 WHERE version.package_id = (SELECT id FROM target)
                   AND event.occurred_at >= $3
                   AND event.occurred_at < $4
                 GROUP BY (event.occurred_at AT TIME ZONE 'UTC')::DATE
             ), totals AS (
                 SELECT count(*) FILTER (
                            WHERE event.occurred_at >= $3
                        )::BIGINT AS total_downloads,
                        count(*)::BIGINT AS total_all_time
                 FROM package_versions version
                 JOIN download_events event ON event.package_version_id = version.id
                 WHERE version.package_id = (SELECT id FROM target)
                   AND event.occurred_at < $4
             )
             SELECT (days.day AT TIME ZONE 'UTC')::DATE AS date,
                    coalesce(daily.downloads, 0)::BIGINT AS downloads,
                    totals.total_downloads,
                    totals.total_all_time
             FROM days
             CROSS JOIN totals
             LEFT JOIN daily ON daily.date = (days.day AT TIME ZONE 'UTC')::DATE
             ORDER BY date",
        )
        .bind(namespace.normalized())
        .bind(package.normalized())
        .bind(since)
        .bind(until)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;
        let first = rows
            .first()
            .ok_or_else(|| data_error("download statistics returned no daily buckets"))?;
        let total_downloads = nonnegative_u64(first.total_downloads, "package download total")?;
        let total_all_time =
            nonnegative_u64(first.total_all_time, "all-time package download total")?;
        let start_date = first.date;
        let end_date = rows
            .last()
            .expect("a non-empty daily series has a final bucket")
            .date;
        let daily = rows
            .into_iter()
            .map(|row| {
                Ok(PackageDownloadDayRecord {
                    date: row.date,
                    downloads: nonnegative_u64(row.downloads, "daily package download count")?,
                })
            })
            .collect::<Result<_, RepositoryError>>()?;
        Ok(Some(PackageDownloadStatisticsRecord {
            start_date,
            end_date,
            total_downloads,
            total_all_time,
            daily,
        }))
    }

    async fn sitemap_entries(
        &self,
        boundary: Option<&SitemapBoundary>,
        limit: u16,
    ) -> Result<Vec<SitemapEntryRecord>, RepositoryError> {
        let sql = format!(
            "WITH {REPRESENTATIVE_CTE}, entries AS (
                 SELECT 'keyword'::TEXT AS kind,
                        NULL::TEXT AS namespace_display_name,
                        NULL::TEXT AS package_display_name,
                        (array_agg(
                            k.display_name
                            ORDER BY r.published_at DESC,
                                     r.namespace_normalized_name,
                                     r.package_normalized_name
                        ))[1] AS keyword_display_name,
                        max(r.published_at) AS last_modified,
                        k.normalized_name AS first_identity,
                        ''::TEXT AS second_identity
                 FROM representative r
                 JOIN package_version_keywords k
                   ON k.package_version_id = r.package_version_id
                 GROUP BY k.normalized_name
                 UNION ALL
                 SELECT 'namespace',
                        n.display_name,
                        NULL,
                        NULL,
                        max(v.published_at),
                        n.normalized_name,
                        ''
                 FROM namespaces n
                 JOIN packages p ON p.namespace_id = n.id
                 JOIN package_versions v ON v.package_id = p.id
                 GROUP BY n.id, n.display_name, n.normalized_name
                 UNION ALL
                 SELECT 'package',
                        n.display_name,
                        p.display_name,
                        NULL,
                        max(v.published_at),
                        n.normalized_name,
                        p.normalized_name
                 FROM namespaces n
                 JOIN packages p ON p.namespace_id = n.id
                 JOIN package_versions v ON v.package_id = p.id
                 GROUP BY n.id, n.display_name, n.normalized_name,
                          p.id, p.display_name, p.normalized_name
             )
             SELECT kind,
                    namespace_display_name,
                    package_display_name,
                    keyword_display_name,
                    last_modified
             FROM entries
             WHERE $1::TEXT IS NULL
                OR (kind, first_identity, second_identity) >
                   ($1, $2, coalesce($3, ''))
             ORDER BY kind, first_identity, second_identity
             LIMIT $4"
        );
        let rows = query_as::<_, SitemapEntryRow>(&sql)
            .bind(boundary.map(|value| sitemap_kind_name(value.kind)))
            .bind(boundary.map(|value| value.first_identity.as_str()))
            .bind(boundary.and_then(|value| value.second_identity.as_deref()))
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(map_sqlx)?;
        rows.into_iter().map(sitemap_entry_record).collect()
    }
}

impl PostgresRepository {
    async fn package_exists(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
    ) -> Result<bool, RepositoryError> {
        query_scalar::<_, bool>(
            "SELECT EXISTS (
                 SELECT 1
                 FROM packages p
                 JOIN namespaces n ON n.id = p.namespace_id
                 WHERE n.normalized_name = $1 AND p.normalized_name = $2
             )",
        )
        .bind(namespace.normalized())
        .bind(package.normalized())
        .fetch_one(&self.pool)
        .await
        .map_err(map_sqlx)
    }
}

fn dependent_package_records(
    rows: Vec<DependentPackageRow>,
) -> Result<Vec<DependentPackageRecord>, RepositoryError> {
    let mut records = Vec::new();
    let mut current_id = None;
    for row in rows {
        if current_id != Some(row.package_version_id) {
            current_id = Some(row.package_version_id);
            records.push(DependentPackageRecord {
                namespace: stored_identity(row.namespace_display_name.clone())?,
                package: stored_identity(row.package_display_name.clone())?,
                version: stored_version(&row.version)?,
                package_type: package_kind(&row.package_type)?,
                description: row.description.clone(),
                published_at: row.published_at,
                yanked: row.yanked,
                requirements: Vec::new(),
            });
        }
        records
            .last_mut()
            .expect("a dependent row creates its package record")
            .requirements
            .push(DependencyRecord {
                alias: stored_identity(row.dependency_alias)?,
                target_namespace: stored_identity(row.dependency_target_namespace)?,
                target_package: stored_identity(row.dependency_target_package)?,
                version_range: VersionRange::new(row.dependency_version_range).map_err(
                    |error| RepositoryError::with_source(RepositoryErrorKind::CorruptData, error),
                )?,
            });
    }
    Ok(records)
}

fn keyword_record(row: KeywordDiscoveryRow) -> Result<KeywordRecord, RepositoryError> {
    Ok(KeywordRecord {
        keyword: stored_identity(row.keyword_display_name)?,
        package_count: u64::try_from(row.package_count)
            .map_err(|_| data_error("keyword package count is negative"))?,
    })
}

fn package_version_history_record(
    row: &VersionHistoryRow,
) -> Result<PackageVersionHistoryRecord, RepositoryError> {
    Ok(PackageVersionHistoryRecord {
        version: stored_version(&row.version)?,
        min_rux: stored_version(&row.min_rux)?,
        package_type: package_kind(&row.package_type)?,
        published_at: row.published_at,
        yanked: row.yanked,
    })
}

fn highlight_package_record(
    row: HighlightPackageRow,
) -> Result<HighlightPackageRecord, RepositoryError> {
    Ok(HighlightPackageRecord {
        namespace: stored_identity(row.namespace_display_name)?,
        package: stored_identity(row.package_display_name)?,
        version: stored_version(&row.version)?,
        package_type: package_kind(&row.package_type)?,
        description: row.description,
        published_at: row.published_at,
        downloads: row
            .downloads
            .map(u64::try_from)
            .transpose()
            .map_err(|_| data_error("highlight download count is negative"))?,
    })
}

fn sitemap_entry_record(row: SitemapEntryRow) -> Result<SitemapEntryRecord, RepositoryError> {
    let (kind, namespace, package, keyword) = match row.kind.as_str() {
        "keyword" => (
            SitemapEntryKind::Keyword,
            None,
            None,
            Some(stored_identity(required_sitemap_value(
                row.keyword_display_name,
                "keyword",
            )?)?),
        ),
        "namespace" => (
            SitemapEntryKind::Namespace,
            Some(stored_identity(required_sitemap_value(
                row.namespace_display_name,
                "namespace",
            )?)?),
            None,
            None,
        ),
        "package" => (
            SitemapEntryKind::Package,
            Some(stored_identity(required_sitemap_value(
                row.namespace_display_name,
                "package namespace",
            )?)?),
            Some(stored_identity(required_sitemap_value(
                row.package_display_name,
                "package",
            )?)?),
            None,
        ),
        _ => return Err(data_error("unknown sitemap entry kind")),
    };
    Ok(SitemapEntryRecord {
        kind,
        namespace,
        package,
        keyword,
        last_modified: row.last_modified,
    })
}

fn required_sitemap_value(value: Option<String>, field: &str) -> Result<String, RepositoryError> {
    value.ok_or_else(|| data_error(format!("sitemap {field} is missing")))
}

const fn sitemap_kind_name(kind: SitemapEntryKind) -> &'static str {
    match kind {
        SitemapEntryKind::Keyword => "keyword",
        SitemapEntryKind::Namespace => "namespace",
        SitemapEntryKind::Package => "package",
    }
}

const fn package_kind_name(value: PackageKind) -> &'static str {
    match value {
        PackageKind::Program => "program",
        PackageKind::Library => "library",
        PackageKind::Source => "source",
    }
}

fn literal_pattern(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

fn package_search_record(row: PackageSearchRow) -> Result<PackageSearchRecord, RepositoryError> {
    let match_class = u8::try_from(row.match_class)
        .ok()
        .filter(|value| *value <= 5)
        .ok_or_else(|| data_error("search match class is outside its valid range"))?;
    if row.relevance < 0 {
        return Err(data_error("search relevance is negative"));
    }
    Ok(PackageSearchRecord {
        namespace: stored_identity(row.namespace_display_name)?,
        package: stored_identity(row.package_display_name)?,
        version: SemanticVersion::new(row.version).map_err(|error| {
            RepositoryError::with_source(RepositoryErrorKind::CorruptData, error)
        })?,
        package_type: package_kind(&row.package_type)?,
        description: row.description,
        published_at: row.published_at,
        yanked: row.yanked,
        match_class,
        relevance: row.relevance,
    })
}

#[async_trait]
impl DownloadReader for PostgresRepository {
    async fn download_target_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Result<Option<DownloadTargetRecord>, RepositoryError> {
        query_as::<_, DownloadTargetRow>(
            "SELECT v.id AS package_version_id, v.storage_key
             FROM package_versions v
             JOIN packages p ON p.id = v.package_id
             JOIN namespaces n ON n.id = p.namespace_id
             WHERE n.normalized_name = $1
               AND p.normalized_name = $2
               AND v.version = $3",
        )
        .bind(namespace.normalized())
        .bind(package.normalized())
        .bind(version.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx)
        .map(|target| {
            target.map(|target| DownloadTargetRecord {
                package_version_id: PackageVersionId::new(target.package_version_id),
                storage_key: target.storage_key,
            })
        })
    }
}

#[async_trait]
impl ResolverIndexReader for PostgresRepository {
    async fn resolver_index_by_name(
        &self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
    ) -> Result<Option<ResolverIndexRecord>, RepositoryError> {
        let rows = query_as::<_, ResolverIndexRow>(
            "SELECT n.display_name AS namespace_display_name,
                    p.display_name AS package_display_name,
                    v.id AS package_version_id,
                    v.version,
                    v.min_rux,
                    (v.yanked_at IS NOT NULL) AS yanked,
                    d.display_alias AS dependency_alias,
                    d.target_namespace_display_name AS dependency_target_namespace,
                    d.target_package_display_name AS dependency_target_package,
                    d.version_range AS dependency_version_range
             FROM namespaces n
             JOIN packages p ON p.namespace_id = n.id
             LEFT JOIN package_versions v ON v.package_id = p.id
             LEFT JOIN dependencies d ON d.package_version_id = v.id
             WHERE n.normalized_name = $1 AND p.normalized_name = $2
             ORDER BY v.id, d.normalized_alias",
        )
        .bind(namespace.normalized())
        .bind(package.normalized())
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)?;

        resolver_index_record(rows)
    }
}

fn resolver_index_record(
    rows: Vec<ResolverIndexRow>,
) -> Result<Option<ResolverIndexRecord>, RepositoryError> {
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    let mut index = ResolverIndexRecord {
        namespace: stored_identity(first.namespace_display_name.clone())?,
        package: stored_identity(first.package_display_name.clone())?,
        versions: Vec::new(),
    };

    let mut current_version_id = None;
    for row in rows {
        let Some(version_id) = row.package_version_id else {
            if resolver_version_columns_present(&row) {
                return Err(data_error("resolver index version state is inconsistent"));
            }
            continue;
        };
        if current_version_id != Some(version_id) {
            index.versions.push(resolver_version(&row)?);
            current_version_id = Some(version_id);
        }
        if let Some(dependency) = resolver_dependency(row)? {
            index
                .versions
                .last_mut()
                .expect("a dependency row always has a package version")
                .dependencies
                .push(dependency);
        }
    }
    Ok(Some(index))
}

fn resolver_version_columns_present(row: &ResolverIndexRow) -> bool {
    row.version.is_some()
        || row.min_rux.is_some()
        || row.dependency_alias.is_some()
        || row.dependency_target_namespace.is_some()
        || row.dependency_target_package.is_some()
        || row.dependency_version_range.is_some()
}

fn resolver_version(row: &ResolverIndexRow) -> Result<ResolverVersionRecord, RepositoryError> {
    Ok(ResolverVersionRecord {
        version: stored_version(
            row.version
                .as_deref()
                .ok_or_else(|| data_error("resolver index version is missing"))?,
        )?,
        min_rux: stored_version(
            row.min_rux
                .as_deref()
                .ok_or_else(|| data_error("resolver index minimum Rux is missing"))?,
        )?,
        yanked: row.yanked,
        dependencies: Vec::new(),
    })
}

fn resolver_dependency(row: ResolverIndexRow) -> Result<Option<DependencyRecord>, RepositoryError> {
    match (
        row.dependency_alias,
        row.dependency_target_namespace,
        row.dependency_target_package,
        row.dependency_version_range,
    ) {
        (None, None, None, None) => Ok(None),
        (Some(alias), Some(namespace), Some(package), Some(version_range)) => {
            Ok(Some(DependencyRecord {
                alias: stored_identity(alias)?,
                target_namespace: stored_identity(namespace)?,
                target_package: stored_identity(package)?,
                version_range: VersionRange::new(version_range).map_err(|error| {
                    RepositoryError::with_source(RepositoryErrorKind::CorruptData, error)
                })?,
            }))
        }
        _ => Err(data_error(
            "resolver index dependency state is inconsistent",
        )),
    }
}

fn stored_identity(value: String) -> Result<IdentitySegment, RepositoryError> {
    IdentitySegment::new(value)
        .map_err(|error| RepositoryError::with_source(RepositoryErrorKind::CorruptData, error))
}

fn stored_version(value: &str) -> Result<SemanticVersion, RepositoryError> {
    SemanticVersion::new(value)
        .map_err(|error| RepositoryError::with_source(RepositoryErrorKind::CorruptData, error))
}

#[async_trait]
impl ArtifactReferenceReader for PostgresRepository {
    async fn referenced_storage_keys(
        &self,
        keys: &[String],
    ) -> Result<Vec<String>, RepositoryError> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        query_scalar(
            "SELECT storage_key FROM package_versions WHERE storage_key = ANY($1) ORDER BY storage_key",
        )
        .bind(keys)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx)
    }
}

#[async_trait]
impl UnitOfWork for PostgresRepository {
    async fn begin(&self) -> Result<Box<dyn RegistryTransaction>, RepositoryError> {
        Ok(Box::new(PostgresTransaction {
            transaction: self.pool.begin().await.map_err(map_sqlx)?,
        }))
    }
}

#[async_trait]
impl DownloadUnitOfWork for PostgresRepository {
    async fn begin_download(&self) -> Result<Box<dyn DownloadTransaction>, RepositoryError> {
        Ok(Box::new(PostgresTransaction {
            transaction: self.pool.begin().await.map_err(map_sqlx)?,
        }))
    }
}

#[async_trait]
impl AccountUnitOfWork for PostgresRepository {
    async fn begin_account(&self) -> Result<Box<dyn AccountTransaction>, RepositoryError> {
        Ok(Box::new(PostgresTransaction {
            transaction: self.pool.begin().await.map_err(map_sqlx)?,
        }))
    }
}

#[async_trait]
impl AccountLifecycleUnitOfWork for PostgresRepository {
    async fn begin_account_lifecycle(
        &self,
    ) -> Result<Box<dyn AccountLifecycleTransaction>, RepositoryError> {
        Ok(Box::new(PostgresTransaction {
            transaction: self.pool.begin().await.map_err(map_sqlx)?,
        }))
    }
}

#[async_trait]
impl TokenUnitOfWork for PostgresRepository {
    async fn begin_tokens(&self) -> Result<Box<dyn TokenTransaction>, RepositoryError> {
        Ok(Box::new(PostgresTransaction {
            transaction: self.pool.begin().await.map_err(map_sqlx)?,
        }))
    }
}

#[async_trait]
impl AccountWriter for PostgresTransaction {
    async fn upsert_github_user(
        &mut self,
        profile: &GitHubUserProfile,
    ) -> Result<UserRecord, RepositoryError> {
        let sql = format!(
            "INSERT INTO users (github_user_id, github_login, display_name, avatar_url) VALUES ($1::numeric, $2, $3, $4) ON CONFLICT (github_user_id) DO UPDATE SET github_login = EXCLUDED.github_login, display_name = EXCLUDED.display_name, avatar_url = EXCLUDED.avatar_url, updated_at = now() RETURNING {USER_COLUMNS}"
        );
        user_record(
            query_as::<_, UserRow>(&sql)
                .bind(profile.github_user_id.to_string())
                .bind(&profile.github_login)
                .bind(&profile.display_name)
                .bind(&profile.avatar_url)
                .fetch_one(&mut *self.transaction)
                .await
                .map_err(map_sqlx)?,
        )
    }

    async fn create_session(
        &mut self,
        session: &NewSession,
    ) -> Result<SessionRecord, RepositoryError> {
        let sql = format!(
            "INSERT INTO sessions (user_id, secret_hash, csrf_hash, expires_at) VALUES ($1, $2, $3, $4) RETURNING {SESSION_COLUMNS}"
        );
        session_record(
            query_as::<_, SessionRow>(&sql)
                .bind(session.user_id.get())
                .bind(session.secret_hash.as_bytes().as_slice())
                .bind(session.csrf_hash.as_bytes().as_slice())
                .bind(session.expires_at)
                .fetch_one(&mut *self.transaction)
                .await
                .map_err(map_sqlx)?,
        )
    }

    async fn touch_session(
        &mut self,
        id: SessionId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError> {
        let rows =
            query("UPDATE sessions SET last_seen_at = $2 WHERE id = $1 AND last_seen_at < $2")
                .bind(id.get())
                .bind(at)
                .execute(&mut *self.transaction)
                .await
                .map_err(map_sqlx)?
                .rows_affected();
        conditional_outcome(&mut self.transaction, rows, EntityTable::Session, id.get()).await
    }

    async fn revoke_session(
        &mut self,
        id: SessionId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError> {
        let rows =
            query("UPDATE sessions SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL")
                .bind(id.get())
                .bind(at)
                .execute(&mut *self.transaction)
                .await
                .map_err(map_sqlx)?
                .rows_affected();
        conditional_outcome(&mut self.transaction, rows, EntityTable::Session, id.get()).await
    }
}

fn direct_outcome(rows: u64) -> Result<WriteOutcome, RepositoryError> {
    match rows {
        1 => Ok(WriteOutcome::Applied),
        0 => Ok(WriteOutcome::NotFound),
        _ => Err(data_error("single-row mutation affected multiple rows")),
    }
}

#[derive(Clone, Copy)]
enum EntityTable {
    Session,
    Invitation,
    Token,
    PackageVersion,
}

async fn conditional_outcome(
    transaction: &mut Transaction<'static, Postgres>,
    rows: u64,
    table: EntityTable,
    id: Uuid,
) -> Result<WriteOutcome, RepositoryError> {
    if rows == 1 {
        return Ok(WriteOutcome::Applied);
    }
    if rows > 1 {
        return Err(data_error("single-row mutation affected multiple rows"));
    }
    let sql = match table {
        EntityTable::Session => "SELECT EXISTS (SELECT 1 FROM sessions WHERE id = $1)",
        EntityTable::Invitation => {
            "SELECT EXISTS (SELECT 1 FROM namespace_invitations WHERE id = $1)"
        }
        EntityTable::Token => "SELECT EXISTS (SELECT 1 FROM api_tokens WHERE id = $1)",
        EntityTable::PackageVersion => {
            "SELECT EXISTS (SELECT 1 FROM package_versions WHERE id = $1)"
        }
    };
    let exists = query_scalar::<_, bool>(sql)
        .bind(id)
        .fetch_one(&mut **transaction)
        .await
        .map_err(map_sqlx)?;
    Ok(if exists {
        WriteOutcome::Unchanged
    } else {
        WriteOutcome::NotFound
    })
}

#[async_trait]
impl NamespaceWriter for PostgresTransaction {
    async fn create_namespace(
        &mut self,
        name: &IdentitySegment,
        actor: Option<UserId>,
    ) -> Result<NamespaceRecord, RepositoryError> {
        let sql = format!(
            "INSERT INTO namespaces (display_name, created_by_user_id) VALUES ($1, $2) RETURNING {NAMESPACE_COLUMNS}"
        );
        namespace_record(
            query_as::<_, NamespaceRow>(&sql)
                .bind(name.as_str())
                .bind(actor.map(UserId::get))
                .fetch_one(&mut *self.transaction)
                .await
                .map_err(map_sqlx)?,
        )
    }

    async fn set_namespace_owner(
        &mut self,
        namespace_id: NamespaceId,
        user_id: UserId,
        role_value: NamespaceRole,
        actor: Option<UserId>,
    ) -> Result<NamespaceOwnerRecord, RepositoryError> {
        let row = query_as::<_, OwnerRow>("INSERT INTO namespace_owners (namespace_id, user_id, role, added_by_user_id) VALUES ($1, $2, $3, $4) ON CONFLICT (namespace_id, user_id) DO UPDATE SET role = EXCLUDED.role, added_by_user_id = EXCLUDED.added_by_user_id RETURNING namespace_id, user_id, role, added_by_user_id, created_at").bind(namespace_id.get()).bind(user_id.get()).bind(role_text(role_value)).bind(actor.map(UserId::get)).fetch_one(&mut *self.transaction).await.map_err(map_sqlx)?;
        owner_record(&row)
    }

    async fn remove_namespace_owner(
        &mut self,
        namespace_id: NamespaceId,
        user_id: UserId,
    ) -> Result<WriteOutcome, RepositoryError> {
        direct_outcome(
            query("DELETE FROM namespace_owners WHERE namespace_id = $1 AND user_id = $2")
                .bind(namespace_id.get())
                .bind(user_id.get())
                .execute(&mut *self.transaction)
                .await
                .map_err(map_sqlx)?
                .rows_affected(),
        )
    }

    async fn create_invitation(
        &mut self,
        invitation: &NewInvitation,
    ) -> Result<InvitationRecord, RepositoryError> {
        let sql = format!(
            "INSERT INTO namespace_invitations (namespace_id, invited_user_id, invited_by_user_id, role, expires_at) VALUES ($1, $2, $3, $4, $5) RETURNING {INVITATION_COLUMNS}"
        );
        let row = query_as::<_, InvitationRow>(&sql)
            .bind(invitation.namespace_id.get())
            .bind(invitation.invited_user_id.get())
            .bind(invitation.invited_by_user_id.map(UserId::get))
            .bind(role_text(invitation.role))
            .bind(invitation.expires_at)
            .fetch_one(&mut *self.transaction)
            .await
            .map_err(map_sqlx)?;
        invitation_record(&row)
    }

    async fn resolve_invitation(
        &mut self,
        id: InvitationId,
        resolution: InvitationResolution,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError> {
        let column = match resolution {
            InvitationResolution::Accepted => "accepted_at",
            InvitationResolution::Revoked => "revoked_at",
        };
        let sql = format!(
            "UPDATE namespace_invitations SET {column} = $2 WHERE id = $1 AND accepted_at IS NULL AND revoked_at IS NULL"
        );
        let rows = query(&sql)
            .bind(id.get())
            .bind(at)
            .execute(&mut *self.transaction)
            .await
            .map_err(map_sqlx)?
            .rows_affected();
        conditional_outcome(
            &mut self.transaction,
            rows,
            EntityTable::Invitation,
            id.get(),
        )
        .await
    }

    async fn touch_namespace(
        &mut self,
        namespace_id: NamespaceId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError> {
        direct_outcome(
            query("UPDATE namespaces SET updated_at = $2 WHERE id = $1")
                .bind(namespace_id.get())
                .bind(at)
                .execute(&mut *self.transaction)
                .await
                .map_err(map_sqlx)?
                .rows_affected(),
        )
    }
}

#[async_trait]
impl TokenWriter for PostgresTransaction {
    async fn create_token(
        &mut self,
        token: &NewApiToken,
    ) -> Result<ApiTokenRecord, RepositoryError> {
        let sql = format!(
            "INSERT INTO api_tokens (user_id, display_name, token_prefix, secret_hash, expires_at) VALUES ($1, $2, $3, $4, $5) RETURNING {TOKEN_COLUMNS}"
        );
        let row = query_as::<_, TokenRow>(&sql)
            .bind(token.user_id.get())
            .bind(&token.display_name)
            .bind(&token.token_prefix)
            .bind(token.secret_hash.as_bytes().as_slice())
            .bind(token.expires_at)
            .fetch_one(&mut *self.transaction)
            .await
            .map_err(map_sqlx)?;
        let mut scopes = token.scopes.clone();
        scopes.sort_unstable();
        scopes.dedup();
        for item in &scopes {
            query("INSERT INTO api_token_scopes (api_token_id, scope) VALUES ($1, $2)")
                .bind(row.id)
                .bind(scope_text(*item))
                .execute(&mut *self.transaction)
                .await
                .map_err(map_sqlx)?;
        }
        Ok(ApiTokenRecord {
            id: ApiTokenId::new(row.id),
            user_id: UserId::new(row.user_id),
            display_name: row.display_name,
            token_prefix: row.token_prefix,
            secret_hash: fixed_hash(row.secret_hash, "token secret hash")?,
            scopes,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
            expires_at: row.expires_at,
            revoked_at: row.revoked_at,
        })
    }

    async fn touch_token(
        &mut self,
        id: ApiTokenId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError> {
        let rows = query("UPDATE api_tokens SET last_used_at = $2 WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < $2)").bind(id.get()).bind(at).execute(&mut *self.transaction).await.map_err(map_sqlx)?.rows_affected();
        conditional_outcome(&mut self.transaction, rows, EntityTable::Token, id.get()).await
    }

    async fn revoke_token(
        &mut self,
        id: ApiTokenId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError> {
        let rows =
            query("UPDATE api_tokens SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL")
                .bind(id.get())
                .bind(at)
                .execute(&mut *self.transaction)
                .await
                .map_err(map_sqlx)?
                .rows_affected();
        conditional_outcome(&mut self.transaction, rows, EntityTable::Token, id.get()).await
    }
}

#[async_trait]
impl CatalogWriter for PostgresTransaction {
    async fn create_package(
        &mut self,
        namespace_id: NamespaceId,
        name: &IdentitySegment,
        actor: Option<UserId>,
    ) -> Result<PackageRecord, RepositoryError> {
        let sql = format!(
            "INSERT INTO packages (namespace_id, display_name, created_by_user_id) VALUES ($1, $2, $3) RETURNING {PACKAGE_COLUMNS}"
        );
        package_record(
            query_as::<_, PackageRow>(&sql)
                .bind(namespace_id.get())
                .bind(name.as_str())
                .bind(actor.map(UserId::get))
                .fetch_one(&mut *self.transaction)
                .await
                .map_err(map_sqlx)?,
        )
    }

    async fn create_package_version(
        &mut self,
        version: &NewPackageVersion,
    ) -> Result<PackageVersionRecord, RepositoryError> {
        let normalized_manifest = Value::Object(version.normalized_manifest.clone());
        let (readme_file_path, readme_file_text) = version
            .readme_file
            .clone()
            .map_or((None, None), |(path, text)| (Some(path), Some(text)));
        let (license_file_path, license_file_text) = version
            .license_file
            .clone()
            .map_or((None, None), |(path, text)| (Some(path), Some(text)));
        let sql = format!("INSERT INTO package_versions (package_id, version, major, minor, patch, prerelease, build_metadata, manifest_schema_version, min_rux, package_type, description, repository_url, homepage_url, readme_file_path, readme_file_text, license_expression, license_file_path, license_file_text, normalized_manifest, artifact_sha256, artifact_size, storage_key, artifact_file_count, artifact_expanded_bytes, source_file_count, source_line_count, published_by_user_id) VALUES ($1, $2, $3::numeric, $4::numeric, $5::numeric, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27) RETURNING {VERSION_COLUMNS}").replace("v.", "");
        let row = query_as::<_, VersionRow>(&sql)
            .bind(version.package_id.get())
            .bind(version.version.as_str())
            .bind(version.version.major().to_string())
            .bind(version.version.minor().to_string())
            .bind(version.version.patch().to_string())
            .bind(version.version.prerelease())
            .bind(version.version.build())
            .bind(
                i16::try_from(version.manifest_schema_version)
                    .map_err(|_| data_error("manifest schema version exceeds i16"))?,
            )
            .bind(version.min_rux.as_str())
            .bind(package_kind_text(version.package_type))
            .bind(&version.description)
            .bind(&version.repository_url)
            .bind(&version.homepage_url)
            .bind(readme_file_path)
            .bind(readme_file_text)
            .bind(&version.license_expression)
            .bind(license_file_path)
            .bind(license_file_text)
            .bind(normalized_manifest)
            .bind(version.artifact_sha256.as_bytes().as_slice())
            .bind(
                i64::try_from(version.artifact_size)
                    .map_err(|_| data_error("artifact size exceeds i64"))?,
            )
            .bind(&version.storage_key)
            .bind(
                i32::try_from(version.artifact_file_count)
                    .map_err(|_| data_error("artifact file count exceeds i32"))?,
            )
            .bind(
                i64::try_from(version.artifact_expanded_bytes)
                    .map_err(|_| data_error("artifact expanded bytes exceeds i64"))?,
            )
            .bind(
                i32::try_from(version.source_file_count)
                    .map_err(|_| data_error("source file count exceeds i32"))?,
            )
            .bind(
                i64::try_from(version.source_line_count)
                    .map_err(|_| data_error("source line count exceeds i64"))?,
            )
            .bind(version.published_by_user_id.map(UserId::get))
            .fetch_one(&mut *self.transaction)
            .await
            .map_err(map_sqlx)?;
        for (ordinal, author) in version.authors.iter().enumerate() {
            query("INSERT INTO package_version_authors (package_version_id, ordinal, author) VALUES ($1, $2, $3)").bind(row.id).bind(i16::try_from(ordinal).map_err(|_| data_error("too many authors"))?).bind(author).execute(&mut *self.transaction).await.map_err(map_sqlx)?;
        }
        for (ordinal, keyword) in version.keywords.iter().enumerate() {
            query("INSERT INTO package_version_keywords (package_version_id, ordinal, display_name) VALUES ($1, $2, $3)").bind(row.id).bind(i16::try_from(ordinal).map_err(|_| data_error("too many keywords"))?).bind(keyword.as_str()).execute(&mut *self.transaction).await.map_err(map_sqlx)?;
        }
        for dependency in &version.dependencies {
            query("INSERT INTO dependencies (package_version_id, display_alias, target_namespace_display_name, target_package_display_name, version_range) VALUES ($1, $2, $3, $4, $5)").bind(row.id).bind(dependency.alias.as_str()).bind(dependency.target_namespace.as_str()).bind(dependency.target_package.as_str()).bind(dependency.version_range.as_str()).execute(&mut *self.transaction).await.map_err(map_sqlx)?;
        }
        let mut dependencies = version.dependencies.clone();
        dependencies.sort_by(|left, right| left.alias.cmp(&right.alias));
        Ok(PackageVersionRecord {
            id: PackageVersionId::new(row.id),
            package_id: version.package_id,
            version: version.version.clone(),
            manifest_schema_version: version.manifest_schema_version,
            min_rux: version.min_rux.clone(),
            package_type: version.package_type,
            description: version.description.clone(),
            repository_url: version.repository_url.clone(),
            homepage_url: version.homepage_url.clone(),
            readme_file: version.readme_file.clone(),
            license_expression: version.license_expression.clone(),
            license_file: version.license_file.clone(),
            normalized_manifest: version.normalized_manifest.clone(),
            artifact_sha256: version.artifact_sha256,
            artifact_size: version.artifact_size,
            storage_key: version.storage_key.clone(),
            artifact_file_count: version.artifact_file_count,
            artifact_expanded_bytes: version.artifact_expanded_bytes,
            source_file_count: version.source_file_count,
            source_line_count: version.source_line_count,
            published_by_user_id: version.published_by_user_id,
            published_at: row.published_at,
            yanked_at: None,
            yanked_by_user_id: None,
            authors: version.authors.clone(),
            keywords: version.keywords.clone(),
            dependencies,
        })
    }

    async fn set_yank(
        &mut self,
        id: PackageVersionId,
        yank: Option<(OffsetDateTime, UserId)>,
    ) -> Result<WriteOutcome, RepositoryError> {
        let rows = match yank {
            Some((at, actor)) => query(
                "UPDATE package_versions
                 SET yanked_at = $2, yanked_by_user_id = $3
                 WHERE id = $1 AND yanked_at IS NULL",
            )
            .bind(id.get())
            .bind(at)
            .bind(actor.get())
            .execute(&mut *self.transaction)
            .await
            .map_err(map_sqlx)?
            .rows_affected(),
            None => query(
                "UPDATE package_versions
                 SET yanked_at = NULL, yanked_by_user_id = NULL
                 WHERE id = $1 AND yanked_at IS NOT NULL",
            )
            .bind(id.get())
            .execute(&mut *self.transaction)
            .await
            .map_err(map_sqlx)?
            .rows_affected(),
        };
        conditional_outcome(
            &mut self.transaction,
            rows,
            EntityTable::PackageVersion,
            id.get(),
        )
        .await
    }
}

#[async_trait]
impl AuditWriter for PostgresTransaction {
    async fn append_audit(&mut self, event: &AuditEvent) -> Result<(), RepositoryError> {
        let actor = event.actor();
        query("INSERT INTO audit_records (actor_user_id, actor_token_id, action, subject_type, subject_key, metadata) VALUES ($1, $2, $3, $4, $5, $6)")
            .bind(Some(actor.user_id().get())).bind(actor.token_id().map(ApiTokenId::get)).bind(event.action()).bind(event.subject_type()).bind(event.subject_key()).bind(Value::Object(event.metadata().clone())).execute(&mut *self.transaction).await.map_err(map_sqlx)?;
        Ok(())
    }
}

#[async_trait]
impl DownloadWriter for PostgresTransaction {
    async fn append_download(
        &mut self,
        version_id: PackageVersionId,
        at: OffsetDateTime,
    ) -> Result<(), RepositoryError> {
        query("INSERT INTO download_events (package_version_id, occurred_at) VALUES ($1, $2)")
            .bind(version_id.get())
            .bind(at)
            .execute(&mut *self.transaction)
            .await
            .map_err(map_sqlx)?;
        Ok(())
    }
}

#[async_trait]
impl TransactionReader for PostgresTransaction {
    async fn is_identity_blocked(
        &mut self,
        kind: BlockedIdentityKind,
        name: &IdentitySegment,
    ) -> Result<bool, RepositoryError> {
        let kind = match kind {
            BlockedIdentityKind::Namespace => "namespace",
            BlockedIdentityKind::Package => "package",
        };
        query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                 FROM blocked_identities
                 WHERE identity_kind = $1 AND normalized_name = $2
             )",
        )
        .bind(kind)
        .bind(name.normalized())
        .fetch_one(&mut *self.transaction)
        .await
        .map_err(map_sqlx)
    }

    async fn lock_namespace_by_name(
        &mut self,
        name: &IdentitySegment,
    ) -> Result<Option<NamespaceRecord>, RepositoryError> {
        query_as::<_, NamespaceRow>(&format!(
            "SELECT {NAMESPACE_COLUMNS} FROM namespaces WHERE normalized_name = $1 FOR UPDATE"
        ))
        .bind(name.normalized())
        .fetch_optional(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?
        .map(namespace_record)
        .transpose()
    }

    async fn lock_namespace_role(
        &mut self,
        namespace_id: NamespaceId,
        user_id: UserId,
    ) -> Result<Option<NamespaceOwnerRecord>, RepositoryError> {
        query_as::<_, OwnerRow>("SELECT namespace_id, user_id, role, added_by_user_id, created_at FROM namespace_owners WHERE namespace_id = $1 AND user_id = $2 FOR UPDATE")
            .bind(namespace_id.get()).bind(user_id.get()).fetch_optional(&mut *self.transaction).await.map_err(map_sqlx)?.map(|row| owner_record(&row)).transpose()
    }

    async fn user_by_github_login_in_transaction(
        &mut self,
        login: &str,
    ) -> Result<Option<UserRecord>, RepositoryError> {
        query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM users WHERE normalized_github_login = lower($1)"
        ))
        .bind(login)
        .fetch_optional(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?
        .map(user_record)
        .transpose()
    }

    async fn lock_pending_invitation(
        &mut self,
        namespace_id: NamespaceId,
        invited_user_id: UserId,
    ) -> Result<Option<InvitationRecord>, RepositoryError> {
        query_as::<_, InvitationRow>(&format!(
            "SELECT {INVITATION_COLUMNS} FROM namespace_invitations
             WHERE namespace_id = $1 AND invited_user_id = $2
                 AND accepted_at IS NULL AND revoked_at IS NULL
             FOR UPDATE"
        ))
        .bind(namespace_id.get())
        .bind(invited_user_id.get())
        .fetch_optional(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?
        .map(|row| invitation_record(&row))
        .transpose()
    }

    async fn namespace_owner_count(
        &mut self,
        namespace_id: NamespaceId,
    ) -> Result<u64, RepositoryError> {
        let count = query_scalar::<_, i64>(
            "SELECT count(*) FROM namespace_owners WHERE namespace_id = $1 AND role = 'owner'",
        )
        .bind(namespace_id.get())
        .fetch_one(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?;
        u64::try_from(count)
            .map_err(|error| RepositoryError::with_source(RepositoryErrorKind::CorruptData, error))
    }

    async fn lock_package_by_name(
        &mut self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
    ) -> Result<Option<PackageRecord>, RepositoryError> {
        query_as::<_, PackageRow>(&format!("SELECT {QUALIFIED_PACKAGE_COLUMNS} FROM packages p JOIN namespaces n ON n.id = p.namespace_id WHERE n.normalized_name = $1 AND p.normalized_name = $2 FOR UPDATE OF p"))
            .bind(namespace.normalized()).bind(package.normalized()).fetch_optional(&mut *self.transaction).await.map_err(map_sqlx)?.map(package_record).transpose()
    }

    async fn lock_version_id_by_name(
        &mut self,
        namespace: &IdentitySegment,
        package: &IdentitySegment,
        version: &SemanticVersion,
    ) -> Result<Option<PackageVersionId>, RepositoryError> {
        query("SELECT v.id FROM package_versions v JOIN packages p ON p.id = v.package_id JOIN namespaces n ON n.id = p.namespace_id WHERE n.normalized_name = $1 AND p.normalized_name = $2 AND v.version = $3 FOR UPDATE OF v")
            .bind(namespace.normalized()).bind(package.normalized()).bind(version.as_str()).fetch_optional(&mut *self.transaction).await.map_err(map_sqlx).map(|row| row.map(|row| PackageVersionId::new(row.get::<Uuid, _>("id"))))
    }
}

#[async_trait]
impl TokenAuthorizationTransaction for PostgresTransaction {
    async fn lock_user_by_id(&mut self, id: UserId) -> Result<Option<UserRecord>, RepositoryError> {
        query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM users WHERE id = $1 FOR UPDATE"
        ))
        .bind(id.get())
        .fetch_optional(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?
        .map(user_record)
        .transpose()
    }

    async fn lock_token_by_secret_hash(
        &mut self,
        hash: SecretHash,
    ) -> Result<Option<ApiTokenRecord>, RepositoryError> {
        let row = query_as::<_, TokenRow>(&format!(
            "SELECT {TOKEN_COLUMNS} FROM api_tokens WHERE secret_hash = $1 FOR UPDATE"
        ))
        .bind(hash.as_bytes().as_slice())
        .fetch_optional(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?;
        let Some(row) = row else { return Ok(None) };
        let scopes = token_scopes(&mut *self.transaction, row.id).await?;
        Ok(Some(ApiTokenRecord {
            id: ApiTokenId::new(row.id),
            user_id: UserId::new(row.user_id),
            display_name: row.display_name,
            token_prefix: row.token_prefix,
            secret_hash: fixed_hash(row.secret_hash, "token secret hash")?,
            scopes,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
            expires_at: row.expires_at,
            revoked_at: row.revoked_at,
        }))
    }

    async fn lock_token_by_prefix(
        &mut self,
        user_id: UserId,
        prefix: &str,
    ) -> Result<Option<ApiTokenRecord>, RepositoryError> {
        let row = query_as::<_, TokenRow>(&format!(
            "SELECT {TOKEN_COLUMNS} FROM api_tokens WHERE user_id = $1 AND token_prefix = $2 FOR UPDATE"
        ))
        .bind(user_id.get())
        .bind(prefix)
        .fetch_optional(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?;
        let Some(row) = row else { return Ok(None) };
        let scopes = token_scopes(&mut *self.transaction, row.id).await?;
        Ok(Some(ApiTokenRecord {
            id: ApiTokenId::new(row.id),
            user_id: UserId::new(row.user_id),
            display_name: row.display_name,
            token_prefix: row.token_prefix,
            secret_hash: fixed_hash(row.secret_hash, "token secret hash")?,
            scopes,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
            expires_at: row.expires_at,
            revoked_at: row.revoked_at,
        }))
    }
}

#[async_trait]
impl TokenTransaction for PostgresTransaction {
    async fn commit(self: Box<Self>) -> Result<(), RepositoryError> {
        self.transaction.commit().await.map_err(map_sqlx)
    }

    async fn rollback(self: Box<Self>) -> Result<(), RepositoryError> {
        self.transaction.rollback().await.map_err(map_sqlx)
    }
}

#[async_trait]
impl DownloadTransaction for PostgresTransaction {
    async fn commit(self: Box<Self>) -> Result<(), RepositoryError> {
        self.transaction.commit().await.map_err(map_sqlx)
    }

    async fn rollback(self: Box<Self>) -> Result<(), RepositoryError> {
        self.transaction.rollback().await.map_err(map_sqlx)
    }
}

#[async_trait]
impl RegistryTransaction for PostgresTransaction {
    async fn commit(self: Box<Self>) -> Result<(), RepositoryError> {
        self.transaction.commit().await.map_err(map_sqlx)
    }

    async fn rollback(self: Box<Self>) -> Result<(), RepositoryError> {
        self.transaction.rollback().await.map_err(map_sqlx)
    }
}

#[async_trait]
impl AccountTransaction for PostgresTransaction {
    async fn lock_session_by_secret_hash(
        &mut self,
        hash: SecretHash,
    ) -> Result<Option<SessionRecord>, RepositoryError> {
        query_as::<_, SessionRow>(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions WHERE secret_hash = $1 FOR UPDATE"
        ))
        .bind(hash.as_bytes().as_slice())
        .fetch_optional(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?
        .map(session_record)
        .transpose()
    }

    async fn commit(self: Box<Self>) -> Result<(), RepositoryError> {
        self.transaction.commit().await.map_err(map_sqlx)
    }

    async fn rollback(self: Box<Self>) -> Result<(), RepositoryError> {
        self.transaction.rollback().await.map_err(map_sqlx)
    }
}

#[async_trait]
impl AccountLifecycleTransaction for PostgresTransaction {
    async fn lock_user_by_id(&mut self, id: UserId) -> Result<Option<UserRecord>, RepositoryError> {
        query_as::<_, UserRow>(&format!(
            "SELECT {USER_COLUMNS} FROM users WHERE id = $1 FOR UPDATE"
        ))
        .bind(id.get())
        .fetch_optional(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?
        .map(user_record)
        .transpose()
    }

    async fn lock_memberships_by_user_id(
        &mut self,
        user_id: UserId,
    ) -> Result<Vec<NamespaceMembershipRecord>, RepositoryError> {
        query_as::<_, MembershipRow>(
            "SELECT n.id AS namespace_id, n.display_name AS namespace_display_name,
                    n.created_by_user_id AS namespace_created_by_user_id,
                    n.created_at AS namespace_created_at, n.updated_at AS namespace_updated_at,
                    o.user_id, o.role, o.added_by_user_id,
                    o.created_at AS membership_created_at
             FROM namespace_owners o
             JOIN namespaces n ON n.id = o.namespace_id
             WHERE o.user_id = $1
             ORDER BY n.normalized_name
             FOR UPDATE OF n, o",
        )
        .bind(user_id.get())
        .fetch_all(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .map(membership_record)
        .collect()
    }

    async fn namespace_owner_count(
        &mut self,
        namespace_id: NamespaceId,
    ) -> Result<u64, RepositoryError> {
        let count = query_scalar::<_, i64>(
            "SELECT count(*) FROM namespace_owners WHERE namespace_id = $1 AND role = 'owner'",
        )
        .bind(namespace_id.get())
        .fetch_one(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?;
        u64::try_from(count)
            .map_err(|error| RepositoryError::with_source(RepositoryErrorKind::CorruptData, error))
    }

    async fn revoke_sessions_by_user_id(
        &mut self,
        user_id: UserId,
        at: OffsetDateTime,
    ) -> Result<u64, RepositoryError> {
        query("UPDATE sessions SET revoked_at = $2 WHERE user_id = $1 AND revoked_at IS NULL")
            .bind(user_id.get())
            .bind(at)
            .execute(&mut *self.transaction)
            .await
            .map_err(map_sqlx)
            .map(|result| result.rows_affected())
    }

    async fn revoke_and_scrub_tokens_by_user_id(
        &mut self,
        user_id: UserId,
        at: OffsetDateTime,
        replacement_name: &str,
    ) -> Result<u64, RepositoryError> {
        query(
            "UPDATE api_tokens
             SET display_name = $3, revoked_at = COALESCE(revoked_at, $2)
             WHERE user_id = $1",
        )
        .bind(user_id.get())
        .bind(at)
        .bind(replacement_name)
        .execute(&mut *self.transaction)
        .await
        .map_err(map_sqlx)
        .map(|result| result.rows_affected())
    }

    async fn revoke_incoming_invitations_by_user_id(
        &mut self,
        user_id: UserId,
        at: OffsetDateTime,
    ) -> Result<u64, RepositoryError> {
        query(
            "UPDATE namespace_invitations
             SET revoked_at = $2
             WHERE invited_user_id = $1 AND accepted_at IS NULL AND revoked_at IS NULL",
        )
        .bind(user_id.get())
        .bind(at)
        .execute(&mut *self.transaction)
        .await
        .map_err(map_sqlx)
        .map(|result| result.rows_affected())
    }

    async fn remove_memberships_by_user_id(
        &mut self,
        user_id: UserId,
    ) -> Result<u64, RepositoryError> {
        query("DELETE FROM namespace_owners WHERE user_id = $1")
            .bind(user_id.get())
            .execute(&mut *self.transaction)
            .await
            .map_err(map_sqlx)
            .map(|result| result.rows_affected())
    }

    async fn anonymize_user(
        &mut self,
        user_id: UserId,
        at: OffsetDateTime,
    ) -> Result<WriteOutcome, RepositoryError> {
        let rows = query(
            "UPDATE users
             SET github_user_id = NULL, github_login = NULL, display_name = NULL,
                 avatar_url = NULL, updated_at = $2, anonymized_at = $2
             WHERE id = $1 AND anonymized_at IS NULL",
        )
        .bind(user_id.get())
        .bind(at)
        .execute(&mut *self.transaction)
        .await
        .map_err(map_sqlx)?
        .rows_affected();
        match rows {
            1 => Ok(WriteOutcome::Applied),
            0 => Ok(WriteOutcome::Unchanged),
            _ => Err(data_error(
                "single-row account anonymization affected multiple rows",
            )),
        }
    }

    async fn commit(self: Box<Self>) -> Result<(), RepositoryError> {
        self.transaction.commit().await.map_err(map_sqlx)
    }

    async fn rollback(self: Box<Self>) -> Result<(), RepositoryError> {
        self.transaction.rollback().await.map_err(map_sqlx)
    }
}
