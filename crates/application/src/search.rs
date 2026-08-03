use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rux_domain::IdentitySegment;
use sha2::{Digest, Sha256};

use crate::{
    PackageKind, PackageSearchBoundary, PackageSearchCriteria, PackageSearchReader,
    PackageSearchRecord,
};

pub const DEFAULT_SEARCH_LIMIT: u16 = 20;
pub const MAX_SEARCH_LIMIT: u16 = 100;
pub const MAX_SEARCH_QUERY_BYTES: usize = 256;
const MAX_CURSOR_BYTES: usize = 512;
const CURSOR_VERSION: u8 = 1;

#[derive(Clone, Debug, Default)]
pub struct PackageSearchParameters {
    pub query: Option<String>,
    pub namespace: Option<String>,
    pub keyword: Option<String>,
    pub package_type: Option<String>,
    pub limit: Option<u16>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug)]
pub struct PackageSearchPage {
    pub items: Vec<PackageSearchRecord>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageSearchErrorKind {
    InvalidQuery,
    InvalidNamespace,
    InvalidKeyword,
    InvalidPackageType,
    InvalidLimit,
    InvalidCursor,
    Unavailable,
}

#[derive(Debug)]
pub struct PackageSearchError {
    kind: PackageSearchErrorKind,
}

impl PackageSearchError {
    #[must_use]
    pub const fn new(kind: PackageSearchErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> PackageSearchErrorKind {
        self.kind
    }
}

impl fmt::Display for PackageSearchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "package search failed: {:?}", self.kind)
    }
}

impl Error for PackageSearchError {}

#[async_trait]
pub trait PackageSearch: Send + Sync {
    async fn search(
        &self,
        parameters: PackageSearchParameters,
    ) -> Result<PackageSearchPage, PackageSearchError>;
}

pub struct PackageSearchService {
    repository: Arc<dyn PackageSearchReader>,
}

impl PackageSearchService {
    #[must_use]
    pub fn new(repository: Arc<dyn PackageSearchReader>) -> Self {
        Self { repository }
    }
}

#[async_trait]
impl PackageSearch for PackageSearchService {
    async fn search(
        &self,
        parameters: PackageSearchParameters,
    ) -> Result<PackageSearchPage, PackageSearchError> {
        let criteria = criteria(&parameters)?;
        let limit = parameters.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
            return Err(PackageSearchError::new(
                PackageSearchErrorKind::InvalidLimit,
            ));
        }
        let criteria_hash = criteria_hash(&criteria);
        let boundary = parameters
            .cursor
            .as_deref()
            .map(|cursor| decode_cursor(cursor, criteria_hash))
            .transpose()?;
        let fetch_limit = limit
            .checked_add(1)
            .expect("the bounded search limit can be incremented");
        let mut items = self
            .repository
            .search_packages(&criteria, boundary.as_ref(), fetch_limit)
            .await
            .map_err(|_| PackageSearchError::new(PackageSearchErrorKind::Unavailable))?;
        let has_more = items.len() > usize::from(limit);
        items.truncate(usize::from(limit));
        let next_cursor = if has_more {
            items.last().map(|item| encode_cursor(item, criteria_hash))
        } else {
            None
        };
        Ok(PackageSearchPage { items, next_cursor })
    }
}

fn criteria(
    parameters: &PackageSearchParameters,
) -> Result<PackageSearchCriteria, PackageSearchError> {
    let query = parameters.query.as_deref().and_then(canonical_query);
    if query
        .as_ref()
        .is_some_and(|query| query.len() > MAX_SEARCH_QUERY_BYTES || query.as_bytes().contains(&0))
    {
        return Err(PackageSearchError::new(
            PackageSearchErrorKind::InvalidQuery,
        ));
    }
    let identity_query = query
        .as_ref()
        .map(|query| query.to_lowercase().replace('_', "-"));
    let namespace = parameters
        .namespace
        .as_deref()
        .map(IdentitySegment::new)
        .transpose()
        .map_err(|_| PackageSearchError::new(PackageSearchErrorKind::InvalidNamespace))?;
    let keyword = parameters
        .keyword
        .as_deref()
        .map(IdentitySegment::new)
        .transpose()
        .map_err(|_| PackageSearchError::new(PackageSearchErrorKind::InvalidKeyword))?;
    let package_type = parameters
        .package_type
        .as_deref()
        .map(parse_package_type)
        .transpose()?;
    Ok(PackageSearchCriteria {
        query,
        identity_query,
        namespace,
        keyword,
        package_type,
    })
}

fn canonical_query(value: &str) -> Option<String> {
    let query = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!query.is_empty()).then_some(query)
}

fn parse_package_type(value: &str) -> Result<PackageKind, PackageSearchError> {
    match value {
        "program" => Ok(PackageKind::Program),
        "library" => Ok(PackageKind::Library),
        "source" => Ok(PackageKind::Source),
        _ => Err(PackageSearchError::new(
            PackageSearchErrorKind::InvalidPackageType,
        )),
    }
}

fn criteria_hash(criteria: &PackageSearchCriteria) -> [u8; 32] {
    let mut digest = Sha256::new();
    let query = criteria.query.as_ref().map(|query| query.to_lowercase());
    hash_optional(&mut digest, query.as_deref());
    hash_optional(
        &mut digest,
        criteria.namespace.as_ref().map(IdentitySegment::normalized),
    );
    hash_optional(
        &mut digest,
        criteria.keyword.as_ref().map(IdentitySegment::normalized),
    );
    digest.update([match criteria.package_type {
        None => 0,
        Some(PackageKind::Program) => 1,
        Some(PackageKind::Library) => 2,
        Some(PackageKind::Source) => 3,
    }]);
    digest.finalize().into()
}

fn hash_optional(digest: &mut Sha256, value: Option<&str>) {
    match value {
        None => digest.update([0]),
        Some(value) => {
            digest.update([1]);
            digest.update(
                u64::try_from(value.len())
                    .expect("search criteria length fits u64")
                    .to_be_bytes(),
            );
            digest.update(value.as_bytes());
        }
    }
}

fn encode_cursor(item: &PackageSearchRecord, criteria_hash: [u8; 32]) -> String {
    let namespace = item.namespace.normalized().as_bytes();
    let package = item.package.normalized().as_bytes();
    let mut bytes = Vec::with_capacity(52 + namespace.len() + package.len());
    bytes.push(CURSOR_VERSION);
    bytes.extend_from_slice(&criteria_hash);
    bytes.push(item.match_class);
    bytes.extend_from_slice(&item.relevance.to_be_bytes());
    bytes.push(u8::try_from(namespace.len()).expect("identity length fits in a byte"));
    bytes.extend_from_slice(namespace);
    bytes.push(u8::try_from(package.len()).expect("identity length fits in a byte"));
    bytes.extend_from_slice(package);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode_cursor(
    value: &str,
    expected_hash: [u8; 32],
) -> Result<PackageSearchBoundary, PackageSearchError> {
    if value.len() > MAX_CURSOR_BYTES {
        return Err(invalid_cursor());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| invalid_cursor())?;
    let mut decoder = CursorDecoder::new(&bytes);
    if decoder.byte()? != CURSOR_VERSION || decoder.array::<32>()? != expected_hash {
        return Err(invalid_cursor());
    }
    let match_class = decoder.byte()?;
    let relevance = i64::from_be_bytes(decoder.array::<8>()?);
    let namespace = decoder.identity()?;
    let package = decoder.identity()?;
    if !decoder.finished() || match_class > 5 || relevance < 0 {
        return Err(invalid_cursor());
    }
    Ok(PackageSearchBoundary {
        match_class,
        relevance,
        namespace,
        package,
    })
}

fn invalid_cursor() -> PackageSearchError {
    PackageSearchError::new(PackageSearchErrorKind::InvalidCursor)
}

struct CursorDecoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> CursorDecoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn byte(&mut self) -> Result<u8, PackageSearchError> {
        let value = *self.bytes.get(self.offset).ok_or_else(invalid_cursor)?;
        self.offset += 1;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], PackageSearchError> {
        let end = self.offset.checked_add(N).ok_or_else(invalid_cursor)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(invalid_cursor)?
            .try_into()
            .expect("the slice length is checked");
        self.offset = end;
        Ok(value)
    }

    fn identity(&mut self) -> Result<String, PackageSearchError> {
        let length = usize::from(self.byte()?);
        let end = self.offset.checked_add(length).ok_or_else(invalid_cursor)?;
        let value = std::str::from_utf8(
            self.bytes
                .get(self.offset..end)
                .ok_or_else(invalid_cursor)?,
        )
        .map_err(|_| invalid_cursor())?;
        self.offset = end;
        let identity = IdentitySegment::new(value).map_err(|_| invalid_cursor())?;
        if identity.as_str() != identity.normalized() {
            return Err(invalid_cursor());
        }
        Ok(value.to_owned())
    }

    const fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rux_domain::SemanticVersion;
    use time::OffsetDateTime;

    use super::*;
    use crate::{RepositoryError, RepositoryErrorKind};

    struct StubReader {
        records: Vec<PackageSearchRecord>,
        calls: Mutex<Vec<(PackageSearchCriteria, Option<PackageSearchBoundary>, u16)>>,
        unavailable: bool,
    }

    #[async_trait]
    impl PackageSearchReader for StubReader {
        async fn search_packages(
            &self,
            criteria: &PackageSearchCriteria,
            boundary: Option<&PackageSearchBoundary>,
            limit: u16,
        ) -> Result<Vec<PackageSearchRecord>, RepositoryError> {
            self.calls
                .lock()
                .unwrap()
                .push((criteria.clone(), boundary.cloned(), limit));
            if self.unavailable {
                return Err(RepositoryError::new(RepositoryErrorKind::Unavailable));
            }
            Ok(self.records.clone())
        }
    }

    #[tokio::test]
    async fn search_validates_and_canonicalizes_parameters() {
        let reader = Arc::new(stub(Vec::new()));
        let service = PackageSearchService::new(reader.clone());
        service
            .search(PackageSearchParameters {
                query: Some("  Fast   JSON  ".into()),
                namespace: Some("Rux_Tools".into()),
                keyword: Some("Data_Formats".into()),
                package_type: Some("library".into()),
                limit: Some(25),
                cursor: None,
            })
            .await
            .unwrap();
        let calls = reader.calls.lock().unwrap();
        assert_eq!(calls[0].0.query.as_deref(), Some("Fast JSON"));
        assert_eq!(calls[0].0.identity_query.as_deref(), Some("fast json"));
        assert_eq!(
            calls[0].0.namespace.as_ref().unwrap().normalized(),
            "rux-tools"
        );
        assert_eq!(
            calls[0].0.keyword.as_ref().unwrap().normalized(),
            "data-formats"
        );
        assert_eq!(calls[0].0.package_type, Some(PackageKind::Library));
        assert_eq!(calls[0].2, 26);
    }

    #[tokio::test]
    async fn blank_query_browses_and_validation_errors_are_stable() {
        let service = PackageSearchService::new(Arc::new(stub(Vec::new())));
        assert!(
            service
                .search(PackageSearchParameters {
                    query: Some("  \t ".into()),
                    ..PackageSearchParameters::default()
                })
                .await
                .is_ok()
        );
        let cases = [
            (
                PackageSearchParameters {
                    query: Some("x".repeat(257)),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidQuery,
            ),
            (
                PackageSearchParameters {
                    query: Some("json\0parser".into()),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidQuery,
            ),
            (
                PackageSearchParameters {
                    namespace: Some("bad namespace".into()),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidNamespace,
            ),
            (
                PackageSearchParameters {
                    keyword: Some("bad--keyword".into()),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidKeyword,
            ),
            (
                PackageSearchParameters {
                    package_type: Some("native".into()),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidPackageType,
            ),
            (
                PackageSearchParameters {
                    limit: Some(0),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidLimit,
            ),
        ];
        for (parameters, expected) in cases {
            assert_eq!(
                service.search(parameters).await.unwrap_err().kind(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn cursor_round_trips_and_is_bound_to_criteria() {
        let reader = Arc::new(stub(vec![record("Alpha", 5), record("Beta", 4)]));
        let service = PackageSearchService::new(reader.clone());
        let first = service
            .search(PackageSearchParameters {
                query: Some("json".into()),
                limit: Some(1),
                ..PackageSearchParameters::default()
            })
            .await
            .unwrap();
        let cursor = first.next_cursor.expect("another page should be present");
        service
            .search(PackageSearchParameters {
                query: Some("json".into()),
                limit: Some(20),
                cursor: Some(cursor.clone()),
                ..PackageSearchParameters::default()
            })
            .await
            .unwrap();
        {
            let calls = reader.calls.lock().unwrap();
            assert_eq!(calls[1].1.as_ref().unwrap().package, "alpha");
        }
        assert_eq!(
            service
                .search(PackageSearchParameters {
                    query: Some("http".into()),
                    cursor: Some(cursor),
                    ..PackageSearchParameters::default()
                })
                .await
                .unwrap_err()
                .kind(),
            PackageSearchErrorKind::InvalidCursor
        );
    }

    #[tokio::test]
    async fn malformed_cursor_and_repository_failure_are_mapped() {
        let service = PackageSearchService::new(Arc::new(stub(Vec::new())));
        assert_eq!(
            service
                .search(PackageSearchParameters {
                    cursor: Some("not-base64!".into()),
                    ..PackageSearchParameters::default()
                })
                .await
                .unwrap_err()
                .kind(),
            PackageSearchErrorKind::InvalidCursor
        );
        let unavailable = PackageSearchService::new(Arc::new(StubReader {
            unavailable: true,
            ..stub(Vec::new())
        }));
        assert_eq!(
            unavailable
                .search(PackageSearchParameters::default())
                .await
                .unwrap_err()
                .kind(),
            PackageSearchErrorKind::Unavailable
        );
    }

    fn stub(records: Vec<PackageSearchRecord>) -> StubReader {
        StubReader {
            records,
            calls: Mutex::new(Vec::new()),
            unavailable: false,
        }
    }

    fn record(package: &str, match_class: u8) -> PackageSearchRecord {
        PackageSearchRecord {
            namespace: IdentitySegment::new("Rux").unwrap(),
            package: IdentitySegment::new(package).unwrap(),
            version: SemanticVersion::new("1.0.0").unwrap(),
            package_type: PackageKind::Library,
            description: None,
            published_at: OffsetDateTime::UNIX_EPOCH,
            yanked: false,
            match_class,
            relevance: 10,
        }
    }
}
