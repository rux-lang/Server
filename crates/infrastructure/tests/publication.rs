use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use async_trait::async_trait;
use rux_application::{
    ApiTokenService, ApiTokens, ArtifactSha256, ArtifactStorage, ArtifactStorageError,
    ArtifactStorageErrorKind, ArtifactUpload, Clock, CredentialGenerationError,
    CredentialGenerator, GitHubUserProfile, IssueApiToken, NamespaceRole, PackageKind,
    PublicationErrorKind, PublicationMetadata, PublicationService, PublicationWorkflow,
    Publications, StoredArtifact, TokenScope, UnitOfWork, UserId,
};
use rux_domain::{IdentitySegment, SemanticVersion};
use rux_infrastructure::PostgresRepository;
use serde_json::Map;
use sqlx::{PgPool, query, query_scalar};
use time::{Duration, OffsetDateTime};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct FixedClock(OffsetDateTime);

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        self.0
    }
}

struct SequencedCredentials(AtomicU8);

impl CredentialGenerator for SequencedCredentials {
    fn generate(&self) -> Result<[u8; 32], CredentialGenerationError> {
        let sequence = self.0.fetch_add(1, Ordering::Relaxed);
        let mut credential = [7; 32];
        credential[0] = sequence;
        Ok(credential)
    }
}

struct FailingStorage;

#[async_trait]
impl ArtifactStorage for FailingStorage {
    async fn store(&self, _: ArtifactUpload) -> Result<StoredArtifact, ArtifactStorageError> {
        Err(ArtifactStorageError::new(
            ArtifactStorageErrorKind::UploadUnavailable,
        ))
    }
}

struct Fixture {
    publisher: Arc<PublicationService>,
    owner_id: UserId,
    maintainer_id: UserId,
    owner_credential: String,
    maintainer_credential: String,
}

#[sqlx::test(migrations = "../../migrations")]
async fn owners_and_maintainers_publish_one_immutable_aggregate(pool: PgPool) -> TestResult {
    let fixture = fixture(&pool).await?;

    let owner = fixture
        .publisher
        .prepare(
            &fixture.owner_credential,
            metadata("Rux_Tools", "Example_Pkg", "1.0.0"),
        )
        .await?
        .complete(artifact("owner-1.0.0"))
        .await?;
    let maintainer = fixture
        .publisher
        .prepare(
            &fixture.maintainer_credential,
            metadata("rux-tools", "example-pkg", "1.1.0"),
        )
        .await?
        .complete(artifact("maintainer-1.1.0"))
        .await?;

    assert_eq!(owner.namespace.as_str(), "Rux_Tools");
    assert_eq!(owner.package.as_str(), "Example_Pkg");
    assert_eq!(maintainer.package.as_str(), "Example_Pkg");
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM packages")
            .fetch_one(&pool)
            .await?,
        1
    );
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM package_versions")
            .fetch_one(&pool)
            .await?,
        2
    );
    assert_eq!(
        query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action = 'package_version_published'",
        )
        .fetch_one(&pool)
        .await?,
        2
    );
    let actor_ids = query_scalar::<_, i64>(
        "SELECT count(DISTINCT actor_user_id)
         FROM audit_records
         WHERE action = 'package_version_published'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(actor_ids, 2);
    assert!(
        query_scalar::<_, bool>(
            "SELECT bool_and(last_used_at IS NOT NULL) FROM api_tokens WHERE user_id IN ($1, $2)",
        )
        .bind(fixture.owner_id.get())
        .bind(fixture.maintainer_id.get())
        .fetch_one(&pool)
        .await?
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn publication_enforces_scope_membership_and_normalized_blocks(pool: PgPool) -> TestResult {
    let fixture = fixture(&pool).await?;
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let now = OffsetDateTime::now_utc() + Duration::days(1);
    let token_service = Arc::new(ApiTokenService::new(
        repository.clone(),
        Arc::new(FixedClock(now)),
        Arc::new(SequencedCredentials(AtomicU8::new(100))),
    ));
    let outsider = create_user(&repository, 3003, "Outsider-One").await?;
    let outsider_token = token_service
        .issue(
            outsider.id,
            IssueApiToken {
                display_name: "Outsider publish".into(),
                scopes: vec![TokenScope::Publish],
                expires_at: None,
            },
        )
        .await?;
    let wrong_scope = token_service
        .issue(
            fixture.owner_id,
            IssueApiToken {
                display_name: "Yank only".into(),
                scopes: vec![TokenScope::Yank],
                expires_at: None,
            },
        )
        .await?;

    assert_prepare_error(
        &fixture.publisher,
        &wrong_scope.credential,
        metadata("Rux_Tools", "Example", "1.0.0"),
        PublicationErrorKind::InsufficientScope,
    )
    .await;
    assert_prepare_error(
        &fixture.publisher,
        &outsider_token.credential,
        metadata("Rux_Tools", "Example", "1.0.0"),
        PublicationErrorKind::Forbidden,
    )
    .await;
    assert_prepare_error(
        &fixture.publisher,
        &fixture.owner_credential,
        metadata("Missing", "Example", "1.0.0"),
        PublicationErrorKind::NamespaceNotFound,
    )
    .await;

    query(
        "INSERT INTO blocked_identities (identity_kind, display_name)
         VALUES ('namespace', 'RUX-TOOLS')",
    )
    .execute(&pool)
    .await?;
    assert_prepare_error(
        &fixture.publisher,
        &fixture.owner_credential,
        metadata("rux_tools", "Example", "1.0.0"),
        PublicationErrorKind::NamespaceBlocked,
    )
    .await;
    query("DELETE FROM blocked_identities WHERE identity_kind = 'namespace'")
        .execute(&pool)
        .await?;
    query(
        "INSERT INTO blocked_identities (identity_kind, display_name)
         VALUES ('package', 'EXAMPLE-PKG')",
    )
    .execute(&pool)
    .await?;
    assert_prepare_error(
        &fixture.publisher,
        &fixture.owner_credential,
        metadata("Rux_Tools", "example_pkg", "1.0.0"),
        PublicationErrorKind::PackageBlocked,
    )
    .await;

    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM package_versions")
            .fetch_one(&pool)
            .await?,
        0
    );
    assert_eq!(
        query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action = 'package_version_published'",
        )
        .fetch_one(&pool)
        .await?,
        0
    );
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM api_tokens WHERE last_used_at IS NOT NULL",)
            .fetch_one(&pool)
            .await?,
        0
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn audit_failure_rolls_back_publication_and_token_use(pool: PgPool) -> TestResult {
    let fixture = fixture(&pool).await?;
    query(
        "CREATE FUNCTION reject_publication_audit()
         RETURNS TRIGGER
         LANGUAGE plpgsql
         AS $$
         BEGIN
             RAISE EXCEPTION 'reject publication audit';
         END;
         $$",
    )
    .execute(&pool)
    .await?;
    query(
        "CREATE TRIGGER reject_publication_audit
         BEFORE INSERT ON audit_records
         FOR EACH ROW EXECUTE FUNCTION reject_publication_audit()",
    )
    .execute(&pool)
    .await?;

    let error = fixture
        .publisher
        .prepare(
            &fixture.owner_credential,
            metadata("Rux_Tools", "Atomic", "1.0.0"),
        )
        .await?
        .complete(artifact("atomic-1.0.0"))
        .await
        .expect_err("audit failure should fail closed");
    assert_eq!(error.kind(), PublicationErrorKind::Unavailable);
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM packages")
            .fetch_one(&pool)
            .await?,
        0
    );
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM package_versions")
            .fetch_one(&pool)
            .await?,
        0
    );
    assert_eq!(
        query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action = 'package_version_published'",
        )
        .fetch_one(&pool)
        .await?,
        0
    );
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM api_tokens WHERE last_used_at IS NOT NULL",)
            .fetch_one(&pool)
            .await?,
        0
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn prepared_transaction_serializes_version_races_and_abort_releases_it(
    pool: PgPool,
) -> TestResult {
    let fixture = fixture(&pool).await?;
    let first = fixture
        .publisher
        .prepare(
            &fixture.owner_credential,
            metadata("Rux_Tools", "Concurrent", "1.0.0+linux"),
        )
        .await?;

    let competing_service = fixture.publisher.clone();
    let competing_credential = fixture.maintainer_credential.clone();
    let competing = tokio::spawn(async move {
        competing_service
            .prepare(
                &competing_credential,
                metadata("rux-tools", "concurrent", "1.0.0+linux"),
            )
            .await
    });
    tokio::task::yield_now().await;
    assert!(!competing.is_finished());

    first.complete(artifact("concurrent-linux")).await?;
    let conflict = competing
        .await?
        .err()
        .expect("the exact version should lose");
    assert_eq!(conflict.kind(), PublicationErrorKind::VersionConflict);

    fixture
        .publisher
        .prepare(
            &fixture.maintainer_credential,
            metadata("rux-tools", "concurrent", "1.0.0+windows"),
        )
        .await?
        .complete(artifact("concurrent-windows"))
        .await?;

    let aborted = fixture
        .publisher
        .prepare(
            &fixture.owner_credential,
            metadata("Rux_Tools", "Concurrent", "2.0.0"),
        )
        .await?;
    let retry_service = fixture.publisher.clone();
    let retry_credential = fixture.maintainer_credential.clone();
    let retry = tokio::spawn(async move {
        retry_service
            .prepare(
                &retry_credential,
                metadata("rux-tools", "concurrent", "2.0.0"),
            )
            .await?
            .complete(artifact("retry-2.0.0"))
            .await
    });
    tokio::task::yield_now().await;
    assert!(!retry.is_finished());
    aborted.abort().await?;
    retry.await??;

    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM packages")
            .fetch_one(&pool)
            .await?,
        1
    );
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM package_versions")
            .fetch_one(&pool)
            .await?,
        3
    );
    assert_eq!(
        query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action = 'package_version_published'",
        )
        .fetch_one(&pool)
        .await?,
        3
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn storage_failure_aborts_the_prepared_publication(pool: PgPool) -> TestResult {
    let fixture = fixture(&pool).await?;
    let workflow = PublicationWorkflow::new(fixture.publisher.clone(), Arc::new(FailingStorage));
    let failed_metadata = metadata("Rux_Tools", "Storage_Failure", "1.0.0");
    let error = workflow
        .publish(
            &fixture.owner_credential,
            failed_metadata,
            upload("Rux_Tools", "Storage_Failure", "1.0.0"),
        )
        .await
        .expect_err("storage failure should fail publication");
    assert_eq!(error.kind(), PublicationErrorKind::Unavailable);

    fixture
        .publisher
        .prepare(
            &fixture.owner_credential,
            metadata("Rux_Tools", "Storage_Failure", "1.0.0"),
        )
        .await?
        .complete(artifact("storage-retry"))
        .await?;
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM package_versions")
            .fetch_one(&pool)
            .await?,
        1
    );
    Ok(())
}

async fn fixture(pool: &PgPool) -> Result<Fixture, Box<dyn std::error::Error>> {
    let repository = Arc::new(PostgresRepository::new(pool.clone()));
    let owner = create_user(&repository, 3001, "Owner-One").await?;
    let maintainer = create_user(&repository, 3002, "Maintainer-One").await?;
    let mut transaction = repository.begin().await?;
    let namespace = transaction
        .create_namespace(&identity("Rux_Tools"), Some(owner.id))
        .await?;
    transaction
        .set_namespace_owner(namespace.id, owner.id, NamespaceRole::Owner, Some(owner.id))
        .await?;
    transaction
        .set_namespace_owner(
            namespace.id,
            maintainer.id,
            NamespaceRole::Maintainer,
            Some(owner.id),
        )
        .await?;
    transaction.commit().await?;

    let now = OffsetDateTime::now_utc() + Duration::days(1);
    let token_service = Arc::new(ApiTokenService::new(
        repository.clone(),
        Arc::new(FixedClock(now)),
        Arc::new(SequencedCredentials(AtomicU8::new(1))),
    ));
    let owner_token = token_service
        .issue(
            owner.id,
            IssueApiToken {
                display_name: "Owner publish".into(),
                scopes: vec![TokenScope::Publish],
                expires_at: None,
            },
        )
        .await?;
    let maintainer_token = token_service
        .issue(
            maintainer.id,
            IssueApiToken {
                display_name: "Maintainer publish".into(),
                scopes: vec![TokenScope::Publish],
                expires_at: None,
            },
        )
        .await?;
    let publisher = Arc::new(PublicationService::new(repository, token_service));

    Ok(Fixture {
        publisher,
        owner_id: owner.id,
        maintainer_id: maintainer.id,
        owner_credential: owner_token.credential,
        maintainer_credential: maintainer_token.credential,
    })
}

async fn create_user(
    repository: &PostgresRepository,
    github_id: u64,
    login: &str,
) -> Result<rux_application::UserRecord, Box<dyn std::error::Error>> {
    let mut transaction = repository.begin().await?;
    let user = transaction
        .upsert_github_user(&GitHubUserProfile {
            github_user_id: github_id,
            github_login: login.into(),
            display_name: Some(login.into()),
            avatar_url: None,
        })
        .await?;
    transaction.commit().await?;
    Ok(user)
}

async fn assert_prepare_error(
    publisher: &PublicationService,
    credential: &str,
    metadata: PublicationMetadata,
    expected: PublicationErrorKind,
) {
    let error = publisher
        .prepare(credential, metadata)
        .await
        .err()
        .expect("publication should fail");
    assert_eq!(error.kind(), expected);
}

fn metadata(namespace: &str, package: &str, version: &str) -> PublicationMetadata {
    PublicationMetadata {
        namespace: identity(namespace),
        package: identity(package),
        version: semantic_version(version),
        manifest_schema_version: 1,
        min_rux: semantic_version("0.4.0"),
        package_type: PackageKind::Source,
        description: Some("Publication transaction fixture".into()),
        repository_url: None,
        homepage_url: None,
        readme: Some(("README.md".into(), "# Example".into())),
        license_expression: Some("MIT".into()),
        license_url: None,
        normalized_manifest: Map::new(),
        artifact_file_count: 2,
        artifact_expanded_bytes: 2048,
        source_file_count: 1,
        source_line_count: 10,
        authors: vec!["Rux Contributors".into()],
        keywords: vec![identity("Registry")],
        dependencies: Vec::new(),
    }
}

fn artifact(suffix: &str) -> StoredArtifact {
    StoredArtifact {
        sha256: ArtifactSha256::new([4; 32]),
        byte_size: 1024,
        storage_key: format!("packages/{suffix}.ruxpkg"),
    }
}

fn upload(namespace: &str, package: &str, version: &str) -> ArtifactUpload {
    ArtifactUpload {
        path: "unused-by-failing-storage.ruxpkg".into(),
        namespace: identity(namespace),
        package: identity(package),
        version: semantic_version(version),
        sha256: ArtifactSha256::new([4; 32]),
        byte_size: 1024,
    }
}

fn identity(value: &str) -> IdentitySegment {
    IdentitySegment::new(value).expect("valid identity fixture")
}

fn semantic_version(value: &str) -> SemanticVersion {
    SemanticVersion::new(value).expect("valid version fixture")
}
