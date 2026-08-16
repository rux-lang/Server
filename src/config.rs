//! The configuration file both binaries read, and the validated values the API
//! runs on.
//!
//! Two layers. [`ConfigFile`] and its sections are the wire format: plain
//! serde structs that mirror `config.toml` key for key, reject unknown keys,
//! and apply defaults. Everything below `ApiConfig` is the validated form the
//! process actually runs on, reached through [`ApiConfig::load`].
//!
//! The separation exists because the invariants that matter here are
//! cross-field — a burst that cannot exceed its own rate, a playground
//! deadline that cannot be shorter than the ordinary request deadline — and
//! those cannot be expressed in a schema. Deserialization proves the shape;
//! the conversion proves the meaning.
//!
//! `rux-playgroundd` deserializes the same [`ConfigFile`], so a key belongs to
//! whichever process its path names: `[playground.api]` is the registry's,
//! `[playground.broker]` is the daemon's, and `playground.socket` is the one
//! value both read.

use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use ipnet::IpNet;
use rux_application::OrphanCleanupPolicy;
use rux_infrastructure::{GitHubOAuthConfig, InfrastructureConfig};
use serde::Deserialize;
use serde::de::{self, Deserializer, Visitor};
use time::Duration;
use url::{Host, Url};

use crate::abuse::{AbuseConfig, RateLimit};
use crate::upload::DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES;

/// Where both binaries look when `--config` is not given.
pub const DEFAULT_CONFIG_PATH: &str = "config/config.toml";

/// The only accepted command-line option.
const CONFIG_OPTION: &str = "--config";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A configuration failure, reported before logging is initialised.
///
/// `Debug` forwards to `Display` deliberately: both binaries return this out of
/// `main`, and the runtime prints the debug form, so an operator would
/// otherwise see a struct dump instead of the message naming their mistake.
pub enum ConfigError {
    /// The command line was wrong, so no file was opened.
    Arguments(String),
    /// The file could not be read, parsed, or satisfied.
    Invalid { path: PathBuf, detail: String },
}

impl ConfigError {
    /// Reports a problem with the contents of a configuration file.
    pub fn invalid(path: &Path, detail: impl Into<String>) -> Self {
        Self::Invalid {
            path: path.to_path_buf(),
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arguments(detail) => formatter.write_str(detail),
            Self::Invalid { path, detail } => write!(formatter, "{}: {detail}", path.display()),
        }
    }
}

impl fmt::Debug for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for ConfigError {}

// ---------------------------------------------------------------------------
// Locating and reading the file
// ---------------------------------------------------------------------------

/// Resolves the configuration path from command-line arguments.
///
/// Takes the arguments rather than reading them so the rules are testable
/// without touching process state. An unrecognised argument is refused rather
/// than ignored, for the same reason unknown keys are: a mistake an operator
/// cannot see is worse than a startup that fails.
///
/// # Errors
///
/// Returns [`ConfigError::Arguments`] if `--config` appears twice, appears
/// without a value, or any other argument is present.
pub fn config_path<I>(arguments: I) -> Result<PathBuf, ConfigError>
where
    I: IntoIterator<Item = String>,
{
    let mut arguments = arguments.into_iter().skip(1);
    let mut selected: Option<String> = None;

    while let Some(argument) = arguments.next() {
        let value = if argument == CONFIG_OPTION {
            arguments
                .next()
                .ok_or_else(|| ConfigError::Arguments(format!("{CONFIG_OPTION} needs a path")))?
        } else if let Some(value) = argument.strip_prefix("--config=") {
            value.to_owned()
        } else {
            return Err(ConfigError::Arguments(format!(
                "unexpected argument {argument:?}; the only option is {CONFIG_OPTION} <path>"
            )));
        };

        if value.trim().is_empty() {
            return Err(ConfigError::Arguments(format!(
                "{CONFIG_OPTION} needs a path"
            )));
        }
        if selected.is_some() {
            return Err(ConfigError::Arguments(format!(
                "{CONFIG_OPTION} may be given only once"
            )));
        }
        selected = Some(value);
    }

    Ok(selected.map_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH), PathBuf::from))
}

/// Reads and deserializes a configuration file.
///
/// # Errors
///
/// Returns [`ConfigError::Invalid`] if the file is missing, unreadable, not
/// valid TOML, or carries a key the schema does not define.
pub fn load(path: &Path) -> Result<ConfigFile, ConfigError> {
    let text = fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ConfigError::invalid(
                path,
                format!("no such file (pass {CONFIG_OPTION} <path> to name another)"),
            )
        } else {
            ConfigError::invalid(path, error.to_string())
        }
    })?;

    toml_edit::de::from_str::<ConfigFile>(&text)
        .map_err(|error| ConfigError::invalid(path, locate(&text, &error)))
}

/// Renders a deserialization failure as `line:column message`.
///
/// Deliberately does not use `toml_edit`'s own input-annotating renderer: that
/// prints the offending source line, and the offending line may be a
/// credential. The span gives the operator the location; serde's message names
/// the key.
fn locate(text: &str, error: &toml_edit::de::Error) -> String {
    let Some(span) = error.span() else {
        return error.message().to_owned();
    };

    let consumed = &text[..span.start.min(text.len())];
    // Count newlines rather than lines: `lines()` drops the empty final line,
    // so a key at the start of line two would be reported as line one.
    let line = consumed.matches('\n').count() + 1;
    let column = consumed
        .rfind('\n')
        .map_or(consumed.len(), |newline| consumed.len() - newline - 1)
        + 1;

    format!("{line}:{column} {}", error.message())
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

/// A configured credential.
///
/// `Debug` prints a placeholder and the deserializer refuses non-string values
/// without quoting them, because serde's own type-mismatch message embeds the
/// offending value. Both binaries read this file, so a mistyped credential
/// must not reach either process's stderr.
#[derive(Clone, Default)]
pub struct Secret(String);

impl Secret {
    /// Borrows the credential.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Yields the credential to the adapter that needs it.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SecretVisitor;

        // Every method here exists so serde's default `invalid_type` message,
        // which embeds the value, is never the one reported.
        impl<'v> Visitor<'v> for SecretVisitor {
            type Value = Secret;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a quoted string")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(Secret(value.to_owned()))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Err(E::custom("must be a quoted string"))
            }

            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Err(E::custom("must be a quoted string"))
            }

            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Err(E::custom("must be a quoted string"))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Err(E::custom("must be a quoted string"))
            }

            fn visit_seq<A: de::SeqAccess<'v>>(self, _: A) -> Result<Self::Value, A::Error> {
                Err(de::Error::custom("must be a quoted string"))
            }

            fn visit_map<A: de::MapAccess<'v>>(self, _: A) -> Result<Self::Value, A::Error> {
                Err(de::Error::custom("must be a quoted string"))
            }
        }

        deserializer.deserialize_str(SecretVisitor)
    }
}

// ---------------------------------------------------------------------------
// The wire format
// ---------------------------------------------------------------------------

/// `config.toml`, deserialized but not yet validated.
///
/// Every section defaults, so a file may omit any section it does not change.
/// Values that have no sensible default are `Option` here and are required
/// during conversion, which reports them by their dotted key path — serde's
/// own "missing field" message names the field without its section.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigFile {
    pub server: ServerSection,
    pub database: DatabaseSection,
    pub storage: StorageSection,
    pub packages: PackagesSection,
    pub web: WebSection,
    pub github: GitHubSection,
    pub uploads: UploadsSection,
    pub orphan_cleanup: OrphanCleanupSection,
    pub abuse: AbuseSection,
    pub playground: PlaygroundSection,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSection {
    pub bind_address: SocketAddr,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            bind_address: SocketAddr::from(([127, 0, 0, 1], 8080)),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseSection {
    pub url: Option<Secret>,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageSection {
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    pub access_key: Option<Secret>,
    pub secret_key: Option<Secret>,
    pub region: String,
    pub force_path_style: bool,
}

impl Default for StorageSection {
    fn default() -> Self {
        Self {
            endpoint: None,
            bucket: None,
            access_key: None,
            secret_key: None,
            region: "us-east-1".to_owned(),
            force_path_style: false,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PackagesSection {
    pub cdn_base_url: String,
}

impl Default for PackagesSection {
    fn default() -> Self {
        Self {
            cdn_base_url: "http://localhost:9000/packages/".to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebSection {
    pub allowed_origin: String,
    pub callback_url: String,
}

impl Default for WebSection {
    fn default() -> Self {
        Self {
            allowed_origin: "http://localhost:3000".to_owned(),
            callback_url: "http://localhost:3000/packages/-/auth/callback".to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GitHubSection {
    pub client_id: Option<String>,
    pub client_secret: Option<Secret>,
    pub callback_url: String,
}

impl Default for GitHubSection {
    fn default() -> Self {
        Self {
            client_id: None,
            client_secret: None,
            callback_url: "http://localhost:8080/v1/auth/github/callback".to_owned(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UploadsSection {
    /// Omitted selects the operating-system temporary directory; present and
    /// empty is a mistake rather than a second way of saying the same thing.
    pub temporary_directory: Option<PathBuf>,
    pub temporary_capacity_bytes: u64,
    pub max_concurrency: u64,
}

impl Default for UploadsSection {
    fn default() -> Self {
        Self {
            temporary_directory: None,
            temporary_capacity_bytes: DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES,
            max_concurrency: 8,
        }
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct OrphanCleanupSection {
    pub interval_seconds: u64,
    pub minimum_age_seconds: i64,
    pub scan_limit: u16,
    pub delete_limit: u16,
}

impl Default for OrphanCleanupSection {
    fn default() -> Self {
        Self {
            interval_seconds: 3_600,
            minimum_age_seconds: 86_400,
            scan_limit: 1_000,
            delete_limit: 100,
        }
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AbuseSection {
    /// An empty list disables proxy trust entirely, which is why this cannot
    /// simply take serde's `Vec` default.
    pub trusted_proxy_cidrs: Vec<String>,
    pub request_timeout_seconds: u64,
    pub publication_timeout_seconds: u64,
    pub rate_limit: RateLimitSection,
}

impl Default for AbuseSection {
    fn default() -> Self {
        Self {
            trusted_proxy_cidrs: vec!["127.0.0.0/8".to_owned(), "::1/128".to_owned()],
            request_timeout_seconds: 30,
            publication_timeout_seconds: 120,
            rate_limit: RateLimitSection::default(),
        }
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RateLimitSection {
    pub read: RateLimitTier,
    pub security: RateLimitTier,
    pub mutation: RateLimitTier,
    pub publication: RateLimitTier,
    pub playground: RateLimitTier,
}

impl Default for RateLimitSection {
    fn default() -> Self {
        Self {
            read: RateLimitTier::new(120, 60),
            security: RateLimitTier::new(30, 10),
            mutation: RateLimitTier::new(60, 20),
            publication: RateLimitTier::new(6, 2),
            playground: RateLimitTier::new(10, 4),
        }
    }
}

/// One abuse tier's budget.
///
/// Both fields are required when a tier is named at all: the two are coupled
/// by `burst <= per_minute`, and a tier's default burst means nothing once its
/// rate has been changed.
#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RateLimitTier {
    pub per_minute: u64,
    pub burst: u64,
}

impl RateLimitTier {
    const fn new(per_minute: u64, burst: u64) -> Self {
        Self { per_minute, burst }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlaygroundSection {
    /// The one key both processes read: the broker binds it, the API connects
    /// to it. A single value is the point of a single file.
    pub socket: String,
    pub api: PlaygroundApiSection,
    /// Absent unless this host runs the broker; the API never reads it.
    pub broker: Option<BrokerSection>,
}

impl Default for PlaygroundSection {
    fn default() -> Self {
        Self {
            socket: "/run/rux-playground/run.sock".to_owned(),
            api: PlaygroundApiSection::default(),
            broker: None,
        }
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PlaygroundApiSection {
    pub enabled: bool,
    pub timeout_seconds: u64,
    /// Concurrent runs the API will admit, which is not the number of
    /// containers the broker will run — see `playground.broker`.
    pub max_concurrency: u64,
}

impl Default for PlaygroundApiSection {
    fn default() -> Self {
        Self {
            enabled: false,
            timeout_seconds: 30,
            max_concurrency: 2,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrokerSection {
    pub image: Option<String>,
    pub jobs_root: PathBuf,
    pub docker_binary: PathBuf,
    /// `Root:Namespace` imports already seeded into the image.
    pub packages: Vec<String>,
    /// Containers the broker will run at once.
    pub max_concurrency: u64,
    pub request_timeout_seconds: u64,
    pub limits: BrokerLimitsSection,
}

impl Default for BrokerSection {
    fn default() -> Self {
        Self {
            image: None,
            jobs_root: PathBuf::from(rux_sandbox::DEFAULT_JOBS_ROOT),
            docker_binary: PathBuf::from(rux_sandbox::DEFAULT_DOCKER_BINARY),
            packages: Vec::new(),
            max_concurrency: 2,
            request_timeout_seconds: 30,
            limits: BrokerLimitsSection::default(),
        }
    }
}

/// The four sandbox limits an operator may move.
///
/// Deliberately not `rux_sandbox::SandboxLimits`, which carries ten fields and
/// is the protocol's wire type: reusing it would silently make the source,
/// stdin, output, pid, tmpfs, and startup-grace bounds configurable.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrokerLimitsSection {
    pub memory_bytes: u64,
    pub cpu_millis: u64,
    pub compile_timeout_seconds: u64,
    pub run_timeout_seconds: u64,
}

impl Default for BrokerLimitsSection {
    fn default() -> Self {
        let defaults = rux_sandbox::SandboxLimits::default();
        Self {
            memory_bytes: defaults.memory_bytes,
            cpu_millis: defaults.cpu_millis.into(),
            compile_timeout_seconds: defaults.compile_timeout_seconds.into(),
            run_timeout_seconds: defaults.run_timeout_seconds.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// The validated form
// ---------------------------------------------------------------------------

pub struct ApiConfig {
    pub bind_address: SocketAddr,
    pub allowed_web_origin: String,
    pub web_callback_url: Url,
    pub github_oauth: GitHubOAuthConfig,
    pub infrastructure: InfrastructureConfig,
    pub package_cdn_base_url: Url,
    pub orphan_cleanup: OrphanCleanupConfig,
    pub abuse: AbuseConfig,
    pub uploads: UploadConfig,
    pub playground: PlaygroundConfig,
}

/// How the API reaches the playground sandbox, and whether it does at all.
///
/// The rate limit and admission slots live in [`AbuseConfig`] with the other
/// tiers; this carries what is specific to the playground itself.
#[derive(Clone, Debug)]
pub struct PlaygroundConfig {
    /// Whether the playground routes are mounted at all. When false the
    /// fallback answers, so the endpoints are a 404 rather than a 503.
    pub enabled: bool,
    /// Unix socket the sandbox daemon listens on.
    pub socket_path: PathBuf,
    /// Deadline for one exchange with the daemon.
    pub timeout: StdDuration,
    /// Concurrent runs the API will admit.
    pub max_concurrency: usize,
    /// Per-client request budget for the playground tier.
    pub rate_limit: RateLimit,
}

#[derive(Clone, Copy, Debug)]
pub struct OrphanCleanupConfig {
    pub interval: StdDuration,
    pub policy: OrphanCleanupPolicy,
}

#[derive(Clone, Debug)]
pub struct UploadConfig {
    pub temporary_directory: PathBuf,
    pub temporary_capacity_bytes: u64,
}

impl ApiConfig {
    /// Reads, deserializes, and validates the configuration at `path`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] if the file cannot be read or parsed, a
    /// required value is absent, or any bound or cross-field rule is broken.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let file = load(path)?;
        Self::from_file(&file).map_err(|detail| ConfigError::invalid(path, detail))
    }

    fn from_file(file: &ConfigFile) -> Result<Self, String> {
        let allowed_web_origin = parse_origin(&file.web.allowed_origin)?;
        let web_callback_url = parse_web_callback_url(&file.web.callback_url, &allowed_web_origin)?;
        let github_callback_url = parse_callback_url(&file.github.callback_url)?;
        // Parsed before the abuse tiers, which cross-check the playground
        // timeout against the ordinary request timeout.
        let playground = parse_playground(&file.playground, &file.abuse.rate_limit.playground)?;

        Ok(Self {
            bind_address: file.server.bind_address,
            allowed_web_origin: allowed_web_origin.origin().ascii_serialization(),
            web_callback_url,
            github_oauth: GitHubOAuthConfig {
                client_id: required_text("github.client_id", file.github.client_id.as_deref())?,
                client_secret: required_secret(
                    "github.client_secret",
                    file.github.client_secret.as_ref(),
                )?,
                callback_url: github_callback_url,
            },
            infrastructure: InfrastructureConfig {
                database_url: required_secret("database.url", file.database.url.as_ref())?,
                storage_endpoint: required_text(
                    "storage.endpoint",
                    file.storage.endpoint.as_deref(),
                )?,
                storage_bucket: required_text("storage.bucket", file.storage.bucket.as_deref())?,
                storage_access_key: required_secret(
                    "storage.access_key",
                    file.storage.access_key.as_ref(),
                )?,
                storage_secret_key: required_secret(
                    "storage.secret_key",
                    file.storage.secret_key.as_ref(),
                )?,
                storage_region: file.storage.region.clone(),
                storage_force_path_style: file.storage.force_path_style,
            },
            package_cdn_base_url: parse_package_cdn_base_url(&file.packages.cdn_base_url)?,
            orphan_cleanup: parse_orphan_cleanup(&file.orphan_cleanup)?,
            abuse: parse_abuse(&file.abuse, &file.uploads, &playground)?,
            uploads: parse_uploads(&file.uploads)?,
            playground,
        })
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn parse_abuse(
    abuse: &AbuseSection,
    uploads: &UploadsSection,
    playground: &PlaygroundConfig,
) -> Result<AbuseConfig, String> {
    let request_timeout = bounded(
        "abuse.request_timeout_seconds",
        abuse.request_timeout_seconds,
        1,
        120,
    )?;
    let publication_timeout = bounded(
        "abuse.publication_timeout_seconds",
        abuse.publication_timeout_seconds,
        request_timeout,
        600,
    )?;
    // A run compiles and executes, so it needs at least as long as an ordinary
    // request; anything shorter would cut off runs the sandbox would finish.
    if playground.timeout < StdDuration::from_secs(request_timeout) {
        return Err(
            "playground.api.timeout_seconds must be at least abuse.request_timeout_seconds"
                .to_owned(),
        );
    }

    Ok(AbuseConfig {
        trusted_proxy_cidrs: parse_cidrs(&abuse.trusted_proxy_cidrs)?,
        request_timeout: StdDuration::from_secs(request_timeout),
        publication_timeout: StdDuration::from_secs(publication_timeout),
        playground_timeout: playground.timeout,
        read: parse_rate_limit("read", &abuse.rate_limit.read)?,
        security: parse_rate_limit("security", &abuse.rate_limit.security)?,
        mutation: parse_rate_limit("mutation", &abuse.rate_limit.mutation)?,
        publication: parse_rate_limit("publication", &abuse.rate_limit.publication)?,
        playground: playground.rate_limit,
        upload_max_concurrency: usize::try_from(bounded(
            "uploads.max_concurrency",
            uploads.max_concurrency,
            1,
            64,
        )?)
        .expect("the configured upper bound fits usize"),
        playground_max_concurrency: playground.max_concurrency,
    })
}

fn parse_playground(
    playground: &PlaygroundSection,
    rate_limit: &RateLimitTier,
) -> Result<PlaygroundConfig, String> {
    let socket_path = required_path("playground.socket", &playground.socket)?;

    Ok(PlaygroundConfig {
        enabled: playground.api.enabled,
        socket_path,
        timeout: StdDuration::from_secs(bounded(
            "playground.api.timeout_seconds",
            playground.api.timeout_seconds,
            1,
            120,
        )?),
        max_concurrency: usize::try_from(bounded(
            "playground.api.max_concurrency",
            playground.api.max_concurrency,
            1,
            16,
        )?)
        .expect("the configured upper bound fits usize"),
        rate_limit: parse_rate_limit("playground", rate_limit)?,
    })
}

fn parse_cidrs(networks: &[String]) -> Result<Arc<[IpNet]>, String> {
    networks
        .iter()
        .map(|network| {
            network.trim().parse::<IpNet>().map_err(|error| {
                format!("abuse.trusted_proxy_cidrs entry {network:?} is not a network: {error}")
            })
        })
        .collect::<Result<Vec<_>, String>>()
        .map(Into::into)
}

fn parse_rate_limit(tier: &str, budget: &RateLimitTier) -> Result<RateLimit, String> {
    let per_minute = bounded(
        &format!("abuse.rate_limit.{tier}.per_minute"),
        budget.per_minute,
        1,
        10_000,
    )?;
    let burst = bounded(
        &format!("abuse.rate_limit.{tier}.burst"),
        budget.burst,
        1,
        per_minute,
    )?;
    Ok(RateLimit {
        per_minute: u32::try_from(per_minute).expect("the configured upper bound fits u32"),
        burst: u32::try_from(burst).expect("the configured upper bound fits u32"),
    })
}

fn parse_uploads(uploads: &UploadsSection) -> Result<UploadConfig, String> {
    let temporary_directory = match uploads.temporary_directory.as_deref() {
        None => std::env::temp_dir().join("rux-server-uploads"),
        Some(directory) if directory.as_os_str().is_empty() => {
            return Err(
                "uploads.temporary_directory must name a directory; omit it to use the operating-system temporary directory"
                    .to_owned(),
            );
        }
        Some(directory) => directory.to_path_buf(),
    };

    Ok(UploadConfig {
        temporary_directory,
        temporary_capacity_bytes: bounded(
            "uploads.temporary_capacity_bytes",
            uploads.temporary_capacity_bytes,
            5 * 1024 * 1024,
            10 * 1024 * 1024 * 1024,
        )?,
    })
}

fn parse_orphan_cleanup(cleanup: &OrphanCleanupSection) -> Result<OrphanCleanupConfig, String> {
    if cleanup.interval_seconds == 0 {
        return Err("orphan_cleanup.interval_seconds must be positive".to_owned());
    }
    if cleanup.minimum_age_seconds <= 0 {
        return Err("orphan_cleanup.minimum_age_seconds must be positive".to_owned());
    }

    let policy = OrphanCleanupPolicy::new(
        Duration::seconds(cleanup.minimum_age_seconds),
        cleanup.scan_limit,
        cleanup.delete_limit,
    )
    .map_err(|error| format!("orphan_cleanup is not a usable policy: {error}"))?;

    Ok(OrphanCleanupConfig {
        interval: StdDuration::from_secs(cleanup.interval_seconds),
        policy,
    })
}

/// Range-checks one setting, naming it by its dotted key path.
///
/// # Errors
///
/// Returns the operator-facing message if `value` falls outside the range.
pub fn bounded(key: &str, value: u64, minimum: u64, maximum: u64) -> Result<u64, String> {
    if !(minimum..=maximum).contains(&value) {
        return Err(format!("{key} must be between {minimum} and {maximum}"));
    }
    Ok(value)
}

/// Requires a value that has no sensible default, treating blank as absent.
///
/// # Errors
///
/// Returns the operator-facing message if the value is absent or blank.
pub fn required_text(key: &str, value: Option<&str>) -> Result<String, String> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value.to_owned()),
        _ => Err(format!("{key} is required")),
    }
}

fn required_secret(key: &str, value: Option<&Secret>) -> Result<String, String> {
    match value {
        Some(secret) if !secret.is_blank() => Ok(secret.as_str().to_owned()),
        _ => Err(format!("{key} is required")),
    }
}

fn required_path(key: &str, value: &str) -> Result<PathBuf, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{key} is required"));
    }
    Ok(PathBuf::from(trimmed))
}

fn parse_package_cdn_base_url(value: &str) -> Result<Url, String> {
    let url = parse_url("packages.cdn_base_url", value)?;
    validate_web_url(&url, "packages.cdn_base_url")?;
    if url.cannot_be_a_base()
        || !url.path().ends_with('/')
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "packages.cdn_base_url must be a hierarchical base URL ending in / without a query or fragment"
                .to_owned(),
        );
    }
    Ok(url)
}

fn parse_origin(value: &str) -> Result<Url, String> {
    let url = parse_url("web.allowed_origin", value)?;
    validate_web_url(&url, "web.allowed_origin")?;
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err("web.allowed_origin must contain only an origin".to_owned());
    }
    Ok(url)
}

fn parse_callback_url(value: &str) -> Result<Url, String> {
    let url = parse_url("github.callback_url", value)?;
    validate_web_url(&url, "github.callback_url")?;
    if url.path() != "/v1/auth/github/callback" || url.query().is_some() || url.fragment().is_some()
    {
        return Err(
            "github.callback_url must use /v1/auth/github/callback without a query or fragment"
                .to_owned(),
        );
    }
    Ok(url)
}

fn parse_web_callback_url(value: &str, allowed_web_origin: &Url) -> Result<Url, String> {
    let url = parse_url("web.callback_url", value)?;
    validate_web_url(&url, "web.callback_url")?;
    if url.origin() != allowed_web_origin.origin()
        || url.path() != "/packages/-/auth/callback"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "web.callback_url must use web.allowed_origin and /packages/-/auth/callback without a query or fragment"
                .to_owned(),
        );
    }
    Ok(url)
}

fn parse_url(key: &str, value: &str) -> Result<Url, String> {
    Url::parse(value).map_err(|error| format!("{key} is not a valid URL: {error}"))
}

fn validate_web_url(url: &Url, key: &str) -> Result<(), String> {
    if !url.username().is_empty() || url.password().is_some() || url.host().is_none() {
        return Err(format!("{key} must be an absolute URL without credentials"));
    }
    if url.scheme() == "https" || (url.scheme() == "http" && is_loopback(url)) {
        return Ok(());
    }
    Err(format!(
        "{key} must use HTTPS except for loopback development"
    ))
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed local-development configuration.
    const DEVELOPMENT: &str = include_str!("../config/config.toml");

    fn parse(document: &str) -> Result<ConfigFile, toml_edit::de::Error> {
        toml_edit::de::from_str::<ConfigFile>(document)
    }

    fn refuse(document: &str) -> String {
        let error = parse(document).expect_err("document should be refused");
        locate(document, &error)
    }

    /// The smallest document that satisfies every required value.
    fn complete() -> String {
        [
            "[database]",
            "url = \"postgres://registry:registry@localhost:5432/registry\"",
            "[storage]",
            "endpoint = \"http://localhost:9000\"",
            "bucket = \"packages\"",
            "access_key = \"registry\"",
            "secret_key = \"registry-secret\"",
            "[github]",
            "client_id = \"client\"",
            "client_secret = \"secret\"",
            "",
        ]
        .join("\n")
    }

    #[test]
    fn the_committed_configuration_is_valid_and_carries_the_schema_defaults() {
        // The committed file is the only configuration most readers ever
        // see, so a value that drifts from the code's default is a
        // documentation bug that should fail the build rather than mislead
        // someone writing the production file. Credentials and the two
        // required identifiers are excluded: they are placeholders by
        // construction.
        let file = parse(DEVELOPMENT).expect("the committed config should deserialize");
        let default = ConfigFile::default();

        assert_eq!(file.server.bind_address, default.server.bind_address);
        assert_eq!(file.storage.region, default.storage.region);
        assert_eq!(
            file.packages.cdn_base_url, default.packages.cdn_base_url,
            "packages.cdn_base_url drifted from its default"
        );
        assert_eq!(file.web.allowed_origin, default.web.allowed_origin);
        assert_eq!(file.web.callback_url, default.web.callback_url);
        assert_eq!(file.github.callback_url, default.github.callback_url);
        assert_eq!(
            file.uploads.temporary_capacity_bytes,
            default.uploads.temporary_capacity_bytes
        );
        assert_eq!(
            file.uploads.max_concurrency,
            default.uploads.max_concurrency
        );
        assert_eq!(file.orphan_cleanup, default.orphan_cleanup);
        assert_eq!(file.abuse, default.abuse);
        assert_eq!(file.playground.socket, default.playground.socket);
        assert_eq!(file.playground.api, default.playground.api);
        // storage.force_path_style is the one deliberate divergence: local
        // development runs MinIO, which needs path-style addressing, while
        // the default suits a production S3 endpoint.
        assert!(file.storage.force_path_style);
        assert!(!default.storage.force_path_style);
    }

    #[test]
    fn the_committed_configuration_runs_the_api() {
        // This is the file a fresh checkout starts on, so it must be enough to
        // boot with no arguments at all.
        let file = parse(DEVELOPMENT).expect("the committed config should deserialize");
        let config = ApiConfig::from_file(&file).expect("the committed config should be valid");

        assert_eq!(
            config.bind_address,
            SocketAddr::from(([127, 0, 0, 1], 8080))
        );
        assert_eq!(config.allowed_web_origin, "http://localhost:3000");
        assert!(!config.playground.enabled);
        assert_eq!(
            config.abuse.read,
            RateLimit {
                per_minute: 120,
                burst: 60
            }
        );
    }

    #[test]
    fn an_unknown_key_is_refused_and_named() {
        let message = refuse("[storage]\nbucket_name = \"packages\"\n");

        assert!(message.contains("bucket_name"), "message was {message:?}");
        assert!(message.starts_with("2:"), "message should locate the key");
    }

    #[test]
    fn an_unknown_section_is_refused() {
        assert!(refuse("[storrage]\n").contains("storrage"));
        assert!(refuse("[playground.brokerr]\n").contains("brokerr"));
    }

    #[test]
    fn a_mistyped_secret_is_refused_without_echoing_it() {
        // The value is the credential. Serde's own type-mismatch message
        // would quote it, and both binaries read this file.
        let message = refuse("[storage]\nsecret_key = 12345\n");

        assert!(message.contains("quoted string"), "message was {message:?}");
        assert!(!message.contains("12345"), "message leaked the value");
    }

    #[test]
    fn required_values_are_named_with_their_section() {
        // `ApiConfig` has no `Debug` on purpose - it holds credentials - so
        // these unwrap by pattern rather than through `expect_err`.
        let file = parse("").expect("an empty document is well-formed");
        let Err(message) = ApiConfig::from_file(&file) else {
            panic!("a document with no credentials should be refused");
        };
        assert_eq!(message, "github.client_id is required");

        // Blank is treated as absent rather than accepted, which the
        // environment form did not do.
        let blank = parse("[database]\nurl = \"   \"\n").expect("well-formed");
        let Err(message) = ApiConfig::from_file(&blank) else {
            panic!("a blank credential is not a credential");
        };
        assert!(message.contains("is required"), "message was {message:?}");
    }

    #[test]
    fn origins_and_callbacks_are_exact_and_secure() {
        let origin = parse_origin("https://rux-lang.dev").expect("origin should parse");
        assert!(parse_origin("http://localhost:3000").is_ok());
        assert!(parse_origin("https://rux-lang.dev/path").is_err());
        assert!(parse_origin("http://rux-lang.dev").is_err());
        assert!(
            parse_web_callback_url("https://rux-lang.dev/packages/-/auth/callback", &origin,)
                .is_ok()
        );
        assert!(
            parse_web_callback_url("https://other.example/packages/-/auth/callback", &origin)
                .is_err()
        );
        assert!(parse_web_callback_url("https://rux-lang.dev/auth/callback", &origin).is_err());
        assert!(parse_callback_url("https://api.rux-lang.dev/v1/auth/github/callback").is_ok());
        assert!(parse_callback_url("https://api.rux-lang.dev/other").is_err());
        assert!(
            parse_callback_url("https://api.rux-lang.dev/v1/auth/github/callback?next=/").is_err()
        );
    }

    #[test]
    fn package_cdn_base_urls_are_absolute_secure_and_directory_like() {
        assert!(
            parse_package_cdn_base_url("https://packages.fra1.cdn.digitaloceanspaces.com/").is_ok()
        );
        assert!(parse_package_cdn_base_url("http://localhost:9000/packages/").is_ok());
        assert!(parse_package_cdn_base_url("https://cdn.example.test/packages").is_err());
        assert!(parse_package_cdn_base_url("http://cdn.example.test/packages/").is_err());
        assert!(parse_package_cdn_base_url("https://user@cdn.example.test/").is_err());
        assert!(parse_package_cdn_base_url("https://cdn.example.test/?token=secret").is_err());
        assert!(parse_package_cdn_base_url("https://cdn.example.test/#fragment").is_err());
    }

    #[test]
    fn orphan_cleanup_defaults_are_bounded_and_positive() {
        let config = parse_orphan_cleanup(&OrphanCleanupSection::default())
            .expect("default cleanup config should parse");

        assert_eq!(config.interval, StdDuration::from_secs(3_600));
        assert_eq!(config.policy.minimum_age(), Duration::hours(24));
        assert_eq!(config.policy.scan_limit(), 1_000);
        assert_eq!(config.policy.delete_limit(), 100);
    }

    #[test]
    fn orphan_cleanup_rejects_invalid_limits_and_durations() {
        let cleanup = |interval, minimum_age, scan_limit, delete_limit| {
            parse_orphan_cleanup(&OrphanCleanupSection {
                interval_seconds: interval,
                minimum_age_seconds: minimum_age,
                scan_limit,
                delete_limit,
            })
        };

        assert!(cleanup(0, 86_400, 1_000, 100).is_err());
        assert!(cleanup(3_600, 0, 1_000, 100).is_err());
        assert!(cleanup(3_600, 86_400, 1_001, 100).is_err());
        assert!(cleanup(3_600, 86_400, 100, 101).is_err());
    }

    #[test]
    fn abuse_defaults_are_tiered_and_trust_only_loopback_proxies() {
        let config = parse_abuse(
            &AbuseSection::default(),
            &UploadsSection::default(),
            &playground_defaults(),
        )
        .expect("default abuse controls should parse");

        assert_eq!(config.trusted_proxy_cidrs.len(), 2);
        assert_eq!(config.request_timeout, StdDuration::from_secs(30));
        assert_eq!(config.publication_timeout, StdDuration::from_secs(120));
        assert_eq!(
            config.read,
            RateLimit {
                per_minute: 120,
                burst: 60
            }
        );
        assert_eq!(
            config.publication,
            RateLimit {
                per_minute: 6,
                burst: 2
            }
        );
        assert_eq!(config.upload_max_concurrency, 8);
        assert_eq!(config.playground_timeout, StdDuration::from_secs(30));
        assert_eq!(config.playground_max_concurrency, 2);
        assert_eq!(
            config.playground,
            RateLimit {
                per_minute: 10,
                burst: 4
            }
        );
    }

    fn playground_defaults() -> PlaygroundConfig {
        let enabled = PlaygroundSection {
            api: PlaygroundApiSection {
                enabled: true,
                ..PlaygroundApiSection::default()
            },
            ..PlaygroundSection::default()
        };
        parse_playground(&enabled, &RateLimitSection::default().playground)
            .expect("playground defaults should parse")
    }

    #[test]
    fn the_playground_is_off_by_default() {
        // The registry must work on a host without Docker, so the routes are
        // not mounted unless someone asks for them.
        assert!(!PlaygroundApiSection::default().enabled);
        assert!(!ConfigFile::default().playground.api.enabled);
    }

    #[test]
    fn playground_defaults_are_conservatively_bounded() {
        let config = playground_defaults();

        assert_eq!(
            config.socket_path,
            PathBuf::from("/run/rux-playground/run.sock")
        );
        assert_eq!(config.timeout, StdDuration::from_secs(30));
        assert_eq!(config.max_concurrency, 2);
        assert_eq!(
            config.rate_limit,
            RateLimit {
                per_minute: 10,
                burst: 4
            }
        );
    }

    #[test]
    fn every_playground_knob_is_bounded_rather_than_trusted() {
        let parse_with = |socket: &str, timeout: u64, concurrency: u64, per_minute, burst| {
            parse_playground(
                &PlaygroundSection {
                    socket: socket.to_owned(),
                    api: PlaygroundApiSection {
                        enabled: true,
                        timeout_seconds: timeout,
                        max_concurrency: concurrency,
                    },
                    broker: None,
                },
                &RateLimitTier::new(per_minute, burst),
            )
        };

        assert!(parse_with("/s.sock", 30, 2, 10, 4).is_ok());
        // An empty socket path would silently bind nothing.
        assert!(parse_with("   ", 30, 2, 10, 4).is_err());
        // Timeout, admission slots, and the rate limit all have ceilings.
        assert!(parse_with("/s.sock", 0, 2, 10, 4).is_err());
        assert!(parse_with("/s.sock", 121, 2, 10, 4).is_err());
        assert!(parse_with("/s.sock", 30, 0, 10, 4).is_err());
        assert!(parse_with("/s.sock", 30, 17, 10, 4).is_err());
        assert!(parse_with("/s.sock", 30, 2, 0, 4).is_err());
        // A burst larger than the per-minute budget is nonsense.
        assert!(parse_with("/s.sock", 30, 2, 10, 11).is_err());
    }

    #[test]
    fn a_boolean_knob_must_be_a_boolean() {
        // The environment form had to check this by hand; the format now
        // refuses it before any of our code runs, with a location.
        let message = refuse("[playground.api]\nenabled = \"yes\"\n");
        assert!(message.starts_with("2:"), "message was {message:?}");
        assert!(refuse("[playground.api]\nenabled = 1\n").starts_with("2:"));
    }

    #[test]
    fn a_playground_timeout_below_the_request_timeout_is_refused() {
        // A run compiles and executes, so it can never need less time than an
        // ordinary request is allowed.
        let short = parse_playground(
            &PlaygroundSection {
                api: PlaygroundApiSection {
                    enabled: true,
                    timeout_seconds: 10,
                    ..PlaygroundApiSection::default()
                },
                ..PlaygroundSection::default()
            },
            &RateLimitSection::default().playground,
        )
        .expect("a short playground timeout parses on its own");

        let outcome = parse_abuse(&AbuseSection::default(), &UploadsSection::default(), &short);

        assert!(outcome.is_err());
    }

    #[test]
    fn abuse_configuration_rejects_unsafe_bounds_and_bad_cidrs() {
        let parse_with = |trusted: &[&str], request: u64, publication: u64, burst: u64| {
            let rate_limit = RateLimitSection {
                read: RateLimitTier::new(120, burst),
                ..RateLimitSection::default()
            };
            parse_abuse(
                &AbuseSection {
                    trusted_proxy_cidrs: trusted.iter().map(|n| (*n).to_owned()).collect(),
                    request_timeout_seconds: request,
                    publication_timeout_seconds: publication,
                    rate_limit,
                },
                &UploadsSection::default(),
                &playground_defaults(),
            )
        };

        assert!(parse_with(&["not-a-network"], 30, 120, 60).is_err());
        // An empty list is a deliberate choice to trust no proxy, and must
        // stay distinguishable from the omitted default.
        let none = parse_with(&[], 30, 120, 60).expect("trusting no proxy is a valid choice");
        assert!(none.trusted_proxy_cidrs.is_empty());
        assert!(parse_with(&["127.0.0.0/8"], 0, 120, 60).is_err());
        assert!(parse_with(&["127.0.0.0/8"], 30, 20, 60).is_err());
        assert!(parse_with(&["127.0.0.0/8"], 30, 120, 121).is_err());
    }

    #[test]
    fn upload_configuration_uses_a_bounded_default_directory() {
        let uploads = |directory: Option<&str>, capacity| {
            parse_uploads(&UploadsSection {
                temporary_directory: directory.map(PathBuf::from),
                temporary_capacity_bytes: capacity,
                max_concurrency: 8,
            })
        };

        let config = uploads(None, 104_857_600).expect("defaults should parse");
        assert!(config.temporary_directory.ends_with("rux-server-uploads"));
        assert_eq!(config.temporary_capacity_bytes, 100 * 1024 * 1024);
        assert!(uploads(Some("uploads"), 5_242_879).is_err());
        assert!(uploads(Some("uploads"), 10_737_418_241).is_err());
        // Present and empty is a mistake, not a request for the default.
        assert!(uploads(Some(""), 104_857_600).is_err());
    }

    #[test]
    fn the_two_playground_concurrency_limits_are_distinct_settings() {
        // One bounds what the API admits, the other how many containers the
        // broker runs. Under the environment form they shared a name.
        let document = format!(
            "{}\n[playground.api]\nmax_concurrency = 4\n\
             [playground.broker]\nimage = \"rux-playground:1\"\nmax_concurrency = 8\n",
            complete()
        );
        let file = parse(&document).expect("document should deserialize");
        let config = ApiConfig::from_file(&file).expect("config should be valid");
        let broker = file
            .playground
            .broker
            .expect("the broker section is present");

        assert_eq!(config.playground.max_concurrency, 4);
        assert_eq!(broker.max_concurrency, 8);
    }

    #[test]
    fn both_processes_read_one_socket_path() {
        let document = format!("{}\n[playground]\nsocket = \"/tmp/rux.sock\"\n", complete());
        let file = parse(&document).expect("document should deserialize");
        let config = ApiConfig::from_file(&file).expect("config should be valid");

        assert_eq!(
            config.playground.socket_path,
            PathBuf::from("/tmp/rux.sock")
        );
        assert_eq!(file.playground.socket, "/tmp/rux.sock");
    }

    #[test]
    fn the_configuration_path_defaults_and_accepts_both_spellings() {
        let arguments = |rest: &[&str]| {
            std::iter::once("rux-server".to_owned())
                .chain(rest.iter().map(|a| (*a).to_owned()))
                .collect::<Vec<_>>()
        };

        assert_eq!(
            config_path(arguments(&[])).expect("no arguments is the default"),
            PathBuf::from(DEFAULT_CONFIG_PATH)
        );
        assert_eq!(
            config_path(arguments(&["--config", "other.toml"])).expect("separate value"),
            PathBuf::from("other.toml")
        );
        assert_eq!(
            config_path(arguments(&["--config=other.toml"])).expect("joined value"),
            PathBuf::from("other.toml")
        );
    }

    #[test]
    fn an_unusable_command_line_is_refused_rather_than_ignored() {
        let arguments = |rest: &[&str]| {
            std::iter::once("rux-server".to_owned())
                .chain(rest.iter().map(|a| (*a).to_owned()))
                .collect::<Vec<_>>()
        };

        // Silently ignoring a misspelled option would start the server on the
        // wrong configuration, which is exactly the failure this replaces.
        assert!(config_path(arguments(&["--confg", "other.toml"])).is_err());
        assert!(config_path(arguments(&["other.toml"])).is_err());
        assert!(config_path(arguments(&["--config"])).is_err());
        assert!(config_path(arguments(&["--config", "  "])).is_err());
        assert!(config_path(arguments(&["--config", "a.toml", "--config", "b.toml"])).is_err());
    }

    #[test]
    fn a_missing_file_names_itself_and_the_way_out() {
        let error = load(Path::new("config/definitely-absent.toml"))
            .expect_err("a missing file should be reported");
        let message = error.to_string();

        assert!(message.contains("definitely-absent.toml"), "{message}");
        assert!(message.contains("--config"), "{message}");
    }
}
