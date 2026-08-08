use std::error::Error;
use std::sync::Arc;
use uuid::Uuid;

use rux_application::{
    AccountReader, AccountUnitOfWork, ApiTokenService, ApiTokens, ArtifactReferenceReader,
    ArtifactSha256, AuditActor, AuditEvent, CatalogReader, DependencyRecord, DiscoveryReader,
    DownloadReader, DownloadUnitOfWork, GitHubUserProfile, InvitationResolution, IssueApiToken,
    NamespaceReader, NamespaceRole, NewApiToken, NewInvitation, NewPackageVersion, NewSession,
    PackageId, PackageKind, PackageMetadataReader, RepositoryConflict, RepositoryErrorKind,
    ResolverIndexReader, SecretHash, TokenReader, TokenScope, UnitOfWork, WriteOutcome,
    YankErrorKind, YankService, Yanks,
};
use rux_domain::{IdentitySegment, SemanticVersion, VersionRange};
use rux_infrastructure::{OsCredentialGenerator, PostgresRepository, SystemClock};
use serde_json::{Map, Value};
use sqlx::{PgPool, query_scalar};
use time::{Duration, OffsetDateTime};

type TestResult = Result<(), Box<dyn Error>>;

#[sqlx::test(migrations = "../../migrations")]
async fn account_sessions_commit_and_rollback_explicitly(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool);
    let profile = profile(101, "Owner-One");

    let mut rolled_back = repository.begin().await?;
    rolled_back.upsert_github_user(&profile).await?;
    rolled_back.rollback().await?;
    assert!(repository.user_by_github_id(101).await?.is_none());

    let mut transaction = repository.begin().await?;
    let user = transaction.upsert_github_user(&profile).await?;
    let secret = SecretHash::new([1; 32]);
    let session = transaction
        .create_session(&NewSession {
            user_id: user.id,
            secret_hash: secret,
            csrf_hash: SecretHash::new([2; 32]),
            expires_at: OffsetDateTime::now_utc() + Duration::hours(1),
        })
        .await?;
    assert_eq!(
        transaction
            .touch_session(session.id, session.last_seen_at + Duration::seconds(1))
            .await?,
        WriteOutcome::Applied
    );
    transaction.commit().await?;

    let stored = repository
        .session_by_secret_hash(secret)
        .await?
        .expect("committed session");
    assert_eq!(stored.user_id, user.id);
    assert_eq!(stored.csrf_hash, SecretHash::new([2; 32]));
    let mut locked = repository.begin_account().await?;
    assert_eq!(
        locked
            .lock_session_by_secret_hash(secret)
            .await?
            .expect("session should lock")
            .id,
        stored.id
    );
    locked.rollback().await?;

    let replacement_secret = SecretHash::new([3; 32]);
    let mut rotation = repository.begin_account().await?;
    let current = rotation
        .lock_session_by_secret_hash(secret)
        .await?
        .expect("current session should lock");
    let replacement = rotation
        .create_session(&NewSession {
            user_id: current.user_id,
            secret_hash: replacement_secret,
            csrf_hash: SecretHash::new([4; 32]),
            expires_at: current.expires_at,
        })
        .await?;
    assert_eq!(
        rotation
            .revoke_session(current.id, OffsetDateTime::now_utc())
            .await?,
        WriteOutcome::Applied
    );
    rotation.commit().await?;
    assert!(
        repository
            .session_by_secret_hash(secret)
            .await?
            .expect("old session remains auditable")
            .revoked_at
            .is_some()
    );
    assert_eq!(
        repository
            .session_by_secret_hash(replacement_secret)
            .await?
            .expect("replacement session should commit")
            .expires_at,
        replacement.expires_at
    );

    let mut outcomes = repository.begin().await?;
    assert_eq!(
        outcomes
            .touch_session(stored.id, stored.last_seen_at)
            .await?,
        WriteOutcome::Unchanged
    );
    assert_eq!(
        outcomes
            .revoke_session(
                rux_application::SessionId::new(Uuid::nil()),
                OffsetDateTime::now_utc(),
            )
            .await?,
        WriteOutcome::NotFound
    );
    outcomes.rollback().await?;
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn github_login_changes_preserve_normalized_login_uniqueness(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool);
    let first = committed_user(&repository, profile(111, "First-Owner")).await?;
    committed_user(&repository, profile(112, "Second-Owner")).await?;

    let mut transaction = repository.begin().await?;
    let error = transaction
        .upsert_github_user(&profile(111, "second-owner"))
        .await
        .expect_err("a GitHub login cannot move onto another account");
    assert_eq!(
        error.kind(),
        RepositoryErrorKind::Conflict(RepositoryConflict::GitHubLogin)
    );
    transaction.rollback().await?;

    let stored = repository
        .user_by_github_id(111)
        .await?
        .expect("first account should remain present");
    assert_eq!(stored.id, first.id);
    assert_eq!(stored.github_login.as_deref(), Some("First-Owner"));
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn namespace_invitation_and_token_capabilities_round_trip(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool);
    let owner = committed_user(&repository, profile(201, "Owner-Two")).await?;
    let invitee = committed_user(&repository, profile(202, "Invitee-Two")).await?;

    let mut transaction = repository.begin().await?;
    let namespace = transaction
        .create_namespace(&identity("Rux_Tools"), Some(owner.id))
        .await?;
    transaction
        .set_namespace_owner(namespace.id, owner.id, NamespaceRole::Owner, Some(owner.id))
        .await?;
    let invitation = transaction
        .create_invitation(&NewInvitation {
            namespace_id: namespace.id,
            invited_user_id: invitee.id,
            invited_by_user_id: Some(owner.id),
            role: NamespaceRole::Maintainer,
            expires_at: OffsetDateTime::now_utc() + Duration::days(7),
        })
        .await?;
    let token_hash = SecretHash::new([3; 32]);
    let token = transaction
        .create_token(&NewApiToken {
            user_id: owner.id,
            display_name: "Release token".into(),
            token_prefix: "rux_test_1234".into(),
            secret_hash: token_hash,
            scopes: vec![TokenScope::Namespace, TokenScope::Yank, TokenScope::Publish],
            expires_at: None,
        })
        .await?;
    transaction
        .append_audit(&AuditEvent::namespace_created(
            AuditActor::token(owner.id, token.id),
            &namespace.name,
        ))
        .await?;
    transaction.commit().await?;

    let found = repository
        .namespace_by_name(&identity("rux-tools"))
        .await?
        .expect("normalized namespace lookup");
    assert_eq!(found.name.as_str(), "Rux_Tools");
    assert_eq!(
        repository
            .namespace_role(found.id, owner.id)
            .await?
            .expect("owner")
            .role,
        NamespaceRole::Owner
    );
    assert_eq!(
        repository
            .token_by_secret_hash(token_hash)
            .await?
            .expect("token")
            .scopes,
        vec![TokenScope::Publish, TokenScope::Yank, TokenScope::Namespace]
    );
    let stored_tokens = repository.tokens_by_user_id(owner.id).await?;
    assert_eq!(stored_tokens.len(), 1);
    assert_eq!(&stored_tokens[0], &token);
    let mut locked_token = repository.begin().await?;
    assert_eq!(
        locked_token
            .lock_token_by_prefix(owner.id, &token.token_prefix)
            .await?
            .expect("owner-scoped token prefix should lock"),
        token
    );
    assert!(
        locked_token
            .lock_token_by_prefix(invitee.id, &token.token_prefix)
            .await?
            .is_none()
    );
    locked_token.rollback().await?;

    let mut resolution = repository.begin().await?;
    assert_eq!(
        resolution
            .resolve_invitation(
                invitation.id,
                InvitationResolution::Accepted,
                OffsetDateTime::now_utc(),
            )
            .await?,
        WriteOutcome::Applied
    );
    resolution.commit().await?;
    assert!(
        repository
            .invitation_by_id(invitation.id)
            .await?
            .expect("invitation")
            .accepted_at
            .is_some()
    );

    let mut duplicate = repository.begin().await?;
    let error = duplicate
        .create_namespace(&identity("RUX-TOOLS"), Some(owner.id))
        .await
        .expect_err("normalized collision");
    assert_eq!(
        error.kind(),
        RepositoryErrorKind::Conflict(RepositoryConflict::NamespaceIdentity)
    );
    duplicate.rollback().await?;

    let mut atomic = repository.begin().await?;
    atomic
        .create_namespace(&identity("Transient"), Some(owner.id))
        .await?;
    let error = atomic
        .create_namespace(&identity("rux_tools"), Some(owner.id))
        .await
        .expect_err("second write should fail");
    assert_eq!(
        error.kind(),
        RepositoryErrorKind::Conflict(RepositoryConflict::NamespaceIdentity)
    );
    atomic.rollback().await?;
    assert!(
        repository
            .namespace_by_name(&identity("transient"))
            .await?
            .is_none()
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn complete_package_version_aggregate_round_trips(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let publisher = committed_user(&repository, profile(301, "Publisher-One")).await?;

    let mut transaction = repository.begin().await?;
    let namespace = transaction
        .create_namespace(&identity("Rux"), Some(publisher.id))
        .await?;
    let package = transaction
        .create_package(namespace.id, &identity("Example_Pkg"), Some(publisher.id))
        .await?;
    let mut manifest = Map::new();
    manifest.insert("Manifest".into(), Value::String("normalized".into()));
    let created = transaction
        .create_package_version(&NewPackageVersion {
            package_id: package.id,
            version: version("18446744073709551615.2.3+linux"),
            manifest_schema_version: 1,
            min_rux: version("0.4.0"),
            package_type: PackageKind::Source,
            description: Some("Example package".into()),
            repository_url: Some("https://github.com/rux-lang/example".into()),
            homepage_url: None,
            readme: Some(("README.md".into(), "# Example".into())),
            license_expression: Some("MIT".into()),
            license_url: Some("https://example.com/LICENSE".into()),
            normalized_manifest: manifest,
            artifact_sha256: ArtifactSha256::new([4; 32]),
            artifact_size: 1024,
            storage_key: "packages/rux/example/immutable.ruxpkg".into(),
            artifact_file_count: 3,
            artifact_expanded_bytes: 2048,
            source_file_count: 1,
            source_line_count: 12,
            published_by_user_id: Some(publisher.id),
            authors: vec!["Rux Contributors".into(), "Registry Team".into()],
            keywords: vec![identity("Registry_Tools"), identity("Example")],
            dependencies: vec![
                dependency("Zed", "Rux", "Zed", "^2"),
                dependency("Alpha", "Rux", "Alpha", ">=1, <2"),
            ],
        })
        .await?;
    transaction
        .append_download(created.id, OffsetDateTime::now_utc())
        .await?;
    transaction.commit().await?;

    let stored = repository
        .version_by_name(
            &identity("rux"),
            &identity("example-pkg"),
            &version("18446744073709551615.2.3+linux"),
        )
        .await?
        .expect("stored version");
    assert_eq!(stored.version.major(), u64::MAX);
    assert_eq!(stored.authors, vec!["Rux Contributors", "Registry Team"]);
    assert_eq!(stored.keywords[0].as_str(), "Registry_Tools");
    assert_eq!(stored.dependencies[0].alias.as_str(), "Alpha");
    assert_eq!(stored.artifact_sha256, ArtifactSha256::new([4; 32]));
    let summary = repository
        .package_summary_by_name(&identity("rux"), &identity("example-pkg"))
        .await?
        .expect("package summary");
    assert_eq!(summary.namespace.as_str(), "Rux");
    assert_eq!(summary.package.as_str(), "Example_Pkg");

    let metadata = repository
        .package_version_metadata_by_name(
            &identity("RUX"),
            &identity("EXAMPLE_PKG"),
            &version("18446744073709551615.2.3+linux"),
        )
        .await?
        .expect("package version metadata");
    assert_eq!(metadata.namespace.as_str(), "Rux");
    assert_eq!(metadata.package.as_str(), "Example_Pkg");
    assert_eq!(
        metadata.readme,
        Some(("README.md".into(), "# Example".into()))
    );
    assert_eq!(metadata.license_expression.as_deref(), Some("MIT"));
    assert_eq!(
        metadata.license_url.as_deref(),
        Some("https://example.com/LICENSE")
    );
    assert_eq!(metadata.normalized_manifest["Manifest"], "normalized");
    assert_eq!(metadata.dependencies[0].alias.as_str(), "Alpha");
    assert_eq!(metadata.artifact_sha256, ArtifactSha256::new([4; 32]));
    assert!(!metadata.yanked);
    assert!(
        repository
            .package_version_metadata_by_name(
                &identity("Rux"),
                &identity("Example_Pkg"),
                &version("1.0.0"),
            )
            .await?
            .is_none()
    );
    assert_eq!(
        repository
            .referenced_storage_keys(&[
                "packages/missing.ruxpkg".into(),
                stored.storage_key.clone(),
                stored.storage_key.clone(),
            ])
            .await?,
        vec![stored.storage_key.clone()]
    );
    assert!(repository.referenced_storage_keys(&[]).await?.is_empty());
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM download_events")
            .fetch_one(&pool)
            .await?,
        1
    );

    let target = repository
        .download_target_by_name(
            &identity("RUX"),
            &identity("EXAMPLE-PKG"),
            &version("18446744073709551615.2.3+linux"),
        )
        .await?
        .expect("normalized download target lookup");
    assert_eq!(target.package_version_id, created.id);
    assert_eq!(target.storage_key, stored.storage_key);

    let mut rolled_back_download = repository.begin_download().await?;
    rolled_back_download
        .append_download(created.id, OffsetDateTime::now_utc())
        .await?;
    rolled_back_download.rollback().await?;
    let mut committed_download = repository.begin_download().await?;
    committed_download
        .append_download(created.id, OffsetDateTime::now_utc())
        .await?;
    committed_download.commit().await?;
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM download_events")
            .fetch_one(&pool)
            .await?,
        2
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn package_download_statistics_fill_daily_buckets_across_versions(
    pool: PgPool,
) -> TestResult {
    let repository = PostgresRepository::new(pool);
    let publisher = committed_user(&repository, profile(305, "Statistics-Publisher")).await?;
    let until = OffsetDateTime::UNIX_EPOCH + Duration::days(100);
    let since = until - Duration::days(30);

    let mut transaction = repository.begin().await?;
    let namespace = transaction
        .create_namespace(&identity("Statistics"), Some(publisher.id))
        .await?;
    let package = transaction
        .create_package(namespace.id, &identity("Chart_Pkg"), Some(publisher.id))
        .await?;
    let first = transaction
        .create_package_version(&resolver_package_version(
            package.id,
            "1.0.0",
            31,
            Vec::new(),
        ))
        .await?;
    let second = transaction
        .create_package_version(&resolver_package_version(
            package.id,
            "2.0.0",
            32,
            Vec::new(),
        ))
        .await?;
    transaction
        .set_yank(second.id, Some((OffsetDateTime::now_utc(), publisher.id)))
        .await?;
    for (version_id, occurred_at) in [
        (first.id, since - Duration::days(1)),
        (first.id, since),
        (second.id, since + Duration::days(10)),
        (second.id, since + Duration::days(10) + Duration::hours(2)),
        (first.id, until),
    ] {
        transaction.append_download(version_id, occurred_at).await?;
    }
    transaction.commit().await?;

    let statistics = repository
        .package_download_statistics(
            &identity("statistics"),
            &identity("chart-pkg"),
            since,
            until,
        )
        .await?
        .expect("package statistics");
    assert_eq!(statistics.start_date, since.date());
    assert_eq!(statistics.end_date, (until - Duration::days(1)).date());
    assert_eq!(statistics.total_downloads, 3);
    assert_eq!(statistics.total_all_time, 4);
    assert_eq!(statistics.daily.len(), 30);
    assert_eq!(statistics.daily[0].downloads, 1);
    assert_eq!(statistics.daily[10].downloads, 2);
    assert_eq!(statistics.daily[29].downloads, 0);
    assert!(
        repository
            .package_download_statistics(
                &identity("statistics"),
                &identity("missing"),
                since,
                until,
            )
            .await?
            .is_none()
    );
    Ok(())
}

/// The immutability trigger compares `to_jsonb(NEW)` with `to_jsonb(OLD)`, and
/// `PostgreSQL` leaves generated columns NULL in NEW until BEFORE triggers have
/// run — so the semver sort keys have to be excluded or versions carrying a
/// prerelease or build metadata can never be yanked.
#[sqlx::test(migrations = "../../migrations")]
async fn yanking_is_allowed_for_prerelease_and_build_metadata_versions(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let publisher = committed_user(&repository, profile(311, "Yank-Publisher")).await?;

    let mut transaction = repository.begin().await?;
    let namespace = transaction
        .create_namespace(&identity("Yank_Tools"), Some(publisher.id))
        .await?;
    let package = transaction
        .create_package(namespace.id, &identity("Yank_Pkg"), Some(publisher.id))
        .await?;
    let yanked_at = OffsetDateTime::now_utc();
    for (index, value) in ["1.0.0-rc.1", "1.0.0+musl", "1.1.0-rc.1+musl"]
        .into_iter()
        .enumerate()
    {
        let created = transaction
            .create_package_version(&resolver_package_version(
                package.id,
                value,
                u8::try_from(index).expect("index fits in a byte") + 20,
                Vec::new(),
            ))
            .await?;
        assert_eq!(
            transaction
                .set_yank(created.id, Some((yanked_at, publisher.id)))
                .await?,
            WriteOutcome::Applied,
            "{value} should be yankable"
        );
        assert_eq!(
            transaction.set_yank(created.id, None).await?,
            WriteOutcome::Applied,
            "{value} should be un-yankable"
        );
    }
    transaction.commit().await?;

    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn resolver_index_reads_display_identities_yanks_and_dependencies_in_one_aggregate(
    pool: PgPool,
) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let publisher = committed_user(&repository, profile(302, "Resolver-Publisher")).await?;

    let mut transaction = repository.begin().await?;
    let namespace = transaction
        .create_namespace(&identity("Rux_Tools"), Some(publisher.id))
        .await?;
    transaction
        .set_namespace_owner(
            namespace.id,
            publisher.id,
            NamespaceRole::Owner,
            Some(publisher.id),
        )
        .await?;
    let package = transaction
        .create_package(namespace.id, &identity("Resolver_Pkg"), Some(publisher.id))
        .await?;
    let later = transaction
        .create_package_version(&resolver_package_version(
            package.id,
            "2.0.0",
            7,
            vec![
                dependency("Zed", "Rux", "Zed", "^2"),
                dependency("Alpha", "Rux", "Alpha", ">=1, <2"),
            ],
        ))
        .await?;
    let yanked_at = OffsetDateTime::now_utc();
    assert_eq!(
        transaction
            .set_yank(later.id, Some((yanked_at, publisher.id)))
            .await?,
        WriteOutcome::Applied
    );
    assert_eq!(
        transaction
            .set_yank(
                later.id,
                Some((yanked_at + time::Duration::hours(1), publisher.id)),
            )
            .await?,
        WriteOutcome::Unchanged,
        "repeated yanks preserve the original attribution and timestamp"
    );
    transaction
        .create_package_version(&resolver_package_version(
            package.id,
            "1.0.0+linux",
            8,
            Vec::new(),
        ))
        .await?;
    let empty_package = transaction
        .create_package(namespace.id, &identity("Empty_Pkg"), Some(publisher.id))
        .await?;
    assert_ne!(empty_package.id, package.id);
    transaction.commit().await?;

    let index = repository
        .resolver_index_by_name(&identity("rux-tools"), &identity("resolver-pkg"))
        .await?
        .expect("resolver package should exist");
    assert_eq!(index.namespace.as_str(), "Rux_Tools");
    assert_eq!(index.package.as_str(), "Resolver_Pkg");
    assert_eq!(index.versions.len(), 2);
    assert_eq!(index.versions[0].version.as_str(), "2.0.0");
    assert!(index.versions[0].yanked);
    assert_eq!(index.versions[0].dependencies[0].alias.as_str(), "Alpha");
    assert_eq!(index.versions[0].dependencies[1].alias.as_str(), "Zed");
    assert_eq!(index.versions[1].version.as_str(), "1.0.0+linux");
    assert!(!index.versions[1].yanked);
    assert!(index.versions[1].dependencies.is_empty());
    let yanked_download = repository
        .download_target_by_name(
            &identity("rux-tools"),
            &identity("resolver-pkg"),
            &version("2.0.0"),
        )
        .await?
        .expect("yanked versions remain downloadable");
    assert_eq!(yanked_download.package_version_id, later.id);

    let empty = repository
        .resolver_index_by_name(&identity("Rux_Tools"), &identity("empty-pkg"))
        .await?
        .expect("empty package row should remain distinguishable from a missing package");
    assert!(empty.versions.is_empty());
    assert!(
        repository
            .resolver_index_by_name(&identity("Rux_Tools"), &identity("missing"))
            .await?
            .is_none()
    );

    let token_service = Arc::new(ApiTokenService::new(
        Arc::new(repository.clone()),
        Arc::new(SystemClock),
        Arc::new(OsCredentialGenerator),
    ));
    let maintainer = committed_user(&repository, profile(304, "Resolver-Maintainer")).await?;
    let mut membership = repository.begin().await?;
    membership
        .set_namespace_owner(
            namespace.id,
            maintainer.id,
            NamespaceRole::Maintainer,
            Some(publisher.id),
        )
        .await?;
    membership.commit().await?;
    let token = token_service
        .issue(
            publisher.id,
            IssueApiToken {
                display_name: "resolver yank integration".into(),
                scopes: vec![TokenScope::Yank],
                expires_at: None,
            },
        )
        .await?;
    let maintainer_token = token_service
        .issue(
            maintainer.id,
            IssueApiToken {
                display_name: "resolver maintainer yank".into(),
                scopes: vec![TokenScope::Yank],
                expires_at: None,
            },
        )
        .await?;
    let publish_only = token_service
        .issue(
            publisher.id,
            IssueApiToken {
                display_name: "resolver publish only".into(),
                scopes: vec![TokenScope::Publish],
                expires_at: None,
            },
        )
        .await?;
    let outsider = committed_user(&repository, profile(303, "Resolver-Outsider")).await?;
    let outsider_token = token_service
        .issue(
            outsider.id,
            IssueApiToken {
                display_name: "resolver outsider yank".into(),
                scopes: vec![TokenScope::Yank],
                expires_at: None,
            },
        )
        .await?;
    let yanks = YankService::new(
        Arc::new(repository.clone()),
        token_service,
        Arc::new(SystemClock),
    );
    assert_eq!(
        yanks
            .set_yanked(
                &publish_only.credential,
                "rux-tools",
                "resolver-pkg",
                "2.0.0",
                false,
            )
            .await
            .unwrap_err()
            .kind(),
        YankErrorKind::InsufficientScope
    );
    assert_eq!(
        yanks
            .set_yanked(
                &outsider_token.credential,
                "rux-tools",
                "resolver-pkg",
                "2.0.0",
                false,
            )
            .await
            .unwrap_err()
            .kind(),
        YankErrorKind::Forbidden
    );
    assert_eq!(
        yanks
            .set_yanked("not-a-token", "rux-tools", "resolver-pkg", "2.0.0", false,)
            .await
            .unwrap_err()
            .kind(),
        YankErrorKind::AuthenticationRequired
    );
    assert_eq!(
        yanks
            .set_yanked(&token.credential, "rux-tools", "missing", "2.0.0", false,)
            .await
            .unwrap_err()
            .kind(),
        YankErrorKind::PackageVersionNotFound
    );
    assert_eq!(
        yanks
            .set_yanked(
                &token.credential,
                "bad namespace",
                "resolver-pkg",
                "2.0.0",
                false,
            )
            .await
            .unwrap_err()
            .kind(),
        YankErrorKind::InvalidNamespace
    );
    let unyanked_state = yanks
        .set_yanked(
            &maintainer_token.credential,
            "rux-tools",
            "resolver-pkg",
            "2.0.0",
            false,
        )
        .await?;
    assert!(!unyanked_state.yanked);
    assert_eq!(unyanked_state.namespace.as_str(), "Rux_Tools");
    assert_eq!(unyanked_state.package.as_str(), "Resolver_Pkg");
    yanks
        .set_yanked(
            &token.credential,
            "Rux_Tools",
            "Resolver_Pkg",
            "2.0.0",
            false,
        )
        .await?;

    let unyanked = repository
        .resolver_index_by_name(&identity("rux-tools"), &identity("resolver-pkg"))
        .await?
        .expect("resolver package should exist after unyanking");
    assert!(!unyanked.versions[0].yanked);
    let metadata = repository
        .package_version_metadata_by_name(
            &identity("rux-tools"),
            &identity("resolver-pkg"),
            &version("2.0.0"),
        )
        .await?
        .expect("unyanked metadata should remain available");
    assert!(!metadata.yanked);
    assert_eq!(
        query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_records WHERE action = 'package_version_unyanked'",
        )
        .fetch_one(&pool)
        .await?,
        1,
        "idempotent no-ops must not create another audit record"
    );
    Ok(())
}

async fn committed_user(
    repository: &PostgresRepository,
    profile: GitHubUserProfile,
) -> Result<rux_application::UserRecord, Box<dyn Error>> {
    let mut transaction = repository.begin().await?;
    let user = transaction.upsert_github_user(&profile).await?;
    transaction.commit().await?;
    Ok(user)
}

fn profile(id: u64, login: &str) -> GitHubUserProfile {
    GitHubUserProfile {
        github_user_id: id,
        github_login: login.into(),
        display_name: Some(login.into()),
        avatar_url: Some(format!("https://example.test/{login}.png")),
    }
}

fn identity(value: &str) -> IdentitySegment {
    IdentitySegment::new(value).expect("valid identity fixture")
}

fn version(value: &str) -> SemanticVersion {
    SemanticVersion::new(value).expect("valid version fixture")
}

fn dependency(alias: &str, namespace: &str, package: &str, range: &str) -> DependencyRecord {
    DependencyRecord {
        alias: identity(alias),
        target_namespace: identity(namespace),
        target_package: identity(package),
        version_range: VersionRange::new(range).expect("valid range fixture"),
    }
}

fn resolver_package_version(
    package_id: PackageId,
    value: &str,
    checksum_byte: u8,
    dependencies: Vec<DependencyRecord>,
) -> NewPackageVersion {
    NewPackageVersion {
        package_id,
        version: version(value),
        manifest_schema_version: 1,
        min_rux: version("0.4.0"),
        package_type: PackageKind::Source,
        description: None,
        repository_url: None,
        homepage_url: None,
        readme: None,
        license_expression: None,
        license_url: None,
        normalized_manifest: Map::new(),
        artifact_sha256: ArtifactSha256::new([checksum_byte; 32]),
        artifact_size: 1024,
        storage_key: format!("packages/resolver/{checksum_byte}.ruxpkg"),
        artifact_file_count: 2,
        artifact_expanded_bytes: 1024,
        source_file_count: 1,
        source_line_count: 10,
        published_by_user_id: None,
        authors: Vec::new(),
        keywords: Vec::new(),
        dependencies,
    }
}
