use std::error::Error;
use uuid::Uuid;

use rux_application::{DashboardActivityKind, DashboardReader, NamespaceRole, UserId};
use rux_domain::SemanticVersion;
use rux_infrastructure::PostgresRepository;
use serde_json::json;
use sqlx::{PgPool, query, query_scalar};
use time::OffsetDateTime;

type TestResult = Result<(), Box<dyn Error>>;

#[sqlx::test(migrations = "../../migrations")]
async fn dashboard_is_bounded_scoped_and_role_aware(pool: PgPool) -> TestResult {
    let owner = add_user(&pool, 1, "owner").await?;
    let maintainer = add_user(&pool, 2, "maintainer").await?;
    let other = add_user(&pool, 3, "other").await?;
    let owner_namespace = add_namespace(&pool, "Owner_Tools").await?;
    let maintainer_namespace = add_namespace(&pool, "Maintainer_Tools").await?;
    let foreign_namespace = add_namespace(&pool, "Foreign_Tools").await?;
    let invitation_namespace = add_namespace(&pool, "Invited_Tools").await?;
    add_membership(&pool, owner_namespace, owner, "owner").await?;
    add_membership(&pool, maintainer_namespace, owner, "maintainer").await?;
    add_membership(&pool, foreign_namespace, other, "owner").await?;

    let owner_package = add_package(&pool, owner_namespace, "Owner_Package").await?;
    let maintainer_package = add_package(&pool, maintainer_namespace, "Maintainer_Package").await?;
    let foreign_package = add_package(&pool, foreign_namespace, "Foreign_Package").await?;
    let old_owner_version =
        add_version(&pool, owner_package, "1.0.0", "2026-06-01T00:00:00Z", false).await?;
    let owner_version =
        add_version(&pool, owner_package, "1.1.0", "2026-07-20T00:00:00Z", true).await?;
    let maintainer_version = add_version(
        &pool,
        maintainer_package,
        "2.0.0",
        "2026-07-25T00:00:00Z",
        false,
    )
    .await?;
    let foreign_version = add_version(
        &pool,
        foreign_package,
        "9.0.0",
        "2026-07-30T00:00:00Z",
        false,
    )
    .await?;

    add_download(&pool, old_owner_version, "2026-06-01T00:00:00Z").await?;
    add_download(&pool, owner_version, "2026-07-03T12:00:00Z").await?;
    add_download(&pool, owner_version, "2026-08-02T12:00:00Z").await?;
    add_download(&pool, maintainer_version, "2026-07-20T00:00:00Z").await?;
    add_download(&pool, foreign_version, "2026-07-20T00:00:00Z").await?;

    query(
        "INSERT INTO namespace_invitations (
             namespace_id, invited_user_id, invited_by_user_id, role, created_at, expires_at
         ) VALUES ($1, $2, $3, 'maintainer', $4, $5)",
    )
    .bind(invitation_namespace)
    .bind(owner)
    .bind(maintainer)
    .bind(timestamp("2026-08-01T00:00:00Z"))
    .bind(timestamp("2026-08-08T00:00:00Z"))
    .execute(&pool)
    .await?;

    add_audit(
        &pool,
        owner,
        "namespace_member_role_changed",
        "namespace",
        "owner-tools",
        json!({ "target_user_id": other.to_string(), "previous_role": "maintainer", "role": "owner" }),
        "2026-08-02T11:00:00Z",
    )
    .await?;
    add_audit(
        &pool,
        maintainer,
        "namespace_member_role_changed",
        "namespace",
        "maintainer-tools",
        json!({ "target_user_id": other.to_string(), "previous_role": "maintainer", "role": "owner" }),
        "2026-08-02T11:10:00Z",
    )
    .await?;
    add_audit(
        &pool,
        maintainer,
        "namespace_member_removed",
        "namespace",
        "maintainer-tools",
        json!({ "target_user_id": owner.to_string(), "previous_role": "maintainer" }),
        "2026-08-02T11:20:00Z",
    )
    .await?;
    add_audit(
        &pool,
        maintainer,
        "package_version_published",
        "package_version",
        &maintainer_version.to_string(),
        json!({ "namespace": "Maintainer_Tools", "package": "Maintainer_Package", "version": "2.0.0" }),
        "2026-08-02T11:30:00Z",
    )
    .await?;
    add_audit(
        &pool,
        other,
        "package_version_published",
        "package_version",
        &foreign_version.to_string(),
        json!({ "namespace": "Foreign_Tools", "package": "Foreign_Package", "version": "9.0.0" }),
        "2026-08-02T11:40:00Z",
    )
    .await?;

    let snapshot = PostgresRepository::new(pool)
        .dashboard_snapshot(
            UserId::new(owner),
            timestamp("2026-07-03T12:00:00Z"),
            timestamp("2026-08-02T12:00:00Z"),
            10,
            10,
            5,
        )
        .await?;

    assert_eq!(snapshot.namespace_count, 2);
    assert_eq!(snapshot.package_count, 2);
    assert_eq!(snapshot.invitation_count, 1);
    assert_eq!(
        snapshot
            .namespaces
            .iter()
            .map(|item| (item.namespace.as_str(), item.role, item.package_count))
            .collect::<Vec<_>>(),
        [
            ("Maintainer_Tools", NamespaceRole::Maintainer, 1),
            ("Owner_Tools", NamespaceRole::Owner, 1),
        ]
    );
    assert_eq!(snapshot.packages[0].package.as_str(), "Maintainer_Package");
    assert_eq!(snapshot.packages[1].version.as_str(), "1.1.0");
    assert!(snapshot.packages[1].yanked);
    assert_eq!(snapshot.packages[1].version_count, 2);
    assert_eq!(snapshot.invitations[0].namespace.as_str(), "Invited_Tools");
    assert_eq!(
        snapshot.invitations[0]
            .invited_by
            .as_ref()
            .map(|user| user.github_login.as_str()),
        Some("maintainer")
    );
    assert_eq!(
        snapshot
            .activity
            .iter()
            .map(|item| item.kind)
            .collect::<Vec<_>>(),
        [
            DashboardActivityKind::PackageVersionPublished,
            DashboardActivityKind::NamespaceMemberRemoved,
            DashboardActivityKind::NamespaceMemberRoleChanged,
        ]
    );
    assert_eq!(snapshot.downloads.total_30d, 3);
    assert_eq!(snapshot.downloads.total_all_time, 4);
    assert_eq!(
        snapshot.downloads.top_packages[0].package.as_str(),
        "Owner_Package"
    );
    assert_eq!(snapshot.downloads.top_packages[0].downloads_30d, 2);
    Ok(())
}

async fn add_user(pool: &PgPool, github_id: i64, login: &str) -> Result<Uuid, sqlx::Error> {
    query_scalar("INSERT INTO users (github_user_id, github_login) VALUES ($1, $2) RETURNING id")
        .bind(github_id)
        .bind(login)
        .fetch_one(pool)
        .await
}

async fn add_namespace(pool: &PgPool, name: &str) -> Result<Uuid, sqlx::Error> {
    query_scalar("INSERT INTO namespaces (display_name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
}

async fn add_membership(
    pool: &PgPool,
    namespace: Uuid,
    user: Uuid,
    role: &str,
) -> Result<(), sqlx::Error> {
    query("INSERT INTO namespace_owners (namespace_id, user_id, role) VALUES ($1, $2, $3)")
        .bind(namespace)
        .bind(user)
        .bind(role)
        .execute(pool)
        .await?;
    Ok(())
}

async fn add_package(pool: &PgPool, namespace: Uuid, name: &str) -> Result<Uuid, sqlx::Error> {
    query_scalar("INSERT INTO packages (namespace_id, display_name) VALUES ($1, $2) RETURNING id")
        .bind(namespace)
        .bind(name)
        .fetch_one(pool)
        .await
}

async fn add_version(
    pool: &PgPool,
    package: Uuid,
    value: &str,
    published_at: &str,
    yanked: bool,
) -> Result<Uuid, sqlx::Error> {
    let version = SemanticVersion::new(value).unwrap();
    query_scalar(
        "INSERT INTO package_versions (
             package_id, version, major, minor, patch, prerelease, build_metadata,
             manifest_schema_version, min_rux, package_type, normalized_manifest,
             artifact_sha256, artifact_size, storage_key, artifact_file_count,
             artifact_expanded_bytes, source_file_count, source_line_count,
             published_at, yanked_at
         ) VALUES (
             $1, $2, $3::NUMERIC, $4::NUMERIC, $5::NUMERIC, $6, $7,
             1, '0.4.0', 'shared_library', '{}', decode(repeat('ab', 32), 'hex'), 1,
             $8, 2, 1, 1, 0, $9,
             CASE WHEN $10 THEN $9::TIMESTAMPTZ + interval '1 second' END
         ) RETURNING id",
    )
    .bind(package)
    .bind(version.as_str())
    .bind(version.major().to_string())
    .bind(version.minor().to_string())
    .bind(version.patch().to_string())
    .bind(version.prerelease())
    .bind(version.build())
    .bind(format!(
        "packages/{package}/{}.ruxpkg",
        value.replace('+', "-")
    ))
    .bind(timestamp(published_at))
    .bind(yanked)
    .fetch_one(pool)
    .await
}

async fn add_download(pool: &PgPool, version: Uuid, occurred_at: &str) -> Result<(), sqlx::Error> {
    query("INSERT INTO download_events (package_version_id, occurred_at) VALUES ($1, $2)")
        .bind(version)
        .bind(timestamp(occurred_at))
        .execute(pool)
        .await?;
    Ok(())
}

async fn add_audit(
    pool: &PgPool,
    actor: Uuid,
    action: &str,
    subject_type: &str,
    subject_key: &str,
    metadata: serde_json::Value,
    occurred_at: &str,
) -> Result<(), sqlx::Error> {
    query(
        "INSERT INTO audit_records (
             actor_user_id, action, subject_type, subject_key, metadata, occurred_at
         ) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(actor)
    .bind(action)
    .bind(subject_type)
    .bind(subject_key)
    .bind(metadata)
    .bind(timestamp(occurred_at))
    .execute(pool)
    .await?;
    Ok(())
}

fn timestamp(value: &str) -> OffsetDateTime {
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).unwrap()
}
