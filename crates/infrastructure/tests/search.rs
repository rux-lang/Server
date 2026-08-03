use std::error::Error;
use uuid::Uuid;

use rux_application::{
    PackageKind, PackageSearchBoundary, PackageSearchCriteria, PackageSearchReader,
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

    let results = repository.search_packages(&browse(), None, 10).await?;
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
        .search_packages(&query_criteria("json"), None, 10)
        .await?;
    assert_eq!(
        results
            .iter()
            .map(|record| record.package.as_str())
            .collect::<Vec<_>>(),
        ["Json", "Codec", "Streaming"]
    );
    assert_eq!(
        results
            .iter()
            .map(|record| record.match_class)
            .collect::<Vec<_>>(),
        [4, 3, 1]
    );

    let filtered = repository
        .search_packages(
            &PackageSearchCriteria {
                query: None,
                identity_query: None,
                namespace: Some(identity("community")),
                keyword: Some(identity("json")),
                package_type: Some(PackageKind::Source),
            },
            None,
            10,
        )
        .await?;
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].package.as_str(), "Codec");

    let literal_wildcard = repository
        .search_packages(&query_criteria("%"), None, 10)
        .await?;
    assert!(literal_wildcard.is_empty());
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn keyset_boundary_pages_rank_and_identity_without_duplicates(pool: PgPool) -> TestResult {
    let repository = PostgresRepository::new(pool.clone());
    for package_name in ["Alpha", "Beta", "Gamma"] {
        let package = create_package(&pool, "Rux", package_name).await?;
        insert_version(&pool, package, "1.0.0", PackageKind::Source, None, false).await?;
    }

    let first = repository.search_packages(&browse(), None, 2).await?;
    assert_eq!(first.len(), 2);
    let last = first.last().expect("first page should have a boundary");
    let boundary = PackageSearchBoundary {
        match_class: last.match_class,
        relevance: last.relevance,
        namespace: last.namespace.normalized().to_owned(),
        package: last.package.normalized().to_owned(),
    };
    let second = repository
        .search_packages(&browse(), Some(&boundary), 2)
        .await?;
    assert_eq!(
        first
            .iter()
            .chain(&second)
            .map(|record| record.package.as_str())
            .collect::<Vec<_>>(),
        ["Alpha", "Beta", "Gamma"]
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
    }
}

fn query_criteria(query: &str) -> PackageSearchCriteria {
    PackageSearchCriteria {
        query: Some(query.into()),
        identity_query: Some(query.to_lowercase().replace('_', "-")),
        ..browse()
    }
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
        "INSERT INTO packages (namespace_id, display_name)
         VALUES ($1, $2)
         RETURNING id",
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
    package_type: PackageKind,
    description: Option<&str>,
    yanked: bool,
) -> Result<Uuid, sqlx::Error> {
    let version = SemanticVersion::new(value).expect("valid test version");
    let suffix = value.replace(['+', '.'], "-");
    query_scalar::<_, Uuid>(
        "INSERT INTO package_versions (
             package_id, version, major, minor, patch, prerelease, build_metadata,
             manifest_schema_version, min_rux, package_type, description,
             normalized_manifest, artifact_sha256, artifact_size, storage_key,
             artifact_file_count, artifact_expanded_bytes, source_file_count,
             source_line_count, yanked_at
         ) VALUES (
             $1, $2, $3::NUMERIC, $4::NUMERIC, $5::NUMERIC, $6, $7,
             1, '0.4.0', $8, $9, '{}', decode(repeat('ab', 32), 'hex'), 1024,
             $10, 2, 2048, 1, 10,
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
    .fetch_one(pool)
    .await
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
