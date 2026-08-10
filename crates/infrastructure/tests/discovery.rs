use std::error::Error;
use uuid::Uuid;

use rux_application::{DiscoveryReader, KeywordSortOrder, SitemapEntryKind};
use rux_domain::{IdentitySegment, SemanticVersion};
use rux_infrastructure::PostgresRepository;
use sqlx::{PgPool, query, query_scalar};
use time::OffsetDateTime;

type TestResult = Result<(), Box<dyn Error>>;

#[sqlx::test(migrations = "../../migrations")]
async fn dependents_keywords_and_sitemap_use_representative_versions(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let target = create_package(&pool, "Rux", "Io").await?;
    insert_version(&pool, target, "1.0.0", "2026-01-01T00:00:00Z", false).await?;

    let client = create_package(&pool, "Community", "Client").await?;
    let stable = insert_version(&pool, client, "1.0.0", "2026-02-01T00:00:00Z", false).await?;
    add_dependency(&pool, stable, "Io", "Rux", "Io", "^1").await?;
    add_keyword(&pool, stable, "Web").await?;
    let preview =
        insert_version(&pool, client, "2.0.0-beta.1", "2026-03-01T00:00:00Z", false).await?;
    add_keyword(&pool, preview, "Preview").await?;

    let legacy = create_package(&pool, "Community", "Legacy").await?;
    let legacy_version =
        insert_version(&pool, legacy, "1.0.0", "2026-01-15T00:00:00Z", true).await?;
    add_dependency(&pool, legacy_version, "Runtime", "Rux", "Io", "*").await?;
    add_keyword(&pool, legacy_version, "Web").await?;

    let dependents = repository
        .dependent_packages(&identity("rux"), &identity("io"), None, 10)
        .await?
        .expect("target package exists");
    assert_eq!(
        dependents
            .iter()
            .map(|item| item.package.as_str())
            .collect::<Vec<_>>(),
        ["Client", "Legacy"]
    );
    assert_eq!(dependents[0].version.as_str(), "1.0.0");
    assert_eq!(dependents[0].requirements[0].alias.as_str(), "Io");
    assert!(dependents[1].yanked);

    let keywords = repository
        .keywords(KeywordSortOrder::Packages, 1, 10)
        .await?
        .items;
    assert_eq!(keywords[0].keyword.normalized(), "web");
    assert_eq!(keywords[0].package_count, 2);
    assert!(
        keywords
            .iter()
            .all(|keyword| keyword.keyword.normalized() != "preview")
    );

    let sitemap = repository.sitemap_entries(None, 100).await?;
    assert!(sitemap.iter().any(|entry| {
        entry.kind == SitemapEntryKind::Keyword
            && entry
                .keyword
                .as_ref()
                .is_some_and(|value| value.normalized() == "web")
    }));
    assert!(sitemap.iter().any(|entry| {
        entry.kind == SitemapEntryKind::Namespace
            && entry
                .namespace
                .as_ref()
                .is_some_and(|value| value.normalized() == "community")
    }));
    assert!(sitemap.iter().any(|entry| {
        entry.kind == SitemapEntryKind::Package
            && entry
                .package
                .as_ref()
                .is_some_and(|value| value.normalized() == "legacy")
    }));
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn keywords_sort_by_count_or_name_and_page_by_number(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    // "zeta" is on every package, "alpha" on one — so the two orderings are
    // exact opposites and neither can pass by accident.
    for (index, package) in ["One", "Two", "Three"].iter().enumerate() {
        let id = create_package(&pool, "Rux", package).await?;
        let version = insert_version(&pool, id, "1.0.0", "2026-01-01T00:00:00Z", false).await?;
        add_keyword(&pool, version, "Zeta").await?;
        if index == 0 {
            add_keyword(&pool, version, "Alpha").await?;
            add_keyword(&pool, version, "Middle").await?;
        }
    }

    let by_count = keyword_names(&repository, KeywordSortOrder::Packages, 1, 10).await?;
    assert_eq!(by_count, ["zeta", "alpha", "middle"]);
    let by_name = keyword_names(&repository, KeywordSortOrder::Name, 1, 10).await?;
    assert_eq!(by_name, ["alpha", "middle", "zeta"]);

    let first = repository.keywords(KeywordSortOrder::Name, 1, 2).await?;
    assert_eq!(first.total, 3);
    let second = repository.keywords(KeywordSortOrder::Name, 2, 2).await?;
    assert_eq!(second.total, 3);
    assert_eq!(
        first
            .items
            .iter()
            .chain(&second.items)
            .map(|item| item.keyword.normalized().to_owned())
            .collect::<Vec<_>>(),
        ["alpha", "middle", "zeta"]
    );

    let past_end = repository.keywords(KeywordSortOrder::Name, 9, 2).await?;
    assert!(past_end.items.is_empty());
    assert_eq!(past_end.total, 0);
    Ok(())
}

async fn keyword_names(
    repository: &PostgresRepository,
    sort: KeywordSortOrder,
    page: u32,
    per_page: u16,
) -> Result<Vec<String>, Box<dyn Error>> {
    Ok(repository
        .keywords(sort, page, per_page)
        .await?
        .items
        .into_iter()
        .map(|item| item.keyword.normalized().to_owned())
        .collect())
}

#[sqlx::test(migrations = "../../migrations")]
async fn version_history_pages_in_descending_domain_order(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let package = create_package(&pool, "Rux", "Targets").await?;
    for (version, published) in [
        ("1.0.0", "2026-01-01T00:00:00Z"),
        ("1.0.0+native", "2026-01-02T00:00:00Z"),
        ("1.0.0+portable", "2026-01-03T00:00:00Z"),
        ("2.0.0-beta.10", "2026-01-04T00:00:00Z"),
    ] {
        insert_version(&pool, package, version, published, false).await?;
    }

    let first = repository
        .package_version_history(&identity("rux"), &identity("targets"), None, 2)
        .await?
        .expect("package exists");
    assert_eq!(
        first
            .iter()
            .map(|item| item.version.as_str())
            .collect::<Vec<_>>(),
        ["2.0.0-beta.10", "1.0.0+portable"]
    );
    let second = repository
        .package_version_history(
            &identity("rux"),
            &identity("targets"),
            Some(&SemanticVersion::new("1.0.0+portable")?),
            10,
        )
        .await?
        .expect("package exists");
    assert_eq!(
        second
            .iter()
            .map(|item| item.version.as_str())
            .collect::<Vec<_>>(),
        ["1.0.0+native", "1.0.0"]
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn highlights_use_active_representatives_and_bounded_download_window(
    pool: PgPool,
) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let recent = create_package(&pool, "Rux", "Recent").await?;
    let recent_version =
        insert_version(&pool, recent, "1.0.0", "2026-07-20T00:00:00Z", false).await?;
    let popular = create_package(&pool, "Rux", "Popular").await?;
    let popular_version =
        insert_version(&pool, popular, "1.0.0", "2026-06-01T00:00:00Z", false).await?;
    let hidden = create_package(&pool, "Rux", "Hidden").await?;
    let hidden_version =
        insert_version(&pool, hidden, "1.0.0", "2026-07-25T00:00:00Z", true).await?;
    let future = create_package(&pool, "Rux", "Future").await?;
    let future_version =
        insert_version(&pool, future, "1.0.0", "2026-08-03T00:00:00Z", false).await?;
    for _ in 0..3 {
        add_download(&pool, popular_version, "2026-07-15T00:00:00Z").await?;
    }
    add_download(&pool, popular_version, "2026-06-01T00:00:00Z").await?;
    add_download(&pool, recent_version, "2026-07-16T00:00:00Z").await?;
    for _ in 0..5 {
        add_download(&pool, hidden_version, "2026-07-17T00:00:00Z").await?;
    }
    for _ in 0..10 {
        add_download(&pool, future_version, "2026-07-18T00:00:00Z").await?;
    }

    let highlights = repository
        .package_highlights(
            timestamp("2026-07-03T12:00:00Z"),
            timestamp("2026-08-02T12:00:00Z"),
            10,
        )
        .await?;
    assert_eq!(
        highlights
            .recent
            .iter()
            .map(|item| item.package.as_str())
            .collect::<Vec<_>>(),
        ["Recent", "Popular"]
    );
    assert_eq!(
        highlights
            .popular
            .iter()
            .map(|item| (item.package.as_str(), item.downloads))
            .collect::<Vec<_>>(),
        [("Popular", Some(3)), ("Recent", Some(1))]
    );
    Ok(())
}

fn identity(value: &str) -> IdentitySegment {
    IdentitySegment::new(value).expect("valid identity")
}

fn timestamp(value: &str) -> OffsetDateTime {
    OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .expect("valid timestamp")
}

async fn create_package(
    pool: &PgPool,
    namespace: &str,
    package: &str,
) -> Result<Uuid, sqlx::Error> {
    let namespace_id = query_scalar::<_, Uuid>(
        "INSERT INTO namespaces (display_name)
         VALUES ($1)
         ON CONFLICT (normalized_name) DO UPDATE SET display_name = EXCLUDED.display_name
         RETURNING id",
    )
    .bind(namespace)
    .fetch_one(pool)
    .await?;
    query_scalar::<_, Uuid>(
        "INSERT INTO packages (namespace_id, display_name) VALUES ($1, $2) RETURNING id",
    )
    .bind(namespace_id)
    .bind(package)
    .fetch_one(pool)
    .await
}

async fn insert_version(
    pool: &PgPool,
    package_id: Uuid,
    value: &str,
    published_at: &str,
    yanked: bool,
) -> Result<Uuid, sqlx::Error> {
    let version = SemanticVersion::new(value).expect("valid version");
    query_scalar::<_, Uuid>(
        "INSERT INTO package_versions (
             package_id, version, major, minor, patch, prerelease, build_metadata,
             manifest_schema_version, min_rux, package_type, description,
             normalized_manifest, artifact_sha256, artifact_size, storage_key,
             artifact_file_count, artifact_expanded_bytes, source_file_count,
             source_line_count, published_at, yanked_at
         ) VALUES (
             $1, $2, $3::NUMERIC, $4::NUMERIC, $5::NUMERIC, $6, $7,
             1, '0.4.0', 'library', $2, '{}', decode(repeat('ab', 32), 'hex'), 1,
             $8, 2, 1, 1, 0, $9,
             CASE WHEN $10 THEN $9::TIMESTAMPTZ + interval '1 second' ELSE NULL END
         ) RETURNING id",
    )
    .bind(package_id)
    .bind(version.as_str())
    .bind(version.major().to_string())
    .bind(version.minor().to_string())
    .bind(version.patch().to_string())
    .bind(version.prerelease())
    .bind(version.build())
    .bind(format!(
        "packages/{package_id}/{}.ruxpkg",
        value.replace('+', "-")
    ))
    .bind(timestamp(published_at))
    .bind(yanked)
    .fetch_one(pool)
    .await
}

async fn add_dependency(
    pool: &PgPool,
    version_id: Uuid,
    alias: &str,
    namespace: &str,
    package: &str,
    range: &str,
) -> Result<(), sqlx::Error> {
    query(
        "INSERT INTO dependencies (
             package_version_id, display_alias, target_namespace_display_name,
             target_package_display_name, version_range
         ) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(version_id)
    .bind(alias)
    .bind(namespace)
    .bind(package)
    .bind(range)
    .execute(pool)
    .await?;
    Ok(())
}

async fn add_keyword(pool: &PgPool, version_id: Uuid, keyword: &str) -> Result<(), sqlx::Error> {
    // `(package_version_id, ordinal)` is the primary key, so the ordinal has to
    // advance for a version carrying more than one keyword.
    query(
        "INSERT INTO package_version_keywords (package_version_id, ordinal, display_name)
         SELECT $1, coalesce(max(ordinal) + 1, 0), $2
         FROM package_version_keywords
         WHERE package_version_id = $1",
    )
    .bind(version_id)
    .bind(keyword)
    .execute(pool)
    .await?;
    Ok(())
}

async fn add_download(
    pool: &PgPool,
    version_id: Uuid,
    occurred_at: &str,
) -> Result<(), sqlx::Error> {
    query("INSERT INTO download_events (package_version_id, occurred_at) VALUES ($1, $2)")
        .bind(version_id)
        .bind(timestamp(occurred_at))
        .execute(pool)
        .await?;
    Ok(())
}
