use serde_json::Value;
use sqlx::{Error, PgPool, query, query_as, query_scalar, raw_sql};

const LOCAL_CATALOG_SEED: &str = include_str!("../../../deploy/local/local-catalog.sql");

/// A version that exists for as long as `StdLib::Io` is the first entry in
/// `tools/catalog/packages.mjs`; the generator always starts that package's
/// history at `0.0.0`.
const ANCHOR_STORAGE_KEY: &str = "local-seed/stdlib/io/0.0.0+musl.ruxpkg";

#[sqlx::test(migrations = "../../migrations")]
async fn local_catalog_seed_populates_representative_data(pool: PgPool) -> Result<(), Error> {
    run_seed(&pool).await?;

    // These come from `node tools/catalog/generate-catalog.mjs`, which prints
    // them on every run. Regenerate the seed and update them together.
    assert_eq!(table_count(&pool, "namespaces").await?, 12);
    assert_eq!(table_count(&pool, "packages").await?, 100);
    assert_eq!(table_count(&pool, "package_versions").await?, 633);
    assert_eq!(table_count(&pool, "package_version_authors").await?, 774);
    assert_eq!(table_count(&pool, "package_version_keywords").await?, 1580);
    assert_eq!(table_count(&pool, "dependencies").await?, 860);
    assert_eq!(table_count(&pool, "download_events").await?, 500_009);
    assert_eq!(table_count(&pool, "users").await?, 0);

    // Every package carries between 2 and 10 releases.
    let release_span = query_as::<_, (i64, i64)>(
        "SELECT min(releases), max(releases)
         FROM (
             SELECT count(*) AS releases
             FROM package_versions
             GROUP BY package_id
         ) AS counted",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(release_span, (2, 10));

    // Display names are PascalCase with no separators, so the normalised form
    // is the name lowercased and nothing else.
    let separated = query_scalar::<_, i64>(
        "SELECT count(*)
         FROM (
             SELECT display_name FROM namespaces
             UNION ALL SELECT display_name FROM packages
             UNION ALL SELECT display_name FROM package_version_keywords
         ) AS named
         WHERE display_name LIKE '%\\_%' OR display_name LIKE '%-%'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(separated, 0);

    let identities = query_as::<_, (String, String)>(
        "SELECT namespaces.normalized_name, packages.normalized_name
         FROM packages
         JOIN namespaces ON namespaces.id = packages.namespace_id
         WHERE packages.normalized_name IN ('io', 'httpclient', 'registrycli')
         ORDER BY namespaces.normalized_name",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        identities,
        vec![
            ("acme".into(), "registrycli".into()),
            ("communitytools".into(), "httpclient".into()),
            ("stdlib".into(), "io".into()),
        ]
    );

    // The catalog exercises every rendering path the registry UI has: all three
    // package kinds, prereleases, build metadata, and yanked releases.
    let package_kinds = query_scalar::<_, String>(
        "SELECT package_type
         FROM package_versions
         GROUP BY package_type
         ORDER BY package_type",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(package_kinds, vec!["library", "program", "source"]);

    let variants = query_as::<_, (i64, i64, i64)>(
        "SELECT
             count(*) FILTER (WHERE prerelease IS NOT NULL),
             count(*) FILTER (WHERE build_metadata IS NOT NULL),
             count(*) FILTER (WHERE yanked_at IS NOT NULL)
         FROM package_versions",
    )
    .fetch_one(&pool)
    .await?;
    assert!(variants.0 > 0, "expected prerelease versions");
    assert!(variants.1 > 0, "expected build metadata versions");
    assert!(variants.2 > 0, "expected yanked versions");

    // Every dependency names a package that is actually in the catalog.
    let dangling = query_scalar::<_, i64>(
        "SELECT count(*)
         FROM dependencies
         WHERE NOT EXISTS (
             SELECT 1
             FROM packages
             JOIN namespaces ON namespaces.id = packages.namespace_id
             WHERE namespaces.normalized_name = dependencies.target_namespace_normalized_name
               AND packages.normalized_name = dependencies.target_package_normalized_name
         )",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(dangling, 0);

    // Every release has a README and at least one author and keyword.
    let incomplete = query_scalar::<_, i64>(
        "SELECT count(*)
         FROM package_versions
         WHERE readme_text IS NULL
            OR NOT EXISTS (
                SELECT 1 FROM package_version_authors
                WHERE package_version_id = package_versions.id
            )
            OR NOT EXISTS (
                SELECT 1 FROM package_version_keywords
                WHERE package_version_id = package_versions.id
            )",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(incomplete, 0);

    // READMEs stay within the 20-300 word band the catalog is generated to.
    let readme_span = query_as::<_, (i32, i32)>(
        "SELECT
             min(array_length(regexp_split_to_array(trim(readme_text), '\\s+'), 1)),
             max(array_length(regexp_split_to_array(trim(readme_text), '\\s+'), 1))
         FROM package_versions",
    )
    .fetch_one(&pool)
    .await?;
    assert!(
        readme_span.0 >= 20 && readme_span.1 <= 300,
        "readme word counts {readme_span:?} outside the 20-300 band"
    );

    // Download history lands inside the generated window and is skewed toward
    // the present, which is what the highlights endpoint ranks on.
    let downloads_in_window = query_scalar::<_, bool>(
        "SELECT min(occurred_at) > now() - interval '91 days' AND max(occurred_at) <= now()
         FROM download_events",
    )
    .fetch_one(&pool)
    .await?;
    assert!(downloads_in_window);

    let searchable = query_scalar::<_, bool>(
        "SELECT search_vector @@ plainto_tsquery('simple', 'input')
         FROM package_versions
         WHERE storage_key = $1",
    )
    .bind(ANCHOR_STORAGE_KEY)
    .fetch_one(&pool)
    .await?;
    assert!(searchable);

    let manifest = query_scalar::<_, Value>(
        "SELECT normalized_manifest FROM package_versions WHERE storage_key = $1",
    )
    .bind(ANCHOR_STORAGE_KEY)
    .fetch_one(&pool)
    .await?;
    assert_eq!(manifest["manifest"]["version"], 1);
    assert_eq!(manifest["package"]["namespace"], "StdLib");
    assert_eq!(manifest["package"]["name"], "Io");
    assert_eq!(manifest["package"]["version"], "0.0.0+musl");

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
         WHERE normalized_name = 'communitytools'",
    )
    .execute(&pool)
    .await?;
    query(
        "UPDATE package_versions
         SET yanked_at = published_at + interval '1 day'
         WHERE storage_key = $1",
    )
    .bind(ANCHOR_STORAGE_KEY)
    .execute(&pool)
    .await?;
    query(
        "INSERT INTO namespaces (display_name, created_at, updated_at)
         VALUES ('LocalOnly', '2030-01-02 00:00:00+00', '2030-01-02 00:00:00+00')",
    )
    .execute(&pool)
    .await?;

    let downloads_before = table_count(&pool, "download_events").await?;
    let before = catalog_fingerprint(&pool).await?;
    run_seed(&pool).await?;
    let after = catalog_fingerprint(&pool).await?;

    assert_eq!(after, before);
    assert_eq!(table_count(&pool, "namespaces").await?, 13);
    assert_eq!(table_count(&pool, "packages").await?, 100);
    assert_eq!(table_count(&pool, "package_versions").await?, 633);

    // download_events has no natural key, so the seed guards on existing
    // history rather than ON CONFLICT. Without that guard a second run would
    // double every count and skew the popularity ranking.
    assert_eq!(
        table_count(&pool, "download_events").await?,
        downloads_before
    );

    let preserved_namespace = query_scalar::<_, bool>(
        "SELECT updated_at = '2030-01-01 00:00:00+00'
         FROM namespaces
         WHERE normalized_name = 'communitytools'",
    )
    .fetch_one(&pool)
    .await?;
    assert!(preserved_namespace);

    let preserved_yank = query_scalar::<_, bool>(
        "SELECT yanked_at = published_at + interval '1 day'
         FROM package_versions
         WHERE storage_key = $1",
    )
    .bind(ANCHOR_STORAGE_KEY)
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
