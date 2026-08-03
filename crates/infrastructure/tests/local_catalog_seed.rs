use serde_json::Value;
use sqlx::{Error, PgPool, query, query_as, query_scalar, raw_sql};

const LOCAL_CATALOG_SEED: &str = include_str!("../../../deploy/local/local-catalog.sql");

#[sqlx::test(migrations = "../../migrations")]
async fn local_catalog_seed_populates_representative_data(pool: PgPool) -> Result<(), Error> {
    run_seed(&pool).await?;

    assert_eq!(table_count(&pool, "namespaces").await?, 3);
    assert_eq!(table_count(&pool, "packages").await?, 4);
    assert_eq!(table_count(&pool, "package_versions").await?, 9);
    assert_eq!(table_count(&pool, "package_version_authors").await?, 10);
    assert_eq!(table_count(&pool, "package_version_keywords").await?, 20);
    assert_eq!(table_count(&pool, "dependencies").await?, 9);
    assert_eq!(table_count(&pool, "users").await?, 0);
    assert_eq!(table_count(&pool, "download_events").await?, 0);

    let identities = query_as::<_, (String, String)>(
        "SELECT namespaces.normalized_name, packages.normalized_name
         FROM packages
         JOIN namespaces ON namespaces.id = packages.namespace_id
         ORDER BY namespaces.normalized_name, packages.normalized_name",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        identities,
        vec![
            ("acme".into(), "registry-cli".into()),
            ("community-tools".into(), "http-client".into()),
            ("rux".into(), "io".into()),
            ("rux".into(), "json".into()),
        ]
    );

    let version_states = query_as::<_, (String, Option<String>, Option<String>, bool)>(
        "SELECT version, prerelease, build_metadata, yanked_at IS NOT NULL
         FROM package_versions
         ORDER BY version",
    )
    .fetch_all(&pool)
    .await?;
    assert!(version_states.contains(&("1.1.0-beta.1".into(), Some("beta.1".into()), None, false)));
    assert!(version_states.contains(&("2.0.0+native".into(), None, Some("native".into()), false)));
    assert!(version_states.contains(&(
        "2.0.0+portable".into(),
        None,
        Some("portable".into()),
        false
    )));
    assert!(version_states.contains(&("1.0.0".into(), None, None, true)));

    let package_kinds = query_scalar::<_, String>(
        "SELECT package_type
         FROM package_versions
         GROUP BY package_type
         ORDER BY package_type",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(package_kinds, vec!["library", "program", "source"]);

    let http_dependencies = query_as::<_, (String, String, String)>(
        "SELECT
             dependencies.normalized_alias,
             dependencies.target_namespace_normalized_name,
             dependencies.target_package_normalized_name
         FROM dependencies
         JOIN package_versions ON package_versions.id = dependencies.package_version_id
         JOIN packages ON packages.id = package_versions.package_id
         WHERE packages.normalized_name = 'http-client'
           AND package_versions.version = '1.0.0'
         ORDER BY dependencies.normalized_alias",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        http_dependencies,
        vec![
            ("io".into(), "rux".into(), "io".into()),
            ("json".into(), "rux".into(), "json".into()),
        ]
    );

    let authors = query_scalar::<_, String>(
        "SELECT package_version_authors.author
         FROM package_version_authors
         JOIN package_versions
             ON package_versions.id = package_version_authors.package_version_id
         JOIN packages ON packages.id = package_versions.package_id
         WHERE packages.normalized_name = 'http-client'
           AND package_versions.version = '1.0.0'
         ORDER BY package_version_authors.ordinal",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(authors, vec!["Rux Community", "Casey Example"]);

    let command_line_keyword = query_scalar::<_, String>(
        "SELECT normalized_name
         FROM package_version_keywords
         WHERE display_name = 'Command_Line'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(command_line_keyword, "command-line");

    let searchable = query_scalar::<_, bool>(
        "SELECT search_vector @@ plainto_tsquery('simple', 'streaming')
         FROM package_versions
         WHERE storage_key = 'local-seed/rux/json/1.1.0.ruxpkg'",
    )
    .fetch_one(&pool)
    .await?;
    assert!(searchable);

    let manifest = query_scalar::<_, Value>(
        "SELECT normalized_manifest
         FROM package_versions
         WHERE storage_key = 'local-seed/acme/registry-cli/2.0.0+native.ruxpkg'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(manifest["manifest"]["version"], 1);
    assert_eq!(manifest["package"]["name"], "Registry_Cli");
    assert_eq!(manifest["dependencies"]["http"]["package"], "Http_Client");

    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn local_catalog_seed_is_repeatable_and_preserves_existing_rows(
    pool: PgPool,
) -> Result<(), Error> {
    run_seed(&pool).await?;

    query(
        "UPDATE namespaces
         SET updated_at = '2030-01-01 00:00:00+00'
         WHERE normalized_name = 'community-tools'",
    )
    .execute(&pool)
    .await?;
    query(
        "UPDATE package_versions
         SET yanked_at = published_at + interval '1 day'
         WHERE storage_key = 'local-seed/rux/io/1.1.0.ruxpkg'",
    )
    .execute(&pool)
    .await?;
    query(
        "INSERT INTO namespaces (display_name, created_at, updated_at)
         VALUES ('Local_Only', '2030-01-02 00:00:00+00', '2030-01-02 00:00:00+00')",
    )
    .execute(&pool)
    .await?;

    let before = catalog_fingerprint(&pool).await?;
    run_seed(&pool).await?;
    let after = catalog_fingerprint(&pool).await?;

    assert_eq!(after, before);
    assert_eq!(table_count(&pool, "namespaces").await?, 4);
    assert_eq!(table_count(&pool, "packages").await?, 4);
    assert_eq!(table_count(&pool, "package_versions").await?, 9);

    let preserved_namespace = query_scalar::<_, bool>(
        "SELECT updated_at = '2030-01-01 00:00:00+00'
         FROM namespaces
         WHERE normalized_name = 'community-tools'",
    )
    .fetch_one(&pool)
    .await?;
    assert!(preserved_namespace);

    let preserved_yank = query_scalar::<_, bool>(
        "SELECT yanked_at = published_at + interval '1 day'
         FROM package_versions
         WHERE storage_key = 'local-seed/rux/io/1.1.0.ruxpkg'",
    )
    .fetch_one(&pool)
    .await?;
    assert!(preserved_yank);

    Ok(())
}

async fn run_seed(pool: &PgPool) -> Result<(), Error> {
    raw_sql(LOCAL_CATALOG_SEED).execute(pool).await?;
    Ok(())
}

async fn table_count(pool: &PgPool, table: &str) -> Result<i64, Error> {
    query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
}

async fn catalog_fingerprint(pool: &PgPool) -> Result<String, Error> {
    query_scalar(
        "SELECT md5(jsonb_build_object(
             'namespaces', (
                 SELECT jsonb_agg(to_jsonb(rows) ORDER BY rows.id)
                 FROM (SELECT * FROM namespaces) AS rows
             ),
             'packages', (
                 SELECT jsonb_agg(to_jsonb(rows) ORDER BY rows.id)
                 FROM (SELECT * FROM packages) AS rows
             ),
             'versions', (
                 SELECT jsonb_agg(to_jsonb(rows) ORDER BY rows.id)
                 FROM (SELECT * FROM package_versions) AS rows
             ),
             'authors', (
                 SELECT jsonb_agg(
                     to_jsonb(rows)
                     ORDER BY rows.package_version_id, rows.ordinal
                 )
                 FROM (SELECT * FROM package_version_authors) AS rows
             ),
             'keywords', (
                 SELECT jsonb_agg(
                     to_jsonb(rows)
                     ORDER BY rows.package_version_id, rows.ordinal
                 )
                 FROM (SELECT * FROM package_version_keywords) AS rows
             ),
             'dependencies', (
                 SELECT jsonb_agg(
                     to_jsonb(rows)
                     ORDER BY rows.package_version_id, rows.normalized_alias
                 )
                 FROM (SELECT * FROM dependencies) AS rows
             )
         )::text)",
    )
    .fetch_one(pool)
    .await
}
