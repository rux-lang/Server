use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rux_domain::IdentitySegment;

use crate::{
    PackageKind, PackageSearchCriteria, PackageSearchReader, PackageSearchRecord,
    PackageSortDirection, PackageSortOrder,
};

pub const DEFAULT_SEARCH_PAGE_SIZE: u16 = 20;
pub const MAX_SEARCH_PAGE_SIZE: u16 = 100;
pub const MAX_SEARCH_QUERY_BYTES: usize = 256;

/// The furthest page a client may ask for.
///
/// Offset pagination makes a deep page as expensive as every page before it,
/// so the ceiling keeps a crafted `?page=` from scanning the whole catalog.
pub const MAX_SEARCH_PAGE: u32 = 10_000;

#[derive(Clone, Debug, Default)]
pub struct PackageSearchParameters {
    pub query: Option<String>,
    pub namespace: Option<String>,
    pub keyword: Option<String>,
    pub package_type: Option<String>,
    pub sort: Option<String>,
    pub order: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct PackageSearchPage {
    pub items: Vec<PackageSearchRecord>,
    pub total: u64,
    pub page: u32,
    pub per_page: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageSearchErrorKind {
    InvalidQuery,
    InvalidNamespace,
    InvalidKeyword,
    InvalidPackageType,
    InvalidSort,
    InvalidOrder,
    InvalidPage,
    InvalidPerPage,
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
        let per_page = parameters.per_page.unwrap_or(DEFAULT_SEARCH_PAGE_SIZE);
        if !(1..=MAX_SEARCH_PAGE_SIZE).contains(&per_page) {
            return Err(PackageSearchError::new(
                PackageSearchErrorKind::InvalidPerPage,
            ));
        }
        let page = parameters.page.unwrap_or(1);
        if !(1..=MAX_SEARCH_PAGE).contains(&page) {
            return Err(PackageSearchError::new(PackageSearchErrorKind::InvalidPage));
        }
        let result = self
            .repository
            .search_packages(&criteria, page, per_page)
            .await
            .map_err(|_| PackageSearchError::new(PackageSearchErrorKind::Unavailable))?;
        Ok(PackageSearchPage {
            items: result.items,
            total: result.total,
            page,
            per_page,
        })
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
    let sort = parameters
        .sort
        .as_deref()
        .map(parse_sort)
        .transpose()?
        // Relevance scores are all zero without a query, so browsing defaults to
        // the name order the scoring would otherwise degenerate into anyway.
        .unwrap_or(if query.is_some() {
            PackageSortOrder::Relevance
        } else {
            PackageSortOrder::Name
        });
    let order = parameters
        .order
        .as_deref()
        .map(parse_order)
        .transpose()?
        .unwrap_or_else(|| default_sort_direction(sort));
    if sort == PackageSortOrder::Relevance && order == PackageSortDirection::Ascending {
        return Err(PackageSearchError::new(
            PackageSearchErrorKind::InvalidOrder,
        ));
    }
    Ok(PackageSearchCriteria {
        query,
        identity_query,
        namespace,
        keyword,
        package_type,
        sort,
        order,
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

fn parse_sort(value: &str) -> Result<PackageSortOrder, PackageSearchError> {
    match value {
        "relevance" => Ok(PackageSortOrder::Relevance),
        "name" => Ok(PackageSortOrder::Name),
        "downloads" => Ok(PackageSortOrder::Downloads),
        "recent_downloads" => Ok(PackageSortOrder::RecentDownloads),
        "updated" => Ok(PackageSortOrder::Updated),
        "created" => Ok(PackageSortOrder::Created),
        _ => Err(PackageSearchError::new(PackageSearchErrorKind::InvalidSort)),
    }
}

const fn default_sort_direction(sort: PackageSortOrder) -> PackageSortDirection {
    match sort {
        PackageSortOrder::Name => PackageSortDirection::Ascending,
        PackageSortOrder::Relevance
        | PackageSortOrder::Downloads
        | PackageSortOrder::RecentDownloads
        | PackageSortOrder::Updated
        | PackageSortOrder::Created => PackageSortDirection::Descending,
    }
}

fn parse_order(value: &str) -> Result<PackageSortDirection, PackageSearchError> {
    match value {
        "asc" => Ok(PackageSortDirection::Ascending),
        "desc" => Ok(PackageSortDirection::Descending),
        _ => Err(PackageSearchError::new(
            PackageSearchErrorKind::InvalidOrder,
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use rux_domain::SemanticVersion;
    use time::OffsetDateTime;

    use super::*;
    use crate::{PackageSearchPageRecord, RepositoryError, RepositoryErrorKind};

    struct StubReader {
        records: Vec<PackageSearchRecord>,
        total: u64,
        calls: Mutex<Vec<(PackageSearchCriteria, u32, u16)>>,
        unavailable: bool,
    }

    #[async_trait]
    impl PackageSearchReader for StubReader {
        async fn search_packages(
            &self,
            criteria: &PackageSearchCriteria,
            page: u32,
            per_page: u16,
        ) -> Result<PackageSearchPageRecord, RepositoryError> {
            self.calls
                .lock()
                .unwrap()
                .push((criteria.clone(), page, per_page));
            if self.unavailable {
                return Err(RepositoryError::new(RepositoryErrorKind::Unavailable));
            }
            Ok(PackageSearchPageRecord {
                items: self.records.clone(),
                total: self.total,
            })
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
                sort: Some("downloads".into()),
                order: Some("asc".into()),
                page: Some(3),
                per_page: Some(25),
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
        assert_eq!(calls[0].0.sort, PackageSortOrder::Downloads);
        assert_eq!(calls[0].0.order, PackageSortDirection::Ascending);
        assert_eq!(calls[0].1, 3);
        assert_eq!(calls[0].2, 25);
    }

    #[tokio::test]
    async fn sort_defaults_to_relevance_with_a_query_and_name_without_one() {
        let reader = Arc::new(stub(Vec::new()));
        let service = PackageSearchService::new(reader.clone());
        service
            .search(PackageSearchParameters {
                query: Some("json".into()),
                ..PackageSearchParameters::default()
            })
            .await
            .unwrap();
        service
            .search(PackageSearchParameters::default())
            .await
            .unwrap();
        // A whitespace-only query is discarded by canonicalization, so it browses
        // and must pick the browse default rather than a relevance ordering.
        service
            .search(PackageSearchParameters {
                query: Some("   ".into()),
                ..PackageSearchParameters::default()
            })
            .await
            .unwrap();
        let calls = reader.calls.lock().unwrap();
        assert_eq!(calls[0].0.sort, PackageSortOrder::Relevance);
        assert_eq!(calls[0].0.order, PackageSortDirection::Descending);
        assert_eq!(calls[1].0.sort, PackageSortOrder::Name);
        assert_eq!(calls[1].0.order, PackageSortDirection::Ascending);
        assert_eq!(calls[2].0.sort, PackageSortOrder::Name);
        assert_eq!(calls[2].0.order, PackageSortDirection::Ascending);
    }

    #[tokio::test]
    async fn page_defaults_to_one_and_the_page_is_echoed_with_the_total() {
        let reader = Arc::new(StubReader {
            total: 137,
            ..stub(vec![record("Alpha"), record("Beta")])
        });
        let service = PackageSearchService::new(reader.clone());
        let page = service
            .search(PackageSearchParameters {
                per_page: Some(15),
                ..PackageSearchParameters::default()
            })
            .await
            .unwrap();
        assert_eq!(page.total, 137);
        assert_eq!(page.page, 1);
        assert_eq!(page.per_page, 15);
        assert_eq!(reader.calls.lock().unwrap()[0].1, 1);
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
                    sort: Some("stars".into()),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidSort,
            ),
            (
                PackageSearchParameters {
                    per_page: Some(0),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidPerPage,
            ),
            (
                PackageSearchParameters {
                    per_page: Some(MAX_SEARCH_PAGE_SIZE + 1),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidPerPage,
            ),
            (
                PackageSearchParameters {
                    page: Some(0),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidPage,
            ),
            (
                PackageSearchParameters {
                    page: Some(MAX_SEARCH_PAGE + 1),
                    ..PackageSearchParameters::default()
                },
                PackageSearchErrorKind::InvalidPage,
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
    async fn order_validation_rejects_unknown_values_and_ascending_relevance() {
        let service = PackageSearchService::new(Arc::new(stub(Vec::new())));
        for parameters in [
            PackageSearchParameters {
                order: Some("sideways".into()),
                ..PackageSearchParameters::default()
            },
            PackageSearchParameters {
                query: Some("json".into()),
                order: Some("asc".into()),
                ..PackageSearchParameters::default()
            },
        ] {
            assert_eq!(
                service.search(parameters).await.unwrap_err().kind(),
                PackageSearchErrorKind::InvalidOrder
            );
        }
    }

    #[tokio::test]
    async fn repository_failure_is_mapped_to_unavailable() {
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
        let total = records.len() as u64;
        StubReader {
            records,
            total,
            calls: Mutex::new(Vec::new()),
            unavailable: false,
        }
    }

    fn record(package: &str) -> PackageSearchRecord {
        PackageSearchRecord {
            namespace: IdentitySegment::new("Rux").unwrap(),
            package: IdentitySegment::new(package).unwrap(),
            version: SemanticVersion::new("1.0.0").unwrap(),
            package_type: PackageKind::Library,
            description: None,
            published_at: OffsetDateTime::UNIX_EPOCH,
            yanked: false,
            downloads_total: 0,
            downloads_30d: 0,
        }
    }
}
