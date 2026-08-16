use sqlx::migrate::Migrator;
use sqlx::{AssertSqlSafe, Error, PgPool, query, query_as, query_scalar};
use uuid::Uuid;

static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

struct CatalogFixture {
    owner: Uuid,
    invitee: Uuid,
    namespace: Uuid,
    package: Uuid,
}

#[sqlx::test(migrations = "../../migrations")]
async fn initial_schema_accepts_a_complete_registry_graph(pool: PgPool) -> Result<(), Error> {
    let fixture = create_catalog_fixture(&pool).await?;
    let version_id = insert_version(&pool, fixture.package, "1.2.3+linux", "linux").await?;

    query(
        "INSERT INTO package_version_authors (package_version_id, ordinal, author)
         VALUES ($1, 0, 'Rux Contributors <info@rux-lang.dev>')",
    )
    .bind(version_id)
    .execute(&pool)
    .await?;
    query(
        "INSERT INTO package_version_keywords (package_version_id, ordinal, display_name)
         VALUES ($1, 0, 'Registry_Tools')",
    )
    .bind(version_id)
    .execute(&pool)
    .await?;
    query(
        "INSERT INTO dependencies (
             package_version_id,
             display_alias,
             target_namespace_display_name,
             target_package_display_name,
             version_range,
             target_os
         ) VALUES ($1, 'Fast_Json', 'Acme_Tools', 'Fast_Json', '^2.0', ARRAY['Linux', 'Windows'])",
    )
    .bind(version_id)
    .execute(&pool)
    .await?;

    let token_id = query_scalar::<_, Uuid>(
        "INSERT INTO api_tokens (user_id, display_name, token_prefix, secret_hash)
         VALUES ($1, 'Release token', 'rux_test_1234', $2)
         RETURNING id",
    )
    .bind(fixture.owner)
    .bind(vec![2_u8; 32])
    .fetch_one(&pool)
    .await?;
    query(
        "INSERT INTO api_token_scopes (api_token_id, scope)
         VALUES ($1, 'publish'), ($1, 'yank')",
    )
    .bind(token_id)
    .execute(&pool)
    .await?;
    query(
        "INSERT INTO audit_records (
             actor_user_id,
             actor_token_id,
             action,
             subject_type,
             subject_key,
             metadata
         ) VALUES ($1, $2, 'package_published', 'package_version', 'Rux/Example@1.2.3+linux', '{}')",
    )
    .bind(fixture.owner)
    .bind(token_id)
    .execute(&pool)
    .await?;
    query("INSERT INTO download_events (package_version_id) VALUES ($1)")
        .bind(version_id)
        .execute(&pool)
        .await?;

    let names = query_as::<_, (String, String, String, String, Vec<String>)>(
        "SELECT
             n.normalized_name,
             p.normalized_name,
             k.normalized_name,
             d.target_namespace_normalized_name,
             d.target_os
         FROM namespaces n
         JOIN packages p ON p.namespace_id = n.id
         JOIN package_versions v ON v.package_id = p.id
         JOIN package_version_keywords k ON k.package_version_id = v.id
         JOIN dependencies d ON d.package_version_id = v.id
         WHERE v.id = $1",
    )
    .bind(version_id)
    .fetch_one(&pool)
    .await?;

    assert_eq!(
        names,
        (
            "rux".into(),
            "example".into(),
            "registry-tools".into(),
            "acme-tools".into(),
            vec!["Linux".into(), "Windows".into()]
        )
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn normalized_identity_collisions_and_invalid_syntax_are_rejected(
    pool: PgPool,
) -> Result<(), Error> {
    let owner_id = insert_user(&pool, 1, "Owner-One").await?;
    query("INSERT INTO namespaces (display_name, created_by_user_id) VALUES ('Foo_Bar', $1)")
        .bind(owner_id)
        .execute(&pool)
        .await?;

    assert_constraint(
        query("INSERT INTO namespaces (display_name) VALUES ('foo-bar')")
            .execute(&pool)
            .await,
        "namespaces_normalized_name_unique",
    );
    assert_constraint(
        query("INSERT INTO namespaces (display_name) VALUES ('bad--name')")
            .execute(&pool)
            .await,
        "namespaces_display_name_syntax",
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn blocked_identity_policy_is_empty_normalized_and_typed(pool: PgPool) -> Result<(), Error> {
    assert_eq!(
        query_scalar::<_, i64>("SELECT count(*) FROM blocked_identities")
            .fetch_one(&pool)
            .await?,
        0
    );
    query(
        "INSERT INTO blocked_identities (identity_kind, display_name)
         VALUES ('namespace', 'Rux_Tools'), ('package', 'Rux_Tools')",
    )
    .execute(&pool)
    .await?;

    assert_constraint(
        query(
            "INSERT INTO blocked_identities (identity_kind, display_name)
             VALUES ('namespace', 'rux-tools')",
        )
        .execute(&pool)
        .await,
        "blocked_identities_kind_normalized_name_unique",
    );
    assert_constraint(
        query(
            "INSERT INTO blocked_identities (identity_kind, display_name)
             VALUES ('owner', 'Example')",
        )
        .execute(&pool)
        .await,
        "blocked_identities_kind",
    );
    assert_constraint(
        query(
            "INSERT INTO blocked_identities (identity_kind, display_name)
             VALUES ('package', 'bad--name')",
        )
        .execute(&pool)
        .await,
        "blocked_identities_display_name_syntax",
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn semantic_version_identity_preserves_build_metadata_and_u64_bounds(
    pool: PgPool,
) -> Result<(), Error> {
    let fixture = create_catalog_fixture(&pool).await?;
    insert_version(&pool, fixture.package, "1.0.0+linux", "linux").await?;
    insert_version(&pool, fixture.package, "1.0.0+windows", "windows").await?;

    assert_constraint(
        insert_version(&pool, fixture.package, "1.0.0+linux", "linux-duplicate").await,
        "package_versions_package_version_unique",
    );

    query(
        "INSERT INTO package_versions (
             package_id, version, major, minor, patch, manifest_schema_version, min_rux,
             package_type, normalized_manifest, artifact_sha256, artifact_size, storage_key,
             artifact_file_count, artifact_expanded_bytes, source_file_count, source_line_count
         ) VALUES (
             $1, '18446744073709551615.0.0', 18446744073709551615, 0, 0, 1, '0.4.0',
             'source_library', '{}', decode(repeat('ab', 32), 'hex'), 1024, 'packages/u64-max.ruxpkg',
             2, 2048, 1, 1
         )",
    )
    .bind(fixture.package)
    .execute(&pool)
    .await?;

    assert_constraint(
        query(
            "INSERT INTO package_versions (
                 package_id, version, major, minor, patch, manifest_schema_version, min_rux,
                 package_type, normalized_manifest, artifact_sha256, artifact_size, storage_key,
                 artifact_file_count, artifact_expanded_bytes, source_file_count, source_line_count
             ) VALUES (
                 $1, '18446744073709551616.0.0', 18446744073709551616, 0, 0, 1, '0.4.0',
                 'source_library', '{}', decode(repeat('cd', 32), 'hex'), 1024, 'packages/u64-overflow.ruxpkg',
                 2, 2048, 1, 1
             )",
        )
        .bind(fixture.package)
        .execute(&pool)
        .await,
        "package_versions_component_range",
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn credentials_roles_invitations_and_append_only_records_are_constrained(
    pool: PgPool,
) -> Result<(), Error> {
    let fixture = create_catalog_fixture(&pool).await?;
    let version_id = insert_version(&pool, fixture.package, "1.0.0", "release").await?;

    assert_constraint(
        query(
            "INSERT INTO packages (namespace_id, display_name)
             VALUES ('00000000-0000-0000-0000-000000000000', 'Orphan')",
        )
        .execute(&pool)
        .await,
        "packages_namespace_id_fkey",
    );

    assert_constraint(
        query(
            "INSERT INTO namespace_owners (namespace_id, user_id, role)
             VALUES ($1, $2, 'administrator')",
        )
        .bind(fixture.namespace)
        .bind(fixture.invitee)
        .execute(&pool)
        .await,
        "namespace_owners_role",
    );
    assert_constraint(
        query(
            "INSERT INTO sessions (user_id, secret_hash, csrf_hash, expires_at)
             VALUES ($1, $2, $3, now() + interval '1 hour')",
        )
        .bind(fixture.owner)
        .bind(vec![1_u8; 31])
        .bind(vec![2_u8; 32])
        .execute(&pool)
        .await,
        "sessions_secret_hash_length",
    );
    assert_constraint(
        query(
            "INSERT INTO namespace_invitations (
                 namespace_id, invited_user_id, invited_by_user_id, role, expires_at,
                 accepted_at, revoked_at
             ) VALUES ($1, $2, $3, 'maintainer', now() + interval '1 day', now(), now())",
        )
        .bind(fixture.namespace)
        .bind(fixture.invitee)
        .bind(fixture.owner)
        .execute(&pool)
        .await,
        "namespace_invitations_resolution",
    );
    assert_constraint(
        query(
            "INSERT INTO namespace_invitations (
                 namespace_id, invited_user_id, invited_by_user_id, role, expires_at
             ) VALUES ($1, $2, $3, 'owner', now() + interval '2 days')",
        )
        .bind(fixture.namespace)
        .bind(fixture.invitee)
        .bind(fixture.owner)
        .execute(&pool)
        .await,
        "namespace_invitations_one_pending_idx",
    );

    query(
        "INSERT INTO dependencies (
             package_version_id, display_alias, target_namespace_display_name,
             target_package_display_name, version_range
         ) VALUES ($1, 'Json_Parser', 'Rux', 'Json', '^1')",
    )
    .bind(version_id)
    .execute(&pool)
    .await?;
    assert_constraint(
        query(
            "INSERT INTO dependencies (
                 package_version_id, display_alias, target_namespace_display_name,
                 target_package_display_name, version_range
             ) VALUES ($1, 'json-parser', 'Other', 'Json', '^2')",
        )
        .bind(version_id)
        .execute(&pool)
        .await,
        "dependencies_pkey",
    );
    for (alias, target_os) in [
        ("DuplicateTarget", "ARRAY['Windows', 'Windows']"),
        ("UnknownTarget", "ARRAY['Android']"),
    ] {
        let statement = format!(
            "INSERT INTO dependencies (
                 package_version_id, display_alias, target_namespace_display_name,
                 target_package_display_name, version_range, target_os
             ) VALUES ($1, '{alias}', 'Rux', 'Json', '^1', {target_os})"
        );
        assert_constraint(
            query(AssertSqlSafe(statement))
                .bind(version_id)
                .execute(&pool)
                .await,
            "dependencies_target_os_valid",
        );
    }

    assert_constraint(
        query(
            "INSERT INTO api_tokens (user_id, display_name, token_prefix, secret_hash)
             VALUES ($1, 'Bad token', 'rux_bad_1234', $2)",
        )
        .bind(fixture.owner)
        .bind(vec![3_u8; 31])
        .execute(&pool)
        .await,
        "api_tokens_secret_hash_length",
    );

    let token_id = query_scalar::<_, Uuid>(
        "INSERT INTO api_tokens (user_id, display_name, token_prefix, secret_hash)
         VALUES ($1, 'Test token', 'rux_test_5678', $2)
         RETURNING id",
    )
    .bind(fixture.owner)
    .bind(vec![4_u8; 32])
    .fetch_one(&pool)
    .await?;
    assert_constraint(
        query("INSERT INTO api_token_scopes (api_token_id, scope) VALUES ($1, 'admin')")
            .bind(token_id)
            .execute(&pool)
            .await,
        "api_token_scopes_scope",
    );

    query(
        "INSERT INTO audit_records (actor_user_id, action, subject_type)
         VALUES ($1, 'package_published', 'package_version')",
    )
    .bind(fixture.owner)
    .execute(&pool)
    .await?;
    assert_sql_state(
        query("DELETE FROM audit_records").execute(&pool).await,
        "55000",
    );
    assert_sql_state(
        query("UPDATE package_versions SET description = 'changed' WHERE id = $1")
            .bind(version_id)
            .execute(&pool)
            .await,
        "55000",
    );
    query("UPDATE package_versions SET yanked_at = now(), yanked_by_user_id = $2 WHERE id = $1")
        .bind(version_id)
        .bind(fixture.owner)
        .execute(&pool)
        .await?;
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn migration_rolls_back_and_reapplies_cleanly(pool: PgPool) -> Result<(), Error> {
    MIGRATOR.run(&pool).await?;
    assert!(table_exists(&pool, "package_versions").await?);
    assert!(table_exists(&pool, "blocked_identities").await?);
    assert!(extension_exists(&pool, "pg_trgm").await?);

    MIGRATOR.undo(&pool, 0).await?;
    assert!(!table_exists(&pool, "package_versions").await?);
    assert!(!table_exists(&pool, "users").await?);
    assert!(!table_exists(&pool, "blocked_identities").await?);
    assert!(!extension_exists(&pool, "pg_trgm").await?);

    MIGRATOR.run(&pool).await?;
    assert!(table_exists(&pool, "download_events").await?);
    assert!(table_exists(&pool, "blocked_identities").await?);
    assert!(extension_exists(&pool, "pg_trgm").await?);
    Ok(())
}

async fn create_catalog_fixture(pool: &PgPool) -> Result<CatalogFixture, Error> {
    let owner_id = insert_user(pool, 1, "Owner-One").await?;
    let invitee_id = insert_user(pool, 2, "Invitee-Two").await?;
    let namespace_id = query_scalar::<_, Uuid>(
        "INSERT INTO namespaces (display_name, created_by_user_id)
         VALUES ('Rux', $1)
         RETURNING id",
    )
    .bind(owner_id)
    .fetch_one(pool)
    .await?;
    query(
        "INSERT INTO namespace_owners (namespace_id, user_id, role, added_by_user_id)
         VALUES ($1, $2, 'owner', $2)",
    )
    .bind(namespace_id)
    .bind(owner_id)
    .execute(pool)
    .await?;
    query(
        "INSERT INTO namespace_invitations (
             namespace_id, invited_user_id, invited_by_user_id, role, expires_at
         ) VALUES ($1, $2, $3, 'maintainer', now() + interval '7 days')",
    )
    .bind(namespace_id)
    .bind(invitee_id)
    .bind(owner_id)
    .execute(pool)
    .await?;
    let package_id = query_scalar::<_, Uuid>(
        "INSERT INTO packages (namespace_id, display_name, created_by_user_id)
         VALUES ($1, 'Example', $2)
         RETURNING id",
    )
    .bind(namespace_id)
    .bind(owner_id)
    .fetch_one(pool)
    .await?;

    Ok(CatalogFixture {
        owner: owner_id,
        invitee: invitee_id,
        namespace: namespace_id,
        package: package_id,
    })
}

async fn insert_user(pool: &PgPool, github_user_id: i64, login: &str) -> Result<Uuid, Error> {
    query_scalar::<_, Uuid>(
        "INSERT INTO users (github_user_id, github_login)
         VALUES ($1, $2)
         RETURNING id",
    )
    .bind(github_user_id)
    .bind(login)
    .fetch_one(pool)
    .await
}

async fn insert_version(
    pool: &PgPool,
    package_id: Uuid,
    version: &str,
    storage_suffix: &str,
) -> Result<Uuid, Error> {
    let build_metadata = version.split_once('+').map(|(_, build)| build);
    query_scalar::<_, Uuid>(
        "INSERT INTO package_versions (
             package_id, version, major, minor, patch, build_metadata,
             manifest_schema_version, min_rux, package_type, normalized_manifest,
             artifact_sha256, artifact_size, storage_key, artifact_file_count,
             artifact_expanded_bytes, source_file_count, source_line_count
         ) VALUES (
             $1, $2, 1, 2, 3, $3, 1, '0.4.0', 'source_library', '{}', $4, 1024,
             $5, 2, 2048, 1, 10
         )
         RETURNING id",
    )
    .bind(package_id)
    .bind(version)
    .bind(build_metadata)
    .bind(vec![1_u8; 32])
    .bind(format!("packages/example-{storage_suffix}.ruxpkg"))
    .fetch_one(pool)
    .await
}

async fn table_exists(pool: &PgPool, table: &str) -> Result<bool, Error> {
    query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("public.{table}"))
        .fetch_one(pool)
        .await
}

async fn extension_exists(pool: &PgPool, extension: &str) -> Result<bool, Error> {
    query_scalar::<_, bool>("SELECT EXISTS (SELECT FROM pg_extension WHERE extname = $1)")
        .bind(extension)
        .fetch_one(pool)
        .await
}

fn assert_constraint<T>(result: Result<T, Error>, expected: &str) {
    let error = result
        .err()
        .expect("statement should violate a database constraint");
    let database = error
        .as_database_error()
        .expect("constraint failure should be a database error");
    assert_eq!(database.constraint(), Some(expected));
}

fn assert_sql_state<T>(result: Result<T, Error>, expected: &str) {
    let error = result
        .err()
        .expect("statement should fail with a database error");
    let database = error
        .as_database_error()
        .expect("failure should be a database error");
    assert_eq!(database.code().as_deref(), Some(expected));
}
