use std::env;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use ipnet::IpNet;
use rux_application::OrphanCleanupPolicy;
use rux_infrastructure::{GitHubOAuthConfig, InfrastructureConfig};
use rux_server::abuse::{AbuseConfig, RateLimit};
use rux_server::upload::DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES;
use time::Duration;
use url::{Host, Url};

pub struct ApiConfig {
    pub bind_address: SocketAddr,
    pub allowed_web_origin: String,
    pub web_callback_url: Url,
    pub github_oauth: GitHubOAuthConfig,
    pub infrastructure: InfrastructureConfig,
    pub package_cdn_base_url: Url,
    pub orphan_cleanup: OrphanCleanupConfig,
    pub observability: ObservabilityConfig,
    pub abuse: AbuseConfig,
    pub uploads: UploadConfig,
}

#[derive(Clone, Copy, Debug)]
pub struct OrphanCleanupConfig {
    pub interval: StdDuration,
    pub policy: OrphanCleanupPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceExporter {
    None,
    Otlp,
}

#[derive(Clone, Copy, Debug)]
pub struct ObservabilityConfig {
    pub metrics_bind_address: SocketAddr,
    pub dependency_probe_interval: StdDuration,
    pub trace_exporter: TraceExporter,
}

#[derive(Clone, Debug)]
pub struct UploadConfig {
    pub temporary_directory: PathBuf,
    pub temporary_capacity_bytes: u64,
}

impl ApiConfig {
    pub fn from_environment() -> Result<Self, Box<dyn std::error::Error>> {
        let allowed_web_origin =
            parse_origin(&value("RUX_ALLOWED_WEB_ORIGIN", "http://localhost:3000"))?;
        let web_callback_url = parse_web_callback_url(
            &value(
                "RUX_WEB_CALLBACK_URL",
                "http://localhost:3000/packages/-/auth/callback",
            ),
            &allowed_web_origin,
        )?;
        let github_callback_url = parse_callback_url(&value(
            "RUX_GITHUB_CALLBACK_URL",
            "http://localhost:8080/v1/auth/github/callback",
        ))?;
        Ok(Self {
            bind_address: value("RUX_BIND_ADDRESS", "127.0.0.1:8080").parse()?,
            allowed_web_origin: allowed_web_origin.origin().ascii_serialization(),
            web_callback_url,
            github_oauth: GitHubOAuthConfig {
                client_id: required_nonempty("RUX_GITHUB_CLIENT_ID")?,
                client_secret: required_nonempty("RUX_GITHUB_CLIENT_SECRET")?,
                callback_url: github_callback_url,
            },
            infrastructure: InfrastructureConfig {
                database_url: required("RUX_DATABASE_URL")?,
                storage_endpoint: required("RUX_STORAGE_ENDPOINT")?,
                storage_bucket: required("RUX_STORAGE_BUCKET")?,
                storage_access_key: required("RUX_STORAGE_ACCESS_KEY")?,
                storage_secret_key: required("RUX_STORAGE_SECRET_KEY")?,
                storage_region: value("RUX_STORAGE_REGION", "us-east-1"),
                storage_force_path_style: value("RUX_STORAGE_FORCE_PATH_STYLE", "false").parse()?,
            },
            package_cdn_base_url: parse_package_cdn_base_url(&value(
                "RUX_PACKAGE_CDN_BASE_URL",
                "http://localhost:9000/packages/",
            ))?,
            orphan_cleanup: parse_orphan_cleanup(
                &value("RUX_ORPHAN_CLEANUP_INTERVAL_SECONDS", "3600"),
                &value("RUX_ORPHAN_CLEANUP_MIN_AGE_SECONDS", "86400"),
                &value("RUX_ORPHAN_CLEANUP_SCAN_LIMIT", "1000"),
                &value("RUX_ORPHAN_CLEANUP_DELETE_LIMIT", "100"),
            )?,
            observability: parse_observability(
                &value("RUX_METRICS_BIND_ADDRESS", "127.0.0.1:9464"),
                &value("RUX_METRICS_ALLOW_NON_LOOPBACK", "false"),
                &value("RUX_DEPENDENCY_PROBE_INTERVAL_SECONDS", "15"),
                &value("OTEL_TRACES_EXPORTER", "none"),
            )?,
            abuse: parse_abuse(
                &value("RUX_TRUSTED_PROXY_CIDRS", "127.0.0.0/8,::1/128"),
                &value("RUX_REQUEST_TIMEOUT_SECONDS", "30"),
                &value("RUX_PUBLICATION_TIMEOUT_SECONDS", "120"),
                &value("RUX_RATE_LIMIT_READ_PER_MINUTE", "120"),
                &value("RUX_RATE_LIMIT_READ_BURST", "60"),
                &value("RUX_RATE_LIMIT_SECURITY_PER_MINUTE", "30"),
                &value("RUX_RATE_LIMIT_SECURITY_BURST", "10"),
                &value("RUX_RATE_LIMIT_MUTATION_PER_MINUTE", "60"),
                &value("RUX_RATE_LIMIT_MUTATION_BURST", "20"),
                &value("RUX_RATE_LIMIT_PUBLICATION_PER_MINUTE", "6"),
                &value("RUX_RATE_LIMIT_PUBLICATION_BURST", "2"),
                &value("RUX_UPLOAD_MAX_CONCURRENCY", "8"),
            )?,
            uploads: parse_uploads(
                &value("RUX_UPLOAD_TEMPORARY_DIRECTORY", ""),
                &value(
                    "RUX_UPLOAD_TEMPORARY_CAPACITY_BYTES",
                    &DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES.to_string(),
                ),
            )?,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_abuse(
    trusted_proxy_cidrs: &str,
    request_timeout_seconds: &str,
    publication_timeout_seconds: &str,
    read_per_minute: &str,
    read_burst: &str,
    security_per_minute: &str,
    security_burst: &str,
    mutation_per_minute: &str,
    mutation_burst: &str,
    publication_per_minute: &str,
    publication_burst: &str,
    upload_max_concurrency: &str,
) -> Result<AbuseConfig, Box<dyn std::error::Error>> {
    let request_timeout = bounded_u64(
        "RUX_REQUEST_TIMEOUT_SECONDS",
        request_timeout_seconds,
        1,
        120,
    )?;
    let publication_timeout = bounded_u64(
        "RUX_PUBLICATION_TIMEOUT_SECONDS",
        publication_timeout_seconds,
        request_timeout,
        600,
    )?;
    Ok(AbuseConfig {
        trusted_proxy_cidrs: parse_cidrs(trusted_proxy_cidrs)?,
        request_timeout: StdDuration::from_secs(request_timeout),
        publication_timeout: StdDuration::from_secs(publication_timeout),
        read: parse_rate_limit("READ", read_per_minute, read_burst)?,
        security: parse_rate_limit("SECURITY", security_per_minute, security_burst)?,
        mutation: parse_rate_limit("MUTATION", mutation_per_minute, mutation_burst)?,
        publication: parse_rate_limit("PUBLICATION", publication_per_minute, publication_burst)?,
        upload_max_concurrency: usize::try_from(bounded_u64(
            "RUX_UPLOAD_MAX_CONCURRENCY",
            upload_max_concurrency,
            1,
            64,
        )?)
        .expect("the configured upper bound fits usize"),
    })
}

fn parse_cidrs(value: &str) -> Result<Arc<[IpNet]>, Box<dyn std::error::Error>> {
    if value.trim().is_empty() {
        return Ok(Arc::from([]));
    }
    value
        .split(',')
        .map(|network| network.trim().parse::<IpNet>().map_err(Into::into))
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()
        .map(Into::into)
}

fn parse_rate_limit(
    tier: &str,
    per_minute: &str,
    burst: &str,
) -> Result<RateLimit, Box<dyn std::error::Error>> {
    let per_minute = bounded_u64(
        &format!("RUX_RATE_LIMIT_{tier}_PER_MINUTE"),
        per_minute,
        1,
        10_000,
    )?;
    let burst = bounded_u64(
        &format!("RUX_RATE_LIMIT_{tier}_BURST"),
        burst,
        1,
        per_minute,
    )?;
    Ok(RateLimit {
        per_minute: u32::try_from(per_minute).expect("the configured upper bound fits u32"),
        burst: u32::try_from(burst).expect("the configured upper bound fits u32"),
    })
}

fn parse_uploads(
    temporary_directory: &str,
    temporary_capacity_bytes: &str,
) -> Result<UploadConfig, Box<dyn std::error::Error>> {
    let temporary_directory = if temporary_directory.trim().is_empty() {
        std::env::temp_dir().join("rux-server-uploads")
    } else {
        PathBuf::from(temporary_directory)
    };
    let temporary_capacity_bytes = bounded_u64(
        "RUX_UPLOAD_TEMPORARY_CAPACITY_BYTES",
        temporary_capacity_bytes,
        5 * 1024 * 1024,
        10 * 1024 * 1024 * 1024,
    )?;
    Ok(UploadConfig {
        temporary_directory,
        temporary_capacity_bytes,
    })
}

fn bounded_u64(
    name: &str,
    input: &str,
    minimum: u64,
    maximum: u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    let parsed = input.parse::<u64>()?;
    if !(minimum..=maximum).contains(&parsed) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be between {minimum} and {maximum}"),
        )
        .into());
    }
    Ok(parsed)
}

fn parse_observability(
    bind_address: &str,
    allow_non_loopback: &str,
    probe_interval_seconds: &str,
    trace_exporter: &str,
) -> Result<ObservabilityConfig, Box<dyn std::error::Error>> {
    let metrics_bind_address: SocketAddr = bind_address.parse()?;
    let allow_non_loopback: bool = allow_non_loopback.parse()?;
    if !metrics_bind_address.ip().is_loopback() && !allow_non_loopback {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "RUX_METRICS_BIND_ADDRESS must be loopback unless RUX_METRICS_ALLOW_NON_LOOPBACK=true",
        )
        .into());
    }
    let dependency_probe_interval = StdDuration::from_secs(positive_u64(
        "RUX_DEPENDENCY_PROBE_INTERVAL_SECONDS",
        probe_interval_seconds,
    )?);
    let trace_exporter = match trace_exporter.trim().to_ascii_lowercase().as_str() {
        "" | "none" => TraceExporter::None,
        "otlp" => TraceExporter::Otlp,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "OTEL_TRACES_EXPORTER must be none or otlp",
            )
            .into());
        }
    };
    Ok(ObservabilityConfig {
        metrics_bind_address,
        dependency_probe_interval,
        trace_exporter,
    })
}

fn parse_package_cdn_base_url(value: &str) -> Result<Url, Box<dyn std::error::Error>> {
    let url = Url::parse(value)?;
    validate_web_url(&url, "RUX_PACKAGE_CDN_BASE_URL")?;
    if url.cannot_be_a_base()
        || !url.path().ends_with('/')
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_url(
            "RUX_PACKAGE_CDN_BASE_URL must be a hierarchical base URL ending in / without a query or fragment",
        ));
    }
    Ok(url)
}

fn parse_orphan_cleanup(
    interval_seconds: &str,
    minimum_age_seconds: &str,
    scan_limit: &str,
    delete_limit: &str,
) -> Result<OrphanCleanupConfig, Box<dyn std::error::Error>> {
    let interval_seconds = positive_u64("RUX_ORPHAN_CLEANUP_INTERVAL_SECONDS", interval_seconds)?;
    let minimum_age_seconds =
        positive_i64("RUX_ORPHAN_CLEANUP_MIN_AGE_SECONDS", minimum_age_seconds)?;
    let scan_limit = scan_limit.parse::<u16>()?;
    let delete_limit = delete_limit.parse::<u16>()?;
    let policy = OrphanCleanupPolicy::new(
        Duration::seconds(minimum_age_seconds),
        scan_limit,
        delete_limit,
    )?;
    Ok(OrphanCleanupConfig {
        interval: StdDuration::from_secs(interval_seconds),
        policy,
    })
}

fn positive_u64(name: &str, input: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let parsed = input.parse::<u64>()?;
    if parsed == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be positive"),
        )
        .into());
    }
    Ok(parsed)
}

fn positive_i64(name: &str, input: &str) -> Result<i64, Box<dyn std::error::Error>> {
    let parsed = input.parse::<i64>()?;
    if parsed <= 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be positive"),
        )
        .into());
    }
    Ok(parsed)
}

fn required(name: &str) -> Result<String, env::VarError> {
    env::var(name)
}

fn required_nonempty(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let value = required(name)?;
    if value.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must not be empty"),
        )
        .into());
    }
    Ok(value)
}

fn value(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn parse_origin(value: &str) -> Result<Url, Box<dyn std::error::Error>> {
    let url = Url::parse(value)?;
    validate_web_url(&url, "RUX_ALLOWED_WEB_ORIGIN")?;
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(invalid_url(
            "RUX_ALLOWED_WEB_ORIGIN must contain only an origin",
        ));
    }
    Ok(url)
}

fn parse_callback_url(value: &str) -> Result<Url, Box<dyn std::error::Error>> {
    let url = Url::parse(value)?;
    validate_web_url(&url, "RUX_GITHUB_CALLBACK_URL")?;
    if url.path() != "/v1/auth/github/callback" || url.query().is_some() || url.fragment().is_some()
    {
        return Err(invalid_url(
            "RUX_GITHUB_CALLBACK_URL must use /v1/auth/github/callback without a query or fragment",
        ));
    }
    Ok(url)
}

fn parse_web_callback_url(
    value: &str,
    allowed_web_origin: &Url,
) -> Result<Url, Box<dyn std::error::Error>> {
    let url = Url::parse(value)?;
    validate_web_url(&url, "RUX_WEB_CALLBACK_URL")?;
    if url.origin() != allowed_web_origin.origin()
        || url.path() != "/packages/-/auth/callback"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_url(
            "RUX_WEB_CALLBACK_URL must use the allowed web origin and /packages/-/auth/callback without a query or fragment",
        ));
    }
    Ok(url)
}

fn validate_web_url(url: &Url, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !url.username().is_empty() || url.password().is_some() || url.host().is_none() {
        return Err(invalid_url(&format!(
            "{name} must be an absolute URL without credentials"
        )));
    }
    if url.scheme() == "https" || (url.scheme() == "http" && is_loopback(url)) {
        return Ok(());
    }
    Err(invalid_url(&format!(
        "{name} must use HTTPS except for loopback development"
    )))
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

fn invalid_url(message: &str) -> Box<dyn std::error::Error> {
    io::Error::new(io::ErrorKind::InvalidInput, message).into()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn orphan_cleanup_defaults_are_bounded_and_positive() {
        let config = parse_orphan_cleanup("3600", "86400", "1000", "100")
            .expect("default cleanup config should parse");

        assert_eq!(config.interval, StdDuration::from_secs(3_600));
        assert_eq!(config.policy.minimum_age(), Duration::hours(24));
        assert_eq!(config.policy.scan_limit(), 1_000);
        assert_eq!(config.policy.delete_limit(), 100);
    }

    #[test]
    fn orphan_cleanup_rejects_invalid_limits_and_durations() {
        assert!(parse_orphan_cleanup("0", "86400", "1000", "100").is_err());
        assert!(parse_orphan_cleanup("3600", "0", "1000", "100").is_err());
        assert!(parse_orphan_cleanup("3600", "86400", "1001", "100").is_err());
        assert!(parse_orphan_cleanup("3600", "86400", "100", "101").is_err());
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
    fn observability_defaults_are_private_and_bounded() {
        let config = parse_observability("127.0.0.1:9464", "false", "15", "none")
            .expect("default observability config should parse");
        assert!(config.metrics_bind_address.ip().is_loopback());
        assert_eq!(config.dependency_probe_interval, StdDuration::from_secs(15));
        assert_eq!(config.trace_exporter, TraceExporter::None);
    }

    #[test]
    fn observability_requires_an_explicit_non_loopback_override() {
        assert!(parse_observability("0.0.0.0:9464", "false", "15", "none").is_err());
        assert!(parse_observability("0.0.0.0:9464", "true", "15", "otlp").is_ok());
        assert!(parse_observability("127.0.0.1:9464", "false", "0", "none").is_err());
        assert!(parse_observability("127.0.0.1:9464", "false", "15", "zipkin").is_err());
    }

    #[test]
    fn abuse_defaults_are_tiered_and_trust_only_loopback_proxies() {
        let config = parse_abuse(
            "127.0.0.0/8,::1/128",
            "30",
            "120",
            "120",
            "60",
            "30",
            "10",
            "60",
            "20",
            "6",
            "2",
            "8",
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
    }

    #[test]
    fn abuse_configuration_rejects_unsafe_bounds_and_bad_cidrs() {
        let parse = |trusted: &str, request: &str, publication: &str, burst: &str| {
            parse_abuse(
                trusted,
                request,
                publication,
                "120",
                burst,
                "30",
                "10",
                "60",
                "20",
                "6",
                "2",
                "8",
            )
        };
        assert!(parse("not-a-network", "30", "120", "60").is_err());
        assert!(parse("", "30", "120", "60").is_ok());
        assert!(parse("127.0.0.0/8", "0", "120", "60").is_err());
        assert!(parse("127.0.0.0/8", "30", "20", "60").is_err());
        assert!(parse("127.0.0.0/8", "30", "120", "121").is_err());
    }

    #[test]
    fn upload_configuration_uses_a_bounded_default_directory() {
        let config = parse_uploads("", "104857600").expect("defaults should parse");
        assert!(config.temporary_directory.ends_with("rux-server-uploads"));
        assert_eq!(config.temporary_capacity_bytes, 100 * 1024 * 1024);
        assert!(parse_uploads("uploads", "5242879").is_err());
        assert!(parse_uploads("uploads", "10737418241").is_err());
    }
}
