use std::env;
use std::error::Error;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rux_artifact::{ARTIFACT_MAX_BYTES, inspect_artifact};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, query, query_scalar};
use uuid::Uuid;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

type DynError = Box<dyn Error + Send + Sync>;

const DATABASE_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const TOKEN_SECRET: [u8; 32] = [42; 32];
const PUBLICATION_NAMESPACE: &str = "Perf_Publish";
const PUBLICATION_PACKAGE: &str = "Perf_Package";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Profile {
    Smoke,
    Launch,
}

impl Profile {
    fn parse(value: &str) -> Result<Self, DynError> {
        match value {
            "smoke" => Ok(Self::Smoke),
            "launch" => Ok(Self::Launch),
            _ => Err(format!("unknown profile {value:?}; expected smoke or launch").into()),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Launch => "launch",
        }
    }

    const fn scale(self) -> Scale {
        match self {
            Self::Smoke => Scale {
                namespaces: 10,
                packages: 100,
                versions_per_package: 10,
                downloads: 10_000,
            },
            Self::Launch => Scale {
                namespaces: 1_000,
                packages: 10_000,
                versions_per_package: 10,
                downloads: 1_000_000,
            },
        }
    }

    const fn fixture_counts(self) -> (usize, usize) {
        match self {
            Self::Smoke => (2, 1),
            Self::Launch => (20, 10),
        }
    }

    const fn database_budget(self) -> u64 {
        match self {
            Self::Smoke => 64 * 1024 * 1024,
            Self::Launch => DATABASE_BUDGET_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
struct Scale {
    namespaces: i64,
    packages: i64,
    versions_per_package: i64,
    downloads: i64,
}

impl Scale {
    fn versions(self) -> Result<i64, DynError> {
        self.packages
            .checked_mul(self.versions_per_package)
            .ok_or_else(|| "version count overflow".into())
    }

    fn validate(self) -> Result<(), DynError> {
        if self.namespaces < 2
            || self.packages < 1
            || self.versions_per_package < 1
            || self.downloads < 0
        {
            return Err("performance scale contains an invalid count".into());
        }
        self.versions()?;
        Ok(())
    }
}

#[derive(Serialize)]
struct SeedReport {
    profile: &'static str,
    database: String,
    scale: Scale,
    package_versions: i64,
    authors: i64,
    keywords: i64,
    dependencies: i64,
    audit_records: i64,
    download_events: i64,
}

#[derive(Serialize)]
struct SizeReport {
    profile: &'static str,
    database: String,
    database_bytes: u64,
    budget_bytes: u64,
    within_budget: bool,
}

#[derive(Serialize)]
struct FixtureManifest {
    profile: &'static str,
    run_id: String,
    bearer_token: String,
    namespace: &'static str,
    package: &'static str,
    typical: Vec<PublicationFixture>,
    large: Vec<PublicationFixture>,
}

#[derive(Serialize)]
struct FixtureReport<'a> {
    profile: &'static str,
    run_id: &'a str,
    namespace: &'static str,
    package: &'static str,
    typical: &'a [PublicationFixture],
    large: &'a [PublicationFixture],
}

impl<'a> From<&'a FixtureManifest> for FixtureReport<'a> {
    fn from(manifest: &'a FixtureManifest) -> Self {
        Self {
            profile: manifest.profile,
            run_id: &manifest.run_id,
            namespace: manifest.namespace,
            package: manifest.package,
            typical: &manifest.typical,
            large: &manifest.large,
        }
    }
}

#[derive(Serialize)]
struct PublicationFixture {
    version: String,
    manifest_path: String,
    package_path: String,
    package_bytes: u64,
}

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let mut args = env::args().skip(1);
    let command = args.next().ok_or_else(usage)?;
    let profile = Profile::parse(&args.next().ok_or_else(usage)?)?;

    match command.as_str() {
        "migrate" => {
            ensure_no_extra_args(args)?;
            let pool = database_pool().await?;
            let database = assert_database_name(&pool).await?;
            sqlx::migrate!("../../migrations").run(&pool).await?;
            print_json(&serde_json::json!({
                "profile": profile.name(),
                "database": database,
                "migrated": true
            }))?;
        }
        "seed" => {
            ensure_no_extra_args(args)?;
            let pool = database_pool().await?;
            let report = seed(&pool, profile).await?;
            print_json(&report)?;
        }
        "size" => {
            ensure_no_extra_args(args)?;
            let pool = database_pool().await?;
            let report = database_size(&pool, profile).await?;
            print_json(&report)?;
            if !report.within_budget {
                return Err(format!(
                    "database size {} exceeds budget {}",
                    report.database_bytes, report.budget_bytes
                )
                .into());
            }
        }
        "fixtures" => {
            let output = PathBuf::from(args.next().ok_or_else(usage)?);
            let run_id = args.next().ok_or_else(usage)?;
            ensure_no_extra_args(args)?;
            validate_run_id(&run_id)?;
            let report = generate_fixtures(profile, &output, &run_id)?;
            print_json(&FixtureReport::from(&report))?;
        }
        _ => return Err(usage()),
    }
    Ok(())
}

fn usage() -> DynError {
    "usage: rux-performance <migrate|seed|size> <smoke|launch> | fixtures <smoke|launch> <output-directory> <run-id>".into()
}

fn ensure_no_extra_args(mut args: impl Iterator<Item = String>) -> Result<(), DynError> {
    if args.next().is_some() {
        return Err(usage());
    }
    Ok(())
}

fn validate_run_id(value: &str) -> Result<(), DynError> {
    if value.is_empty()
        || value.len() > 24
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("run-id must contain 1-24 ASCII alphanumeric or hyphen characters".into());
    }
    Ok(())
}

async fn database_pool() -> Result<PgPool, DynError> {
    let url = env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL must identify a dedicated *_performance database")?;
    Ok(PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?)
}

async fn assert_performance_database(pool: &PgPool) -> Result<String, DynError> {
    let database = assert_database_name(pool).await?;
    let migrated =
        query_scalar::<_, bool>("SELECT to_regclass('public.package_versions') IS NOT NULL")
            .fetch_one(pool)
            .await?;
    if !migrated {
        return Err("performance database has not been migrated".into());
    }
    Ok(database)
}

async fn assert_database_name(pool: &PgPool) -> Result<String, DynError> {
    let database = query_scalar::<_, String>("SELECT current_database()")
        .fetch_one(pool)
        .await?;
    if !database.ends_with("_performance") {
        return Err(format!(
            "refusing database {database:?}; performance databases must end in _performance"
        )
        .into());
    }
    Ok(database)
}

async fn seed(pool: &PgPool, profile: Profile) -> Result<SeedReport, DynError> {
    let database = assert_performance_database(pool).await?;
    seed_contents(pool, profile, database).await
}

async fn seed_contents(
    pool: &PgPool,
    profile: Profile,
    database: String,
) -> Result<SeedReport, DynError> {
    let scale = profile.scale();
    scale.validate()?;
    let occupied = query_scalar::<_, i64>(
        "SELECT (SELECT count(*) FROM namespaces) + (SELECT count(*) FROM package_versions)",
    )
    .fetch_one(pool)
    .await?;
    if occupied != 0 {
        return Err("performance seed requires empty namespace and version tables".into());
    }

    query("SET synchronous_commit = off").execute(pool).await?;
    let principal = seed_principal(pool).await?;
    seed_namespaces(pool, scale, principal.publisher_id).await?;
    seed_packages(pool, scale, principal.publisher_id).await?;
    seed_versions_and_children(pool, scale, principal.publisher_id).await?;
    seed_history(pool, scale, principal).await?;
    query("VACUUM (ANALYZE)").execute(pool).await?;

    Ok(SeedReport {
        profile: profile.name(),
        database,
        scale,
        package_versions: table_count(pool, "package_versions").await?,
        authors: table_count(pool, "package_version_authors").await?,
        keywords: table_count(pool, "package_version_keywords").await?,
        dependencies: table_count(pool, "dependencies").await?,
        audit_records: table_count(pool, "audit_records").await?,
        download_events: table_count(pool, "download_events").await?,
    })
}

#[derive(Clone, Copy)]
struct SeedPrincipal {
    publisher_id: Uuid,
    token_id: Uuid,
}

async fn seed_principal(pool: &PgPool) -> Result<SeedPrincipal, DynError> {
    let publisher_id = query_scalar::<_, Uuid>(
        "INSERT INTO users (github_user_id, github_login, display_name)
         VALUES (9000000000000000000, 'registry-performance', 'Registry Performance')
         RETURNING id",
    )
    .fetch_one(pool)
    .await?;
    let namespace_id = query_scalar::<_, Uuid>(
        "INSERT INTO namespaces (display_name, created_by_user_id)
         VALUES ($1, $2) RETURNING id",
    )
    .bind(PUBLICATION_NAMESPACE)
    .bind(publisher_id)
    .fetch_one(pool)
    .await?;
    query(
        "INSERT INTO namespace_owners (namespace_id, user_id, role, added_by_user_id)
         VALUES ($1, $2, 'owner', $2)",
    )
    .bind(namespace_id)
    .bind(publisher_id)
    .execute(pool)
    .await?;

    let token_hash: [u8; 32] = Sha256::digest(TOKEN_SECRET).into();
    let token_id = query_scalar::<_, Uuid>(
        "INSERT INTO api_tokens (user_id, display_name, token_prefix, secret_hash)
         VALUES ($1, 'Performance publication', 'rux_pat_KioqKioq', $2)
         RETURNING id",
    )
    .bind(publisher_id)
    .bind(token_hash.as_slice())
    .fetch_one(pool)
    .await?;
    query("INSERT INTO api_token_scopes (api_token_id, scope) VALUES ($1, 'publish')")
        .bind(token_id)
        .execute(pool)
        .await?;

    Ok(SeedPrincipal {
        publisher_id,
        token_id,
    })
}

async fn seed_namespaces(pool: &PgPool, scale: Scale, publisher_id: Uuid) -> Result<(), DynError> {
    query(
        "INSERT INTO namespaces (display_name, created_by_user_id, created_at, updated_at)
         SELECT 'N' || lpad(sequence::text, 6, '0'), $2,
                now() - interval '2 years' + sequence * interval '1 minute',
                now() - interval '2 years' + sequence * interval '1 minute'
         FROM generate_series(1, $1::bigint - 1) AS sequence",
    )
    .bind(scale.namespaces)
    .bind(publisher_id)
    .execute(pool)
    .await?;

    Ok(())
}

async fn seed_packages(pool: &PgPool, scale: Scale, publisher_id: Uuid) -> Result<(), DynError> {
    query(
        "WITH catalog_namespaces AS (
             SELECT id, row_number() OVER (ORDER BY id) AS ordinal
             FROM namespaces WHERE display_name <> $3
         )
         INSERT INTO packages (namespace_id, display_name, created_by_user_id, created_at)
         SELECT n.id, 'P' || lpad(sequence::text, 7, '0'), $2,
                now() - interval '2 years' + sequence * interval '5 minutes'
         FROM generate_series(1, $1::bigint) AS sequence
         JOIN catalog_namespaces n
           ON n.ordinal = 1 + ((sequence - 1) % ($4::bigint - 1))",
    )
    .bind(scale.packages)
    .bind(publisher_id)
    .bind(PUBLICATION_NAMESPACE)
    .bind(scale.namespaces)
    .execute(pool)
    .await?;

    Ok(())
}

async fn seed_versions_and_children(
    pool: &PgPool,
    scale: Scale,
    publisher_id: Uuid,
) -> Result<(), DynError> {
    query(
        "INSERT INTO package_versions (
             package_id, version, major, minor, patch, prerelease,
             manifest_schema_version, min_rux, package_type, description,
             repository_url, readme_path, readme_text, license_expression,
             normalized_manifest, artifact_sha256, artifact_size, storage_key,
             artifact_file_count, artifact_expanded_bytes, source_file_count,
             source_line_count, published_by_user_id, published_at, yanked_at,
             yanked_by_user_id
         )
         SELECT p.id,
                version_no::text || '.0.0' || CASE WHEN version_no = $1 THEN '-beta.1' ELSE '' END,
                version_no, 0, 0, CASE WHEN version_no = $1 THEN 'beta.1' ELSE NULL END,
                1, '0.4.0', CASE p.ordinal % 3 WHEN 0 THEN 'library' WHEN 1 THEN 'source' ELSE 'program' END,
                CASE WHEN p.ordinal % 20 = 0 THEN 'Needle observability registry search package'
                     WHEN p.ordinal % 7 = 0 THEN 'Streaming serialization package'
                     ELSE 'Representative registry package metadata' END,
                'https://example.invalid/performance/' || p.ordinal,
                'README.md',
                repeat(CASE WHEN p.ordinal % 20 = 0 THEN 'needle search benchmark ' ELSE 'registry package benchmark ' END, 180),
                'MIT',
                jsonb_build_object(
                    'manifest', jsonb_build_object('version', 1, 'min_rux', '0.4.0'),
                    'package', jsonb_build_object('namespace', n.display_name, 'name', p.display_name, 'version', version_no::text || '.0.0')
                ),
                decode(md5(p.ordinal::text || ':' || version_no::text) || md5(version_no::text || ':' || p.ordinal::text), 'hex'),
                1048576,
                'performance/' || n.normalized_name || '/' || p.normalized_name || '/' || version_no || '.ruxpkg',
                4, 4194304, 2, 4000, $2,
                now() - interval '2 years' + ((p.ordinal * $1 + version_no) % 1051200) * interval '1 minute',
                CASE WHEN (p.ordinal * $1 + version_no) % 20 = 0 THEN now() ELSE NULL END,
                CASE WHEN (p.ordinal * $1 + version_no) % 20 = 0 THEN $2 ELSE NULL END
         FROM (
             SELECT id, namespace_id, display_name, normalized_name,
                    substring(display_name FROM 2)::bigint AS ordinal
             FROM packages
         ) p
         JOIN namespaces n ON n.id = p.namespace_id
         CROSS JOIN generate_series(1, $1::bigint) AS version_no",
    )
    .bind(scale.versions_per_package)
    .bind(publisher_id)
    .execute(pool)
    .await?;

    query(
        "INSERT INTO package_version_authors (package_version_id, ordinal, author)
         SELECT id, ordinal, CASE ordinal WHEN 0 THEN 'Rux Contributors' ELSE 'Performance Author' END
         FROM package_versions CROSS JOIN generate_series(0, 1) AS ordinal",
    )
    .execute(pool)
    .await?;
    query(
        "INSERT INTO package_version_keywords (package_version_id, ordinal, display_name)
         SELECT v.id, ordinal,
                CASE ordinal WHEN 0 THEN 'Registry' WHEN 1 THEN 'Serialization'
                     ELSE 'Topic-' || (v.sequence % 100)::text END
         FROM (
             SELECT v.id,
                    (substring(p.display_name FROM 2)::bigint - 1) * $1 + v.major::bigint
                        AS sequence
             FROM package_versions v
             JOIN packages p ON p.id = v.package_id
         ) v
         CROSS JOIN generate_series(0, 2) AS ordinal",
    )
    .bind(scale.versions_per_package)
    .execute(pool)
    .await?;
    query(
        "INSERT INTO dependencies (
             package_version_id, display_alias, target_namespace_display_name,
             target_package_display_name, version_range
         )
         SELECT v.id, 'Dep-' || ordinal::text,
                'N' || lpad((1 + (v.sequence + ordinal) % ($1::bigint - 1))::text, 6, '0'),
                'P' || lpad((1 + (v.sequence + ordinal) % $2::bigint)::text, 7, '0'), '^1.0'
         FROM (
             SELECT v.id,
                    (substring(p.display_name FROM 2)::bigint - 1) * $3 + v.major::bigint
                        AS sequence
             FROM package_versions v
             JOIN packages p ON p.id = v.package_id
         ) v
         CROSS JOIN generate_series(0, 2) AS ordinal",
    )
    .bind(scale.namespaces)
    .bind(scale.packages)
    .bind(scale.versions_per_package)
    .execute(pool)
    .await?;

    Ok(())
}

async fn seed_history(
    pool: &PgPool,
    scale: Scale,
    principal: SeedPrincipal,
) -> Result<(), DynError> {
    query(
        "INSERT INTO audit_records (
             actor_user_id, actor_token_id, action, subject_type, subject_key,
             metadata, occurred_at
         )
         SELECT $1, $2, 'package_version_published', 'package_version',
                p.normalized_name || '@' || v.version,
                jsonb_build_object('namespace', n.normalized_name, 'package', p.normalized_name, 'version', v.version),
                v.published_at
         FROM package_versions v
         JOIN packages p ON p.id = v.package_id
         JOIN namespaces n ON n.id = p.namespace_id",
    )
    .bind(principal.publisher_id)
    .bind(principal.token_id)
    .execute(pool)
    .await?;
    query(
        "INSERT INTO download_events (package_version_id, occurred_at)
         SELECT versions.id,
                now() - (sequence % 525600) * interval '1 minute'
         FROM generate_series(1, $1::bigint) AS sequence
         JOIN (
             SELECT id, row_number() OVER (ORDER BY id) AS ordinal
             FROM package_versions
         ) AS versions
           ON versions.ordinal = 1 + ((sequence - 1) % $2::bigint)",
    )
    .bind(scale.downloads)
    .bind(scale.versions()?)
    .execute(pool)
    .await?;

    Ok(())
}

async fn table_count(pool: &PgPool, table: &str) -> Result<i64, DynError> {
    let allowed = [
        "package_versions",
        "package_version_authors",
        "package_version_keywords",
        "dependencies",
        "audit_records",
        "download_events",
    ];
    if !allowed.contains(&table) {
        return Err("unsupported performance table count".into());
    }
    Ok(
        query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
            .fetch_one(pool)
            .await?,
    )
}

async fn database_size(pool: &PgPool, profile: Profile) -> Result<SizeReport, DynError> {
    let database = assert_performance_database(pool).await?;
    let bytes = query_scalar::<_, i64>("SELECT pg_database_size(current_database())")
        .fetch_one(pool)
        .await?;
    let database_bytes = u64::try_from(bytes).map_err(|_| "database size was negative")?;
    let budget_bytes = profile.database_budget();
    Ok(SizeReport {
        profile: profile.name(),
        database,
        database_bytes,
        budget_bytes,
        within_budget: database_bytes <= budget_bytes,
    })
}

fn generate_fixtures(
    profile: Profile,
    output: &Path,
    run_id: &str,
) -> Result<FixtureManifest, DynError> {
    fs::create_dir_all(output)?;
    let (typical_count, large_count) = profile.fixture_counts();
    let typical = fixture_group(output, run_id, "typical", typical_count, 1024 * 1024)?;
    let large = fixture_group(
        output,
        run_id,
        "large",
        large_count,
        usize::try_from(ARTIFACT_MAX_BYTES)? - 8192,
    )?;
    let manifest = FixtureManifest {
        profile: profile.name(),
        run_id: run_id.to_owned(),
        bearer_token: benchmark_token(),
        namespace: PUBLICATION_NAMESPACE,
        package: PUBLICATION_PACKAGE,
        typical,
        large,
    };
    let path = output.join("fixtures.json");
    fs::write(path, serde_json::to_vec_pretty(&manifest)?)?;
    Ok(manifest)
}

fn fixture_group(
    output: &Path,
    run_id: &str,
    kind: &str,
    count: usize,
    target_size: usize,
) -> Result<Vec<PublicationFixture>, DynError> {
    (0..count)
        .map(|index| {
            let version = format!("1.0.{index}+{kind}-{run_id}");
            let manifest = publication_manifest(&version);
            let archive = build_archive(&manifest, target_size)?;
            inspect_artifact(Cursor::new(&archive), manifest.as_bytes())?;
            let stem = format!("{kind}-{index:02}");
            let manifest_name = format!("{stem}.toml");
            let package_name = format!("{stem}.ruxpkg");
            fs::write(output.join(&manifest_name), manifest)?;
            fs::write(output.join(&package_name), &archive)?;
            Ok(PublicationFixture {
                version,
                manifest_path: manifest_name,
                package_path: package_name,
                package_bytes: u64::try_from(archive.len())?,
            })
        })
        .collect()
}

fn publication_manifest(version: &str) -> String {
    format!(
        "[Manifest]\nVersion = 1\nMinRux = \"0.4.0\"\n\n[Package]\nNamespace = \"{PUBLICATION_NAMESPACE}\"\nName = \"{PUBLICATION_PACKAGE}\"\nVersion = \"{version}\"\nType = \"Source\"\nAuthors = [\"Rux Performance\"]\nKeywords = [\"Registry\", \"Performance\"]\nLicense = \"MIT\"\n"
    )
}

fn build_archive(manifest: &str, target_size: usize) -> Result<Vec<u8>, DynError> {
    let payload_size = target_size.saturating_sub(4096);
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    writer.start_file("Rux.toml", options)?;
    writer.write_all(manifest.as_bytes())?;

    let mut remaining = payload_size;
    let mut file_index = 0;
    while remaining > 0 {
        let size = remaining.min(2 * 1024 * 1024);
        writer.start_file(format!("Src/Fixture{file_index}.rux"), options)?;
        write_source_payload(&mut writer, size)?;
        remaining -= size;
        file_index += 1;
    }
    let bytes = writer.finish()?.into_inner();
    if u64::try_from(bytes.len())? > ARTIFACT_MAX_BYTES {
        return Err("generated artifact exceeded the publication limit".into());
    }
    Ok(bytes)
}

fn write_source_payload(writer: &mut impl Write, bytes: usize) -> Result<(), DynError> {
    const LINE: &[u8] = b"let benchmark_value = 1234567890\n";
    let mut remaining = bytes;
    while remaining > 0 {
        let chunk = remaining.min(LINE.len());
        writer.write_all(&LINE[..chunk])?;
        remaining -= chunk;
    }
    Ok(())
}

fn benchmark_token() -> String {
    format!("rux_pat_{}", URL_SAFE_NO_PAD.encode(TOKEN_SECRET))
}

fn print_json(value: &impl Serialize) -> Result<(), DynError> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_have_the_contract_scale() {
        assert_eq!(Profile::Launch.scale().versions().unwrap(), 100_000);
        assert_eq!(Profile::Smoke.scale().versions().unwrap(), 1_000);
        assert_eq!(Profile::Launch.database_budget(), DATABASE_BUDGET_BYTES);
    }

    #[test]
    fn run_ids_are_safe_for_versions_and_paths() {
        assert!(validate_run_id("release-20260803").is_ok());
        assert!(validate_run_id("../escape").is_err());
        assert!(validate_run_id("").is_err());
    }

    #[test]
    fn generated_fixtures_pass_the_real_inspector() {
        let directory = tempfile::tempdir().unwrap();
        let report = generate_fixtures(Profile::Smoke, directory.path(), "test-run").unwrap();
        assert_eq!(report.typical.len(), 2);
        assert_eq!(report.large.len(), 1);
        assert!(report.typical[0].package_bytes > 1_000_000);
        assert!(report.large[0].package_bytes > 5_000_000);
        assert!(report.large[0].package_bytes <= ARTIFACT_MAX_BYTES);
    }

    #[sqlx::test(migrations = "../../migrations")]
    async fn smoke_seed_populates_the_contract_shape(pool: PgPool) {
        let report = seed_contents(&pool, Profile::Smoke, "test_performance".into())
            .await
            .unwrap();
        assert_eq!(report.scale.namespaces, 10);
        assert_eq!(report.package_versions, 1_000);
        assert_eq!(report.authors, 2_000);
        assert_eq!(report.keywords, 3_000);
        assert_eq!(report.dependencies, 3_000);
        assert_eq!(report.audit_records, 1_000);
        assert_eq!(report.download_events, 10_000);
    }
}
