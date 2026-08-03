use serde_json::Value;
use sqlx::{Error, PgPool, query, query_as, query_scalar};
use uuid::Uuid;

#[sqlx::test(migrations = "../../migrations")]
async fn search_vector_is_generated_and_weighted(pool: PgPool) -> Result<(), Error> {
    assert!(extension_exists(&pool).await?);

    let package_id = create_package(&pool).await?;
    insert_version(
        &pool,
        package_id,
        "1.0.0",
        Some("needle"),
        None,
        "description",
    )
    .await?;
    insert_version(&pool, package_id, "1.0.1", None, Some("needle"), "readme").await?;

    let ranks = query_as::<_, (String, f32)>(
        "SELECT
             version,
             ts_rank(search_vector, plainto_tsquery('simple', 'needle'))
         FROM package_versions
         ORDER BY version",
    )
    .fetch_all(&pool)
    .await?;

    assert_eq!(ranks.len(), 2);
    assert_eq!(ranks[0].0, "1.0.0");
    assert_eq!(ranks[1].0, "1.0.1");
    assert!(ranks[0].1 > ranks[1].1);
    assert!(ranks.iter().all(|(_, rank)| *rank > 0.0));
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn catalog_search_queries_use_search_indexes(pool: PgPool) -> Result<(), Error> {
    let package_id = create_package(&pool).await?;
    let version_id = insert_version(
        &pool,
        package_id,
        "1.0.0",
        Some("Registry search utilities"),
        Some("Tools for finding packages"),
        "plans",
    )
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
             package_version_id, display_alias, target_namespace_display_name,
             target_package_display_name, version_range
         ) VALUES ($1, 'Fast_Json', 'Acme_Tools', 'Fast_Json', '^2.0')",
    )
    .bind(version_id)
    .execute(&pool)
    .await?;

    query(
        "INSERT INTO package_versions (
             package_id, version, major, minor, patch, manifest_schema_version, min_rux,
             package_type, normalized_manifest, artifact_sha256, artifact_size, storage_key,
             artifact_file_count, artifact_expanded_bytes, source_file_count, source_line_count
         )
         SELECT
             $1, '2.0.' || sequence, 2, 0, sequence, 1, '0.4.0', 'source', '{}',
             decode(repeat('cd', 32), 'hex'), 1024,
             'packages/noise-' || sequence || '.ruxpkg', 2, 2048, 1, 10
         FROM generate_series(1, 128) AS sequence",
    )
    .bind(package_id)
    .execute(&pool)
    .await?;
    query(
        "INSERT INTO package_version_keywords (package_version_id, ordinal, display_name)
         SELECT id, 0, 'Noise-' || id
         FROM package_versions
         WHERE package_id = $1 AND id <> $2",
    )
    .bind(package_id)
    .bind(version_id)
    .execute(&pool)
    .await?;
    query("ANALYZE package_versions, package_version_keywords, dependencies")
        .execute(&pool)
        .await?;

    let mut transaction = pool.begin().await?;
    query("SET LOCAL enable_seqscan = off")
        .execute(&mut *transaction)
        .await?;

    assert_explain_uses(
        &mut transaction,
        "EXPLAIN (FORMAT JSON, COSTS OFF)
         SELECT id FROM package_versions
         WHERE search_vector @@ plainto_tsquery('simple', 'registry')",
        "package_versions_search_vector_idx",
    )
    .await?;
    assert_explain_uses(
        &mut transaction,
        &format!(
            "EXPLAIN (FORMAT JSON, COSTS OFF)
             SELECT id FROM package_versions
             WHERE package_id = '{package_id}'
             ORDER BY
                 (yanked_at IS NULL) DESC,
                 (prerelease IS NULL) DESC,
                 major DESC,
                 minor DESC,
                 patch DESC,
                 prerelease_sort_key DESC,
                 (build_metadata IS NOT NULL) DESC,
                 build_metadata_sort_key DESC,
                 id DESC
             LIMIT 1"
        ),
        "package_versions_representative_idx",
    )
    .await?;
    assert_explain_uses(
        &mut transaction,
        &format!(
            "EXPLAIN (FORMAT JSON, COSTS OFF)
             SELECT version FROM package_versions
             WHERE package_id = '{package_id}'
             ORDER BY
                 major DESC,
                 minor DESC,
                 patch DESC,
                 (prerelease IS NULL) DESC,
                 prerelease_sort_key DESC,
                 (build_metadata IS NOT NULL) DESC,
                 build_metadata_sort_key DESC
             LIMIT 20"
        ),
        "package_versions_history_idx",
    )
    .await?;
    assert_explain_uses(
        &mut transaction,
        "EXPLAIN (FORMAT JSON, COSTS OFF)
         SELECT package_version_id FROM download_events
         WHERE occurred_at >= now() - interval '30 days'
         ORDER BY occurred_at DESC",
        "download_events_occurred_at_package_version_idx",
    )
    .await?;
    assert_explain_uses(
        &mut transaction,
        "EXPLAIN (FORMAT JSON, COSTS OFF)
         SELECT id FROM namespaces WHERE normalized_name % 'registry-tool'",
        "namespaces_normalized_name_trgm_idx",
    )
    .await?;
    assert_explain_uses(
        &mut transaction,
        "EXPLAIN (FORMAT JSON, COSTS OFF)
         SELECT id FROM packages WHERE normalized_name = 'example-package'",
        "packages_normalized_name_idx",
    )
    .await?;
    assert_explain_uses(
        &mut transaction,
        "EXPLAIN (FORMAT JSON, COSTS OFF)
         SELECT id FROM packages WHERE normalized_name % 'example-package'",
        "packages_normalized_name_trgm_idx",
    )
    .await?;
    assert_explain_uses(
        &mut transaction,
        "EXPLAIN (FORMAT JSON, COSTS OFF)
         SELECT package_version_id FROM package_version_keywords
         WHERE normalized_name = 'registry-tools'",
        "package_version_keywords_normalized_name_idx",
    )
    .await?;
    // Remove covering exact indexes only inside this rolled-back transaction so
    // the following plan must exercise the trigram operator class.
    query("DROP INDEX package_version_keywords_normalized_name_idx")
        .execute(&mut *transaction)
        .await?;
    query(
        "ALTER TABLE package_version_keywords
         DROP CONSTRAINT package_version_keywords_normalized_name_unique",
    )
    .execute(&mut *transaction)
    .await?;
    assert_explain_uses(
        &mut transaction,
        "EXPLAIN (FORMAT JSON, COSTS OFF)
         SELECT package_version_id FROM package_version_keywords
         WHERE normalized_name % 'registry-tool'",
        "package_version_keywords_normalized_name_trgm_idx",
    )
    .await?;
    assert_explain_uses(
        &mut transaction,
        "EXPLAIN (FORMAT JSON, COSTS OFF)
         SELECT package_version_id FROM dependencies
         WHERE target_namespace_normalized_name = 'acme-tools'
           AND target_package_normalized_name = 'fast-json'",
        "dependencies_target_identity_idx",
    )
    .await?;

    transaction.rollback().await?;
    Ok(())
}

async fn assert_explain_uses(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    statement: &str,
    expected_index: &str,
) -> Result<(), Error> {
    let plan = query_scalar::<_, Value>(statement)
        .fetch_one(&mut **transaction)
        .await?;
    assert!(
        plan_uses_index(&plan, expected_index),
        "expected plan to use {expected_index}: {plan}"
    );
    Ok(())
}

fn plan_uses_index(value: &Value, expected_index: &str) -> bool {
    match value {
        Value::Array(values) => values
            .iter()
            .any(|value| plan_uses_index(value, expected_index)),
        Value::Object(values) => {
            values.get("Index Name").and_then(Value::as_str) == Some(expected_index)
                || values
                    .values()
                    .any(|value| plan_uses_index(value, expected_index))
        }
        _ => false,
    }
}

async fn create_package(pool: &PgPool) -> Result<Uuid, Error> {
    let namespace_id = query_scalar::<_, Uuid>(
        "INSERT INTO namespaces (display_name) VALUES ('Registry_Tools') RETURNING id",
    )
    .fetch_one(pool)
    .await?;
    query_scalar::<_, Uuid>(
        "INSERT INTO packages (namespace_id, display_name)
         VALUES ($1, 'Example_Package')
         RETURNING id",
    )
    .bind(namespace_id)
    .fetch_one(pool)
    .await
}

async fn insert_version(
    pool: &PgPool,
    package_id: Uuid,
    version: &str,
    description: Option<&str>,
    readme_text: Option<&str>,
    storage_suffix: &str,
) -> Result<Uuid, Error> {
    query_scalar::<_, Uuid>(
        "INSERT INTO package_versions (
             package_id, version, major, minor, patch, manifest_schema_version, min_rux,
             package_type, description, readme_path, readme_text, normalized_manifest,
             artifact_sha256, artifact_size, storage_key, artifact_file_count,
             artifact_expanded_bytes, source_file_count, source_line_count
         ) VALUES (
             $1, $2, 1, 0, 0, 1, '0.4.0', 'source', $3,
             CASE WHEN $4::TEXT IS NULL THEN NULL ELSE 'README.md' END,
             $4, '{}', decode(repeat('ab', 32), 'hex'), 1024, $5, 2, 2048, 1, 10
         )
         RETURNING id",
    )
    .bind(package_id)
    .bind(version)
    .bind(description)
    .bind(readme_text)
    .bind(format!("packages/example-{storage_suffix}.ruxpkg"))
    .fetch_one(pool)
    .await
}

async fn extension_exists(pool: &PgPool) -> Result<bool, Error> {
    query_scalar::<_, bool>("SELECT EXISTS (SELECT FROM pg_extension WHERE extname = 'pg_trgm')")
        .fetch_one(pool)
        .await
}
