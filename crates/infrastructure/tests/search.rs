use std::error::Error;
use uuid::Uuid;

use rux_application::{
    PackageKind, PackageSearchCriteria, PackageSearchReader, PackageSortDirection, PackageSortOrder,
};
use rux_domain::{IdentitySegment, SemanticVersion};
use rux_infrastructure::PostgresRepository;
use sqlx::{PgPool, query, query_scalar};

type TestResult = Result<(), Box<dyn Error>>;

#[sqlx::test(migrations = "../../migrations")]
async fn representative_selection_is_stable_first_and_uses_domain_version_order(
    pool: PgPool,
) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let package = create_package(&pool, "Rux", "Parser").await?;
    insert_version(
        &pool,
        package,
        "1.0.0",
        PackageKind::Library,
        Some("stable active"),
        false,
    )
    .await?;
    insert_version(
        &pool,
        package,
        "2.0.0-beta.10",
        PackageKind::Library,
        Some("prerelease active"),
        false,
    )
    .await?;
    insert_version(
        &pool,
        package,
        "1.1.0",
        PackageKind::Library,
        Some("stable yanked"),
        true,
    )
    .await?;

    let all_yanked = create_package(&pool, "Rux", "Legacy").await?;
    insert_version(&pool, all_yanked, "1.0.0", PackageKind::Source, None, true).await?;
    insert_version(
        &pool,
        all_yanked,
        "2.0.0-beta.1",
        PackageKind::Source,
        None,
        true,
    )
    .await?;

    let builds = create_package(&pool, "Rux", "Targets").await?;
    insert_version(
        &pool,
        builds,
        "3.0.0+native.09",
        PackageKind::Program,
        None,
        false,
    )
    .await?;
    insert_version(
        &pool,
        builds,
        "3.0.0+portable.1",
        PackageKind::Program,
        None,
        false,
    )
    .await?;

    let results = repository.search_packages(&browse(), 1, 10).await?.items;
    assert_eq!(version_for(&results, "Parser"), "1.0.0");
    assert_eq!(version_for(&results, "Legacy"), "1.0.0");
    assert!(
        results
            .iter()
            .find(|record| record.package.as_str() == "Legacy")
            .expect("legacy package")
            .yanked
    );
    assert_eq!(version_for(&results, "Targets"), "3.0.0+portable.1");
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn search_ranks_literal_signals_and_filters_representative_metadata(
    pool: PgPool,
) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    let exact = create_package(&pool, "Rux", "Json").await?;
    let exact_version = insert_version(
        &pool,
        exact,
        "1.0.0",
        PackageKind::Library,
        Some("Data format"),
        false,
    )
    .await?;
    add_keyword(&pool, exact_version, "serialization").await?;

    let keyword = create_package(&pool, "Community", "Codec").await?;
    let keyword_version = insert_version(
        &pool,
        keyword,
        "1.0.0",
        PackageKind::Source,
        Some("Encoding helpers"),
        false,
    )
    .await?;
    add_keyword(&pool, keyword_version, "Json").await?;

    let text = create_package(&pool, "Community", "Streaming").await?;
    insert_version(
        &pool,
        text,
        "1.0.0",
        PackageKind::Library,
        Some("Streaming JSON documents"),
        false,
    )
    .await?;

    let results = repository
        .search_packages(&query_criteria("json"), 1, 10)
        .await?
        .items;
    assert_eq!(
        results
            .iter()
            .map(|record| record.package.as_str())
            .collect::<Vec<_>>(),
        ["Json", "Codec", "Streaming"]
    );

    let filtered = repository
        .search_packages(
            &PackageSearchCriteria {
                query: None,
                identity_query: None,
                namespace: Some(identity("community")),
                keyword: Some(identity("json")),
                package_type: Some(PackageKind::Source),
                sort: PackageSortOrder::Name,
                order: PackageSortDirection::Ascending,
            },
            1,
            10,
        )
        .await?;
    assert_eq!(filtered.items.len(), 1);
    assert_eq!(filtered.items[0].package.as_str(), "Codec");
    assert_eq!(filtered.total, 1);

    let literal_wildcard = repository
        .search_packages(&query_criteria("%"), 1, 10)
        .await?;
    assert!(literal_wildcard.items.is_empty());
    assert_eq!(literal_wildcard.total, 0);
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn offset_pages_cover_every_row_once_and_report_the_full_total(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    for package_name in ["Alpha", "Beta", "Gamma"] {
        let package = create_package(&pool, "Rux", package_name).await?;
        insert_version(&pool, package, "1.0.0", PackageKind::Source, None, false).await?;
    }

    let first = repository.search_packages(&browse(), 1, 2).await?;
    assert_eq!(first.items.len(), 2);
    // The total describes the whole result set, not the page.
    assert_eq!(first.total, 3);
    let second = repository.search_packages(&browse(), 2, 2).await?;
    assert_eq!(second.total, 3);
    assert_eq!(
        first
            .items
            .iter()
            .chain(&second.items)
            .map(|record| record.package.as_str())
            .collect::<Vec<_>>(),
        ["Alpha", "Beta", "Gamma"]
    );

    let descending = repository
        .search_packages(
            &PackageSearchCriteria {
                order: PackageSortDirection::Descending,
                ..browse()
            },
            1,
            3,
        )
        .await?;
    assert_eq!(
        descending
            .items
            .iter()
            .map(|record| record.package.as_str())
            .collect::<Vec<_>>(),
        ["Gamma", "Beta", "Alpha"]
    );

    // A page past the end is empty rather than an error, and carries no total
    // of its own because the window count rides on the rows.
    let past_end = repository.search_packages(&browse(), 9, 2).await?;
    assert!(past_end.items.is_empty());
    assert_eq!(past_end.total, 0);
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn sort_orders_select_download_counts_and_recency(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    // Alpha is the most downloaded all time, Beta the most downloaded lately,
    // Gamma has never been downloaded at all and must still appear.
    let alpha = insert_sole_version(&pool, "Alpha").await?;
    let beta = insert_sole_version(&pool, "Beta").await?;
    insert_sole_version(&pool, "Gamma").await?;
    add_downloads(&pool, alpha, 5, 400).await?;
    add_downloads(&pool, alpha, 1, 0).await?;
    add_downloads(&pool, beta, 4, 0).await?;

    let by_total = packages_sorted(
        &repository,
        PackageSortOrder::Downloads,
        PackageSortDirection::Descending,
    )
    .await?;
    assert_eq!(by_total, ["Alpha", "Beta", "Gamma"]);
    assert_eq!(
        packages_sorted(
            &repository,
            PackageSortOrder::Downloads,
            PackageSortDirection::Ascending,
        )
        .await?,
        ["Gamma", "Beta", "Alpha"]
    );
    let by_recent = packages_sorted(
        &repository,
        PackageSortOrder::RecentDownloads,
        PackageSortDirection::Descending,
    )
    .await?;
    assert_eq!(by_recent, ["Beta", "Alpha", "Gamma"]);
    assert_eq!(
        packages_sorted(
            &repository,
            PackageSortOrder::RecentDownloads,
            PackageSortDirection::Ascending,
        )
        .await?,
        ["Gamma", "Alpha", "Beta"]
    );

    let counts = repository
        .search_packages(&browse(), 1, 10)
        .await?
        .items
        .into_iter()
        .map(|record| {
            (
                record.package.as_str().to_owned(),
                record.downloads_total,
                record.downloads_30d,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        counts,
        [
            ("Alpha".into(), 6, 1),
            ("Beta".into(), 4, 4),
            ("Gamma".into(), 0, 0),
        ]
    );
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn created_and_updated_sorts_use_distinct_timestamps(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    // Alpha is the older package but was released again yesterday; Beta is
    // newer and has not shipped since. The two orderings must disagree.
    let alpha = create_package_at(&pool, "Rux", "Alpha", 10).await?;
    insert_version_published(&pool, alpha, "1.0.0", PackageKind::Source, None, false, 10).await?;
    insert_version_published(&pool, alpha, "2.0.0", PackageKind::Source, None, false, 1).await?;
    let beta = create_package_at(&pool, "Rux", "Beta", 2).await?;
    insert_version_published(&pool, beta, "1.0.0", PackageKind::Source, None, false, 5).await?;

    assert_eq!(
        packages_sorted(
            &repository,
            PackageSortOrder::Created,
            PackageSortDirection::Descending,
        )
        .await?,
        ["Beta", "Alpha"]
    );
    assert_eq!(
        packages_sorted(
            &repository,
            PackageSortOrder::Updated,
            PackageSortDirection::Descending,
        )
        .await?,
        ["Alpha", "Beta"]
    );
    assert_eq!(
        packages_sorted(
            &repository,
            PackageSortOrder::Created,
            PackageSortDirection::Ascending,
        )
        .await?,
        ["Alpha", "Beta"]
    );
    assert_eq!(
        packages_sorted(
            &repository,
            PackageSortOrder::Updated,
            PackageSortDirection::Ascending,
        )
        .await?,
        ["Beta", "Alpha"]
    );
    Ok(())
}

fn browse() -> PackageSearchCriteria {
    PackageSearchCriteria {
        query: None,
        identity_query: None,
        namespace: None,
        keyword: None,
        package_type: None,
        sort: PackageSortOrder::Name,
        order: PackageSortDirection::Ascending,
    }
}

fn query_criteria(query: &str) -> PackageSearchCriteria {
    PackageSearchCriteria {
        query: Some(query.into()),
        identity_query: Some(query.to_lowercase().replace('_', "-")),
        sort: PackageSortOrder::Relevance,
        order: PackageSortDirection::Descending,
        ..browse()
    }
}

async fn packages_sorted(
    repository: &PostgresRepository,
    sort: PackageSortOrder,
    order: PackageSortDirection,
) -> Result<Vec<String>, Box<dyn Error>> {
    Ok(repository
        .search_packages(
            &PackageSearchCriteria {
                sort,
                order,
                ..browse()
            },
            1,
            10,
        )
        .await?
        .items
        .into_iter()
        .map(|record| record.package.as_str().to_owned())
        .collect())
}

fn identity(value: &str) -> IdentitySegment {
    IdentitySegment::new(value).expect("valid test identity")
}

fn version_for<'a>(records: &'a [rux_application::PackageSearchRecord], package: &str) -> &'a str {
    records
        .iter()
        .find(|record| record.package.as_str() == package)
        .expect("package should be searchable")
        .version
        .as_str()
}

async fn create_package(
    pool: &PgPool,
    namespace: &str,
    package: &str,
) -> Result<Uuid, sqlx::Error> {
    create_package_at(pool, namespace, package, 0).await
}

async fn create_package_at(
    pool: &PgPool,
    namespace: &str,
    package: &str,
    created_days_ago: i32,
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
        "INSERT INTO packages (namespace_id, display_name, created_at)
         VALUES ($1, $2, now() - make_interval(days => $3))
         RETURNING id",
    )
    .bind(namespace_id)
    .bind(package)
    .bind(created_days_ago)
    .fetch_one(pool)
    .await
}

async fn insert_version(
    pool: &PgPool,
    package_id: Uuid,
    value: &str,
    package_type: PackageKind,
    description: Option<&str>,
    yanked: bool,
) -> Result<Uuid, sqlx::Error> {
    insert_version_published(
        pool,
        package_id,
        value,
        package_type,
        description,
        yanked,
        0,
    )
    .await
}

/// Inserts a version released `days_ago` in the past.
///
/// `published_at` is covered by the immutability trigger, so a test that needs
/// a specific release date has to supply it at insert time.
async fn insert_version_published(
    pool: &PgPool,
    package_id: Uuid,
    value: &str,
    package_type: PackageKind,
    description: Option<&str>,
    yanked: bool,
    days_ago: i32,
) -> Result<Uuid, sqlx::Error> {
    let version = SemanticVersion::new(value).expect("valid test version");
    let suffix = value.replace(['+', '.'], "-");
    query_scalar::<_, Uuid>(
        "INSERT INTO package_versions (
             package_id, version, major, minor, patch, prerelease, build_metadata,
             manifest_schema_version, min_rux, package_type, description,
             normalized_manifest, artifact_sha256, artifact_size, storage_key,
             artifact_file_count, artifact_expanded_bytes, source_file_count,
             source_line_count, published_at, yanked_at
         ) VALUES (
             $1, $2, $3::NUMERIC, $4::NUMERIC, $5::NUMERIC, $6, $7,
             1, '0.4.0', $8, $9, '{}', decode(repeat('ab', 32), 'hex'), 1024,
             $10, 2, 2048, 1, 10,
             now() - make_interval(days => $12),
             CASE WHEN $11 THEN now() ELSE NULL END
         )
         RETURNING id",
    )
    .bind(package_id)
    .bind(version.as_str())
    .bind(version.major().to_string())
    .bind(version.minor().to_string())
    .bind(version.patch().to_string())
    .bind(version.prerelease())
    .bind(version.build())
    .bind(match package_type {
        PackageKind::Program => "program",
        PackageKind::Library => "library",
        PackageKind::Source => "source",
    })
    .bind(description)
    .bind(format!("packages/{package_id}-{suffix}.ruxpkg"))
    .bind(yanked)
    .bind(days_ago)
    .fetch_one(pool)
    .await
}

async fn insert_sole_version(pool: &PgPool, package: &str) -> Result<Uuid, sqlx::Error> {
    let id = create_package(pool, "Rux", package).await?;
    insert_version(pool, id, "1.0.0", PackageKind::Source, None, false).await
}

/// Records `count` downloads `days_ago` in the past, so a row can sit inside or
/// outside the 30-day recent window on purpose.
async fn add_downloads(
    pool: &PgPool,
    version: Uuid,
    count: i32,
    days_ago: i32,
) -> Result<(), sqlx::Error> {
    query(
        "INSERT INTO download_events (package_version_id, occurred_at)
         SELECT $1, now() - make_interval(days => $3)
         FROM generate_series(1, $2)",
    )
    .bind(version)
    .bind(count)
    .bind(days_ago)
    .execute(pool)
    .await?;
    Ok(())
}

async fn add_keyword(pool: &PgPool, version: Uuid, keyword: &str) -> Result<(), sqlx::Error> {
    query(
        "INSERT INTO package_version_keywords (package_version_id, ordinal, display_name)
         VALUES ($1, 0, $2)",
    )
    .bind(version)
    .bind(keyword)
    .execute(pool)
    .await?;
    Ok(())
}
