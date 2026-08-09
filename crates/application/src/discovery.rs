use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rux_domain::{IdentitySegment, SemanticVersion};
use sha2::{Digest, Sha256};
use time::{Duration, Time, UtcOffset};

use crate::{
    Clock, DependentPackageRecord, DiscoveryReader, KeywordBoundary, KeywordRecord,
    PackageDownloadStatisticsRecord, PackageHighlightsRecord, PackageIdentityBoundary,
    PackageVersionHistoryRecord, SitemapBoundary, SitemapEntryKind, SitemapEntryRecord,
};

pub const DEFAULT_DISCOVERY_LIMIT: u16 = 20;
pub const MAX_DISCOVERY_LIMIT: u16 = 100;
pub const DEFAULT_SITEMAP_LIMIT: u16 = 100;
pub const MAX_SITEMAP_LIMIT: u16 = 1_000;
pub const HIGHLIGHT_LIMIT: u16 = 10;
pub const POPULARITY_WINDOW_DAYS: i64 = 30;
pub const DOWNLOAD_STATISTICS_WINDOW_DAYS: i64 = 30;

const CURSOR_VERSION: u8 = 1;
const MAX_CURSOR_BYTES: usize = 512;

#[derive(Clone, Debug, Default)]
pub struct DiscoveryPageParameters {
    pub limit: Option<u16>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DiscoveryPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryErrorKind {
    InvalidNamespace,
    InvalidPackage,
    InvalidLimit,
    InvalidCursor,
    PackageNotFound,
    Unavailable,
}

#[derive(Debug)]
pub struct DiscoveryError {
    kind: DiscoveryErrorKind,
}

impl DiscoveryError {
    #[must_use]
    pub const fn new(kind: DiscoveryErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> DiscoveryErrorKind {
        self.kind
    }
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "package discovery failed: {:?}", self.kind)
    }
}

impl Error for DiscoveryError {}

#[async_trait]
pub trait Discovery: Send + Sync {
    async fn dependents(
        &self,
        namespace: &str,
        package: &str,
        parameters: DiscoveryPageParameters,
    ) -> Result<DiscoveryPage<DependentPackageRecord>, DiscoveryError>;

    async fn keywords(
        &self,
        parameters: DiscoveryPageParameters,
    ) -> Result<DiscoveryPage<KeywordRecord>, DiscoveryError>;

    async fn versions(
        &self,
        namespace: &str,
        package: &str,
        parameters: DiscoveryPageParameters,
    ) -> Result<DiscoveryPage<PackageVersionHistoryRecord>, DiscoveryError>;

    async fn highlights(&self) -> Result<PackageHighlightsRecord, DiscoveryError>;

    async fn download_statistics(
        &self,
        namespace: &str,
        package: &str,
    ) -> Result<PackageDownloadStatisticsRecord, DiscoveryError>;

    async fn sitemap(
        &self,
        parameters: DiscoveryPageParameters,
    ) -> Result<DiscoveryPage<SitemapEntryRecord>, DiscoveryError>;
}

pub struct DiscoveryService {
    repository: Arc<dyn DiscoveryReader>,
    clock: Arc<dyn Clock>,
}

impl DiscoveryService {
    #[must_use]
    pub fn new(repository: Arc<dyn DiscoveryReader>, clock: Arc<dyn Clock>) -> Self {
        Self { repository, clock }
    }
}

#[async_trait]
impl Discovery for DiscoveryService {
    async fn dependents(
        &self,
        namespace: &str,
        package: &str,
        parameters: DiscoveryPageParameters,
    ) -> Result<DiscoveryPage<DependentPackageRecord>, DiscoveryError> {
        let namespace = parse_identity(namespace, DiscoveryErrorKind::InvalidNamespace)?;
        let package = parse_identity(package, DiscoveryErrorKind::InvalidPackage)?;
        let limit = page_limit(
            parameters.limit,
            DEFAULT_DISCOVERY_LIMIT,
            MAX_DISCOVERY_LIMIT,
        )?;
        let scope = scope_hash(
            "dependents",
            &[namespace.normalized(), package.normalized()],
        );
        let boundary = parameters
            .cursor
            .as_deref()
            .map(|value| decode_package_cursor(value, CursorKind::Dependents, scope))
            .transpose()?;
        let mut items = self
            .repository
            .dependent_packages(&namespace, &package, boundary.as_ref(), fetch_limit(limit))
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| DiscoveryError::new(DiscoveryErrorKind::PackageNotFound))?;
        for item in &mut items {
            item.requirements
                .sort_by(|left, right| left.alias.cmp(&right.alias));
        }
        let has_more = items.len() > usize::from(limit);
        items.truncate(usize::from(limit));
        let next_cursor = has_more.then(|| {
            let item = items
                .last()
                .expect("a non-empty over-limit page has a boundary");
            encode_cursor(
                CursorKind::Dependents,
                scope,
                &[
                    item.namespace.normalized().as_bytes(),
                    item.package.normalized().as_bytes(),
                ],
            )
        });
        Ok(DiscoveryPage { items, next_cursor })
    }

    async fn keywords(
        &self,
        parameters: DiscoveryPageParameters,
    ) -> Result<DiscoveryPage<KeywordRecord>, DiscoveryError> {
        let limit = page_limit(
            parameters.limit,
            DEFAULT_DISCOVERY_LIMIT,
            MAX_DISCOVERY_LIMIT,
        )?;
        let scope = scope_hash("keywords", &[]);
        let boundary = parameters
            .cursor
            .as_deref()
            .map(|value| decode_keyword_cursor(value, scope))
            .transpose()?;
        let mut items = self
            .repository
            .keywords(boundary.as_ref(), fetch_limit(limit))
            .await
            .map_err(|_| unavailable())?;
        let has_more = items.len() > usize::from(limit);
        items.truncate(usize::from(limit));
        let next_cursor = has_more.then(|| {
            let item = items
                .last()
                .expect("a non-empty over-limit page has a boundary");
            encode_cursor(
                CursorKind::Keywords,
                scope,
                &[
                    &item.package_count.to_be_bytes(),
                    item.keyword.normalized().as_bytes(),
                ],
            )
        });
        Ok(DiscoveryPage { items, next_cursor })
    }

    async fn versions(
        &self,
        namespace: &str,
        package: &str,
        parameters: DiscoveryPageParameters,
    ) -> Result<DiscoveryPage<PackageVersionHistoryRecord>, DiscoveryError> {
        let namespace = parse_identity(namespace, DiscoveryErrorKind::InvalidNamespace)?;
        let package = parse_identity(package, DiscoveryErrorKind::InvalidPackage)?;
        let limit = page_limit(
            parameters.limit,
            DEFAULT_DISCOVERY_LIMIT,
            MAX_DISCOVERY_LIMIT,
        )?;
        let scope = scope_hash("versions", &[namespace.normalized(), package.normalized()]);
        let boundary = parameters
            .cursor
            .as_deref()
            .map(|value| decode_version_cursor(value, scope))
            .transpose()?;
        let mut items = self
            .repository
            .package_version_history(&namespace, &package, boundary.as_ref(), fetch_limit(limit))
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| DiscoveryError::new(DiscoveryErrorKind::PackageNotFound))?;
        let has_more = items.len() > usize::from(limit);
        items.truncate(usize::from(limit));
        let next_cursor = has_more.then(|| {
            let item = items
                .last()
                .expect("a non-empty over-limit page has a boundary");
            encode_cursor(
                CursorKind::Versions,
                scope,
                &[item.version.as_str().as_bytes()],
            )
        });
        Ok(DiscoveryPage { items, next_cursor })
    }

    async fn highlights(&self) -> Result<PackageHighlightsRecord, DiscoveryError> {
        let until = self.clock.now();
        let since = until - Duration::days(POPULARITY_WINDOW_DAYS);
        self.repository
            .package_highlights(since, until, HIGHLIGHT_LIMIT)
            .await
            .map_err(|_| unavailable())
    }

    async fn download_statistics(
        &self,
        namespace: &str,
        package: &str,
    ) -> Result<PackageDownloadStatisticsRecord, DiscoveryError> {
        let namespace = parse_identity(namespace, DiscoveryErrorKind::InvalidNamespace)?;
        let package = parse_identity(package, DiscoveryErrorKind::InvalidPackage)?;
        let until = self.clock.now();
        let since = until.to_offset(UtcOffset::UTC).replace_time(Time::MIDNIGHT)
            - Duration::days(DOWNLOAD_STATISTICS_WINDOW_DAYS - 1);
        self.repository
            .package_download_statistics(&namespace, &package, since, until)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| DiscoveryError::new(DiscoveryErrorKind::PackageNotFound))
    }

    async fn sitemap(
        &self,
        parameters: DiscoveryPageParameters,
    ) -> Result<DiscoveryPage<SitemapEntryRecord>, DiscoveryError> {
        let limit = page_limit(parameters.limit, DEFAULT_SITEMAP_LIMIT, MAX_SITEMAP_LIMIT)?;
        let scope = scope_hash("sitemap", &[]);
        let boundary = parameters
            .cursor
            .as_deref()
            .map(|value| decode_sitemap_cursor(value, scope))
            .transpose()?;
        let mut items = self
            .repository
            .sitemap_entries(boundary.as_ref(), fetch_limit(limit))
            .await
            .map_err(|_| unavailable())?;
        let has_more = items.len() > usize::from(limit);
        items.truncate(usize::from(limit));
        let next_cursor = has_more.then(|| {
            let item = items
                .last()
                .expect("a non-empty over-limit page has a boundary");
            let (first, second) = sitemap_identity(item);
            let kind = [sitemap_kind_byte(item.kind)];
            let fields = match second {
                Some(second) => vec![kind.as_slice(), first.as_bytes(), second.as_bytes()],
                None => vec![kind.as_slice(), first.as_bytes()],
            };
            encode_cursor(CursorKind::Sitemap, scope, &fields)
        });
        Ok(DiscoveryPage { items, next_cursor })
    }
}

fn parse_identity(
    value: &str,
    kind: DiscoveryErrorKind,
) -> Result<IdentitySegment, DiscoveryError> {
    IdentitySegment::new(value).map_err(|_| DiscoveryError::new(kind))
}

fn page_limit(value: Option<u16>, default: u16, maximum: u16) -> Result<u16, DiscoveryError> {
    let value = value.unwrap_or(default);
    if !(1..=maximum).contains(&value) {
        return Err(DiscoveryError::new(DiscoveryErrorKind::InvalidLimit));
    }
    Ok(value)
}

const fn fetch_limit(limit: u16) -> u16 {
    limit + 1
}

fn unavailable() -> DiscoveryError {
    DiscoveryError::new(DiscoveryErrorKind::Unavailable)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum CursorKind {
    Dependents = 1,
    Keywords = 2,
    Versions = 3,
    Sitemap = 4,
}

fn scope_hash(label: &str, identities: &[&str]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(label.as_bytes());
    for identity in identities {
        digest.update([0]);
        digest.update(identity.as_bytes());
    }
    digest.finalize().into()
}

fn encode_cursor(kind: CursorKind, scope: [u8; 32], fields: &[&[u8]]) -> String {
    let mut bytes = Vec::new();
    bytes.push(CURSOR_VERSION);
    bytes.push(kind as u8);
    bytes.extend_from_slice(&scope);
    for field in fields {
        let length = u16::try_from(field.len()).expect("bounded cursor field fits u16");
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(field);
    }
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decoder(
    value: &str,
    kind: CursorKind,
    scope: [u8; 32],
) -> Result<CursorDecoder, DiscoveryError> {
    if value.len() > MAX_CURSOR_BYTES {
        return Err(invalid_cursor());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| invalid_cursor())?;
    let mut decoder = CursorDecoder::new(bytes);
    if decoder.byte()? != CURSOR_VERSION
        || decoder.byte()? != kind as u8
        || decoder.array::<32>()? != scope
    {
        return Err(invalid_cursor());
    }
    Ok(decoder)
}

fn decode_package_cursor(
    value: &str,
    kind: CursorKind,
    scope: [u8; 32],
) -> Result<PackageIdentityBoundary, DiscoveryError> {
    let mut decoder = decoder(value, kind, scope)?;
    let namespace = decoder.normalized_identity()?;
    let package = decoder.normalized_identity()?;
    decoder.finish()?;
    Ok(PackageIdentityBoundary { namespace, package })
}

fn decode_keyword_cursor(value: &str, scope: [u8; 32]) -> Result<KeywordBoundary, DiscoveryError> {
    let mut decoder = decoder(value, CursorKind::Keywords, scope)?;
    let count = decoder.field()?;
    let package_count = u64::from_be_bytes(count.try_into().map_err(|_| invalid_cursor())?);
    let keyword = decoder.normalized_identity()?;
    decoder.finish()?;
    Ok(KeywordBoundary {
        package_count,
        keyword,
    })
}

fn decode_version_cursor(value: &str, scope: [u8; 32]) -> Result<SemanticVersion, DiscoveryError> {
    let mut decoder = decoder(value, CursorKind::Versions, scope)?;
    let version = std::str::from_utf8(decoder.field()?)
        .map_err(|_| invalid_cursor())
        .and_then(|value| SemanticVersion::new(value).map_err(|_| invalid_cursor()))?;
    decoder.finish()?;
    Ok(version)
}

fn decode_sitemap_cursor(value: &str, scope: [u8; 32]) -> Result<SitemapBoundary, DiscoveryError> {
    let mut decoder = decoder(value, CursorKind::Sitemap, scope)?;
    let kind = decoder.field()?;
    let kind = match kind {
        [1] => SitemapEntryKind::Keyword,
        [2] => SitemapEntryKind::Namespace,
        [3] => SitemapEntryKind::Package,
        _ => return Err(invalid_cursor()),
    };
    let first_identity = decoder.normalized_identity()?;
    let second_identity = if kind == SitemapEntryKind::Package {
        Some(decoder.normalized_identity()?)
    } else {
        None
    };
    decoder.finish()?;
    Ok(SitemapBoundary {
        kind,
        first_identity,
        second_identity,
    })
}

fn sitemap_identity(item: &SitemapEntryRecord) -> (&str, Option<&str>) {
    match item.kind {
        SitemapEntryKind::Keyword => (
            item.keyword
                .as_ref()
                .expect("keyword sitemap record has a keyword")
                .normalized(),
            None,
        ),
        SitemapEntryKind::Namespace => (
            item.namespace
                .as_ref()
                .expect("namespace sitemap record has a namespace")
                .normalized(),
            None,
        ),
        SitemapEntryKind::Package => (
            item.namespace
                .as_ref()
                .expect("package sitemap record has a namespace")
                .normalized(),
            Some(
                item.package
                    .as_ref()
                    .expect("package sitemap record has a package")
                    .normalized(),
            ),
        ),
    }
}

const fn sitemap_kind_byte(kind: SitemapEntryKind) -> u8 {
    match kind {
        SitemapEntryKind::Keyword => 1,
        SitemapEntryKind::Namespace => 2,
        SitemapEntryKind::Package => 3,
    }
}

fn invalid_cursor() -> DiscoveryError {
    DiscoveryError::new(DiscoveryErrorKind::InvalidCursor)
}

struct CursorDecoder {
    bytes: Vec<u8>,
    offset: usize,
}

impl CursorDecoder {
    const fn new(bytes: Vec<u8>) -> Self {
        Self { bytes, offset: 0 }
    }

    fn byte(&mut self) -> Result<u8, DiscoveryError> {
        let value = *self.bytes.get(self.offset).ok_or_else(invalid_cursor)?;
        self.offset += 1;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DiscoveryError> {
        let end = self.offset.checked_add(N).ok_or_else(invalid_cursor)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(invalid_cursor)?
            .try_into()
            .expect("cursor slice length was checked");
        self.offset = end;
        Ok(value)
    }

    fn field(&mut self) -> Result<&[u8], DiscoveryError> {
        let length = usize::from(u16::from_be_bytes(self.array::<2>()?));
        let end = self.offset.checked_add(length).ok_or_else(invalid_cursor)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(invalid_cursor)?;
        self.offset = end;
        Ok(value)
    }

    fn normalized_identity(&mut self) -> Result<String, DiscoveryError> {
        let value = std::str::from_utf8(self.field()?).map_err(|_| invalid_cursor())?;
        let identity = IdentitySegment::new(value).map_err(|_| invalid_cursor())?;
        if identity.as_str() != identity.normalized() {
            return Err(invalid_cursor());
        }
        Ok(value.to_owned())
    }

    fn finish(self) -> Result<(), DiscoveryError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(invalid_cursor())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use time::OffsetDateTime;

    use super::*;
    use crate::{PackageKind, RepositoryError};

    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            OffsetDateTime::from_unix_timestamp(1_785_672_000).unwrap()
        }
    }

    #[derive(Default)]
    struct StubReader {
        dependent_items: Vec<DependentPackageRecord>,
        version_items: Vec<PackageVersionHistoryRecord>,
        highlight_window: Mutex<Option<(OffsetDateTime, OffsetDateTime, u16)>>,
        download_window: Mutex<Option<(OffsetDateTime, OffsetDateTime)>>,
    }

    #[async_trait]
    impl DiscoveryReader for StubReader {
        async fn dependent_packages(
            &self,
            _namespace: &IdentitySegment,
            _package: &IdentitySegment,
            _boundary: Option<&PackageIdentityBoundary>,
            _limit: u16,
        ) -> Result<Option<Vec<DependentPackageRecord>>, RepositoryError> {
            Ok(Some(self.dependent_items.clone()))
        }

        async fn keywords(
            &self,
            _boundary: Option<&KeywordBoundary>,
            _limit: u16,
        ) -> Result<Vec<KeywordRecord>, RepositoryError> {
            Ok(Vec::new())
        }

        async fn package_version_history(
            &self,
            _namespace: &IdentitySegment,
            _package: &IdentitySegment,
            _boundary: Option<&SemanticVersion>,
            _limit: u16,
        ) -> Result<Option<Vec<PackageVersionHistoryRecord>>, RepositoryError> {
            Ok(Some(self.version_items.clone()))
        }

        async fn package_highlights(
            &self,
            since: OffsetDateTime,
            until: OffsetDateTime,
            limit: u16,
        ) -> Result<PackageHighlightsRecord, RepositoryError> {
            *self.highlight_window.lock().unwrap() = Some((since, until, limit));
            Ok(PackageHighlightsRecord::default())
        }

        async fn package_download_statistics(
            &self,
            _namespace: &IdentitySegment,
            _package: &IdentitySegment,
            since: OffsetDateTime,
            until: OffsetDateTime,
        ) -> Result<Option<PackageDownloadStatisticsRecord>, RepositoryError> {
            *self.download_window.lock().unwrap() = Some((since, until));
            Ok(Some(PackageDownloadStatisticsRecord {
                start_date: since.date(),
                end_date: until.date(),
                total_downloads: 0,
                total_all_time: 0,
                daily: Vec::new(),
            }))
        }

        async fn sitemap_entries(
            &self,
            _boundary: Option<&SitemapBoundary>,
            _limit: u16,
        ) -> Result<Vec<SitemapEntryRecord>, RepositoryError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn dependent_cursor_is_scoped_to_the_target_package() {
        let service = DiscoveryService::new(
            Arc::new(StubReader {
                dependent_items: vec![dependent("Alpha"), dependent("Beta")],
                ..StubReader::default()
            }),
            Arc::new(FixedClock),
        );
        let first = service
            .dependents(
                "Rux",
                "Json",
                DiscoveryPageParameters {
                    limit: Some(1),
                    cursor: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(first.items.len(), 1);
        let cursor = first.next_cursor.expect("next page");
        assert_eq!(
            service
                .dependents(
                    "Rux",
                    "Io",
                    DiscoveryPageParameters {
                        limit: Some(1),
                        cursor: Some(cursor),
                    },
                )
                .await
                .unwrap_err()
                .kind(),
            DiscoveryErrorKind::InvalidCursor
        );
    }

    #[tokio::test]
    async fn versions_validate_limits_and_cursor_payloads() {
        let service = DiscoveryService::new(
            Arc::new(StubReader {
                version_items: vec![version("2.0.0"), version("1.0.0")],
                ..StubReader::default()
            }),
            Arc::new(FixedClock),
        );
        assert_eq!(
            service
                .versions(
                    "Rux",
                    "Json",
                    DiscoveryPageParameters {
                        limit: Some(0),
                        cursor: None,
                    },
                )
                .await
                .unwrap_err()
                .kind(),
            DiscoveryErrorKind::InvalidLimit
        );
        let first = service
            .versions(
                "Rux",
                "Json",
                DiscoveryPageParameters {
                    limit: Some(1),
                    cursor: None,
                },
            )
            .await
            .unwrap();
        assert!(first.next_cursor.is_some());
    }

    #[tokio::test]
    async fn highlights_use_a_fixed_thirty_day_window() {
        let reader = Arc::new(StubReader::default());
        let service = DiscoveryService::new(reader.clone(), Arc::new(FixedClock));
        service.highlights().await.unwrap();
        let (since, until, limit) = reader.highlight_window.lock().unwrap().unwrap();
        assert_eq!(until.unix_timestamp(), 1_785_672_000);
        assert_eq!(since.unix_timestamp(), 1_783_080_000);
        assert_eq!(limit, HIGHLIGHT_LIMIT);
    }

    #[tokio::test]
    async fn download_statistics_use_thirty_utc_days_ending_today() {
        let reader = Arc::new(StubReader::default());
        let service = DiscoveryService::new(reader.clone(), Arc::new(FixedClock));
        service.download_statistics("Rux", "Json").await.unwrap();
        let (since, until) = reader.download_window.lock().unwrap().unwrap();
        assert_eq!(until, FixedClock.now());
        assert_eq!(since.hour(), 0);
        assert_eq!(since.minute(), 0);
        assert_eq!(since.second(), 0);
        assert_eq!(
            until.date() - since.date(),
            Duration::days(DOWNLOAD_STATISTICS_WINDOW_DAYS - 1)
        );
    }

    fn dependent(package: &str) -> DependentPackageRecord {
        DependentPackageRecord {
            namespace: identity("Rux"),
            package: identity(package),
            version: SemanticVersion::new("1.0.0").unwrap(),
            package_type: PackageKind::Library,
            description: None,
            published_at: OffsetDateTime::UNIX_EPOCH,
            yanked: false,
            requirements: Vec::new(),
        }
    }

    fn version(value: &str) -> PackageVersionHistoryRecord {
        PackageVersionHistoryRecord {
            version: SemanticVersion::new(value).unwrap(),
            min_rux: SemanticVersion::new("0.4.0").unwrap(),
            package_type: PackageKind::Library,
            published_at: OffsetDateTime::UNIX_EPOCH,
            yanked: false,
        }
    }

    fn identity(value: &str) -> IdentitySegment {
        IdentitySegment::new(value).unwrap()
    }
}
