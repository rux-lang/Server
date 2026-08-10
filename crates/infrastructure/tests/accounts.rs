use std::sync::Arc;
use uuid::Uuid;

use rux_application::{
    ANONYMIZED_TOKEN_NAME, AccountLifecycle, AccountLifecycleErrorKind, AccountLifecycleService,
    ApiTokenService, Clock, NamespaceActor, NamespaceRole, NamespaceService, Namespaces,
    TokenAuthorizer, UserId,
};
use rux_infrastructure::{OsCredentialGenerator, PostgresRepository};
use sqlx::{PgPool, Row, query, query_scalar};
use time::{Duration, OffsetDateTime};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct FixedClock(OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn deletion_anonymizes_security_and_membership_data_but_preserves_history(
    pool: PgPool,
) -> TestResult {
    let deleted_user = insert_user(&pool, 7001, "Delete-Me").await?;
    let co_owner = insert_user(&pool, 7002, "Co-Owner").await?;
    let invitee = insert_user(&pool, 7003, "Invitee").await?;
    let namespace = insert_namespace(&pool, "History", deleted_user).await?;
    let incoming_namespace = insert_namespace(&pool, "Incoming", co_owner).await?;
    add_owner(&pool, namespace, deleted_user, "owner").await?;
    add_owner(&pool, namespace, co_owner, "owner").await?;
    add_owner(&pool, incoming_namespace, co_owner, "owner").await?;

    let incoming_invitation = query_scalar::<_, Uuid>(
        "INSERT INTO namespace_invitations
             (namespace_id, invited_user_id, invited_by_user_id, role, expires_at)
         VALUES ($1, $2, $3, 'maintainer', now() + interval '7 days') RETURNING id",
    )
    .bind(incoming_namespace)
    .bind(deleted_user)
    .bind(co_owner)
    .fetch_one(&pool)
    .await?;
    let outgoing_invitation = query_scalar::<_, Uuid>(
        "INSERT INTO namespace_invitations
             (namespace_id, invited_user_id, invited_by_user_id, role, expires_at)
         VALUES ($1, $2, $3, 'maintainer', now() + interval '7 days') RETURNING id",
    )
    .bind(namespace)
    .bind(invitee)
    .bind(deleted_user)
    .fetch_one(&pool)
    .await?;

    query(
        "INSERT INTO sessions (user_id, secret_hash, csrf_hash, expires_at)
         VALUES ($1, $2, $3, now() + interval '30 days')",
    )
    .bind(deleted_user)
    .bind(vec![1_u8; 32])
    .bind(vec![2_u8; 32])
    .execute(&pool)
    .await?;
    let token = query_scalar::<_, Uuid>(
        "INSERT INTO api_tokens (user_id, display_name, token_prefix, secret_hash)
         VALUES ($1, 'Personal release', 'rux_pat_delete1', $2) RETURNING id",
    )
    .bind(deleted_user)
    .bind(vec![3_u8; 32])
    .fetch_one(&pool)
    .await?;
    query("INSERT INTO api_token_scopes (api_token_id, scope) VALUES ($1, 'publish')")
        .bind(token)
        .execute(&pool)
        .await?;

    let package = query_scalar::<_, Uuid>(
        "INSERT INTO packages (namespace_id, display_name, created_by_user_id)
         VALUES ($1, 'Archive', $2) RETURNING id",
    )
    .bind(namespace)
    .bind(deleted_user)
    .fetch_one(&pool)
    .await?;
    let version = query_scalar::<_, Uuid>(
        "INSERT INTO package_versions (
             package_id, version, major, minor, patch, manifest_schema_version, min_rux,
             package_type, normalized_manifest, artifact_sha256, artifact_size, storage_key,
             artifact_file_count, artifact_expanded_bytes, source_file_count, source_line_count,
             published_by_user_id
         ) VALUES (
             $1, '1.0.0', 1, 0, 0, 1, '0.4.0', 'source_library', '{}'::jsonb, $2, 128,
             'history/archive/1.0.0/package.ruxpkg', 2, 256, 1, 10, $3
         ) RETURNING id",
    )
    .bind(package)
    .bind(vec![4_u8; 32])
    .bind(deleted_user)
    .fetch_one(&pool)
    .await?;

    let candidate = OffsetDateTime::now_utc() + Duration::minutes(1);
    let deleted_at = candidate.replace_nanosecond(candidate.nanosecond() / 1_000 * 1_000)?;
    let service = AccountLifecycleService::new(
        Arc::new(PostgresRepository::new(pool.clone())),
        Arc::new(FixedClock(deleted_at)),
    );
    service
        .delete_account(UserId::new(deleted_user), "Delete-Me")
        .await?;

    let identity = sqlx::query(
        "SELECT github_user_id::text AS github_user_id, github_login, display_name, avatar_url, anonymized_at
         FROM users WHERE id = $1",
    )
    .bind(deleted_user)
    .fetch_one(&pool)
    .await?;
    assert!(
        identity
            .try_get::<Option<String>, _>("github_login")?
            .is_none()
    );
    assert!(
        identity
            .try_get::<Option<String>, _>("display_name")?
            .is_none()
    );
    assert!(
        identity
            .try_get::<Option<String>, _>("avatar_url")?
            .is_none()
    );
    assert!(
        identity
            .try_get::<Option<String>, _>("github_user_id")?
            .is_none()
    );
    assert_eq!(
        identity.try_get::<Option<OffsetDateTime>, _>("anonymized_at")?,
        Some(deleted_at)
    );
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM namespace_owners WHERE user_id = $1")
            .bind(deleted_user)
            .fetch_one(&pool)
            .await?,
        0
    );
    assert_eq!(
        query_scalar::<_, Option<OffsetDateTime>>(
            "SELECT revoked_at FROM namespace_invitations WHERE id = $1",
        )
        .bind(incoming_invitation)
        .fetch_one(&pool)
        .await?,
        Some(deleted_at)
    );
    assert!(
        query_scalar::<_, Option<OffsetDateTime>>(
            "SELECT revoked_at FROM namespace_invitations WHERE id = $1",
        )
        .bind(outgoing_invitation)
        .fetch_one(&pool)
        .await?
        .is_none()
    );
    assert_eq!(
        query_scalar::<_, String>("SELECT display_name FROM api_tokens WHERE id = $1")
            .bind(token)
            .fetch_one(&pool)
            .await?,
        ANONYMIZED_TOKEN_NAME
    );
    assert_eq!(
        query_scalar::<_, Option<OffsetDateTime>>(
            "SELECT revoked_at FROM sessions WHERE user_id = $1"
        )
        .bind(deleted_user)
        .fetch_one(&pool)
        .await?,
        Some(deleted_at)
    );
    assert_eq!(
        query_scalar::<_, Option<Uuid>>(
            "SELECT published_by_user_id FROM package_versions WHERE id = $1",
        )
        .bind(version)
        .fetch_one(&pool)
        .await?,
        Some(deleted_user)
    );
    assert_eq!(
        query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records
             WHERE actor_user_id = $1 AND action = 'account_anonymized'",
        )
        .bind(deleted_user)
        .fetch_one(&pool)
        .await?,
        1
    );

    let replacement = insert_user(&pool, 7001, "Delete-Me").await?;
    assert_ne!(replacement, deleted_user);
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn sole_owner_conflict_rolls_back_without_partial_cleanup(pool: PgPool) -> TestResult {
    let user = insert_user(&pool, 7101, "Last-Owner").await?;
    let namespace = insert_namespace(&pool, "Sole", user).await?;
    add_owner(&pool, namespace, user, "owner").await?;
    query(
        "INSERT INTO sessions (user_id, secret_hash, csrf_hash, expires_at)
         VALUES ($1, $2, $3, now() + interval '30 days')",
    )
    .bind(user)
    .bind(vec![8_u8; 32])
    .bind(vec![9_u8; 32])
    .execute(&pool)
    .await?;

    let service = AccountLifecycleService::new(
        Arc::new(PostgresRepository::new(pool.clone())),
        Arc::new(FixedClock(OffsetDateTime::now_utc() + Duration::minutes(1))),
    );
    let error = service
        .delete_account(UserId::new(user), "Last-Owner")
        .await
        .expect_err("sole ownership should block deletion");
    assert_eq!(error.kind(), AccountLifecycleErrorKind::LastOwner);
    assert_eq!(
        query_scalar::<_, Option<String>>("SELECT github_login FROM users WHERE id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await?,
        Some("Last-Owner".into())
    );
    assert!(
        query_scalar::<_, Option<OffsetDateTime>>(
            "SELECT revoked_at FROM sessions WHERE user_id = $1"
        )
        .bind(user)
        .fetch_one(&pool)
        .await?
        .is_none()
    );
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM namespace_owners WHERE user_id = $1")
            .bind(user)
            .fetch_one(&pool)
            .await?,
        1
    );
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM audit_records")
            .fetch_one(&pool)
            .await?,
        0
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_demotion_and_deletion_never_leave_a_namespace_without_an_owner(
    pool: PgPool,
) -> TestResult {
    let deleting_user = insert_user(&pool, 7201, "Deleting-Owner").await?;
    let other_owner = insert_user(&pool, 7202, "Other-Owner").await?;
    let namespace = insert_namespace(&pool, "Concurrent", deleting_user).await?;
    add_owner(&pool, namespace, deleting_user, "owner").await?;
    add_owner(&pool, namespace, other_owner, "owner").await?;

    let now = OffsetDateTime::now_utc() + Duration::minutes(1);
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let token_service = Arc::new(ApiTokenService::new(
        repository.clone(),
        Arc::new(FixedClock(now)),
        Arc::new(OsCredentialGenerator),
    ));
    let token_authorizer: Arc<dyn TokenAuthorizer> = token_service;
    let namespaces = NamespaceService::new(
        repository.clone(),
        Arc::new(FixedClock(now)),
        token_authorizer,
    );
    let accounts = AccountLifecycleService::new(repository, Arc::new(FixedClock(now)));

    let (deletion, demotion) = tokio::join!(
        accounts.delete_account(UserId::new(deleting_user), "Deleting-Owner"),
        namespaces.set_member_role(
            NamespaceActor::Session(UserId::new(deleting_user)),
            "Concurrent",
            "Other-Owner",
            NamespaceRole::Maintainer,
        ),
    );

    assert!(deletion.is_ok() || demotion.is_ok());
    let active_owners = query_scalar::<_, i64>(
        "SELECT count(*)
         FROM namespace_owners o
         JOIN users u ON u.id = o.user_id
         WHERE o.namespace_id = $1 AND o.role = 'owner' AND u.anonymized_at IS NULL",
    )
    .bind(namespace)
    .fetch_one(&pool)
    .await?;
    assert!(active_owners >= 1);
    Ok(())
}

async fn insert_user(pool: &PgPool, github_id: i64, login: &str) -> Result<Uuid, sqlx::Error> {
    query_scalar(
        "INSERT INTO users (github_user_id, github_login, display_name, avatar_url)
         VALUES ($1, $2, $2, 'https://example.test/avatar.png') RETURNING id",
    )
    .bind(github_id)
    .bind(login)
    .fetch_one(pool)
    .await
}

async fn insert_namespace(pool: &PgPool, name: &str, creator: Uuid) -> Result<Uuid, sqlx::Error> {
    query_scalar(
        "INSERT INTO namespaces (display_name, created_by_user_id) VALUES ($1, $2) RETURNING id",
    )
    .bind(name)
    .bind(creator)
    .fetch_one(pool)
    .await
}

async fn add_owner(
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
