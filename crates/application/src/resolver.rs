use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rux_domain::IdentitySegment;

use crate::{ResolverIndexReader, ResolverIndexRecord};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolverIndexErrorKind {
    InvalidNamespace,
    InvalidPackage,
    PackageNotFound,
    Unavailable,
}

#[derive(Debug)]
pub struct ResolverIndexError {
    kind: ResolverIndexErrorKind,
}

impl ResolverIndexError {
    #[must_use]
    pub const fn new(kind: ResolverIndexErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> ResolverIndexErrorKind {
        self.kind
    }
}

impl fmt::Display for ResolverIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "resolver index operation failed: {:?}",
            self.kind
        )
    }
}

impl Error for ResolverIndexError {}

#[async_trait]
pub trait ResolverIndexes: Send + Sync {
    async fn get(
        &self,
        namespace: &str,
        package: &str,
    ) -> Result<ResolverIndexRecord, ResolverIndexError>;
}

pub struct ResolverIndexService {
    repository: Arc<dyn ResolverIndexReader>,
}

impl ResolverIndexService {
    #[must_use]
    pub fn new(repository: Arc<dyn ResolverIndexReader>) -> Self {
        Self { repository }
    }
}

#[async_trait]
impl ResolverIndexes for ResolverIndexService {
    async fn get(
        &self,
        namespace: &str,
        package: &str,
    ) -> Result<ResolverIndexRecord, ResolverIndexError> {
        let namespace = IdentitySegment::new(namespace)
            .map_err(|_| ResolverIndexError::new(ResolverIndexErrorKind::InvalidNamespace))?;
        let package = IdentitySegment::new(package)
            .map_err(|_| ResolverIndexError::new(ResolverIndexErrorKind::InvalidPackage))?;
        let mut index = self
            .repository
            .resolver_index_by_name(&namespace, &package)
            .await
            .map_err(|_| ResolverIndexError::new(ResolverIndexErrorKind::Unavailable))?
            .ok_or_else(|| ResolverIndexError::new(ResolverIndexErrorKind::PackageNotFound))?;

        index
            .versions
            .sort_by(|left, right| left.version.cmp(&right.version));
        for version in &mut index.versions {
            version
                .dependencies
                .sort_by(|left, right| left.alias.cmp(&right.alias));
        }
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use rux_domain::{SemanticVersion, VersionRange};

    use super::*;
    use crate::{DependencyRecord, RepositoryError, ResolverVersionRecord};

    struct StubReader {
        result: Result<Option<ResolverIndexRecord>, ResolverIndexErrorKind>,
    }

    #[async_trait]
    impl ResolverIndexReader for StubReader {
        async fn resolver_index_by_name(
            &self,
            _namespace: &IdentitySegment,
            _package: &IdentitySegment,
        ) -> Result<Option<ResolverIndexRecord>, RepositoryError> {
            match &self.result {
                Ok(index) => Ok(index.clone()),
                Err(_) => Err(RepositoryError::new(
                    crate::RepositoryErrorKind::Unavailable,
                )),
            }
        }
    }

    #[tokio::test]
    async fn resolver_index_sorts_versions_and_dependencies() {
        let service = ResolverIndexService::new(Arc::new(StubReader {
            result: Ok(Some(ResolverIndexRecord {
                namespace: identity("Rux"),
                package: identity("Example"),
                versions: vec![
                    resolver_version(
                        "1.0.0+windows",
                        vec![dependency("Zed"), dependency("Alpha")],
                    ),
                    resolver_version("1.0.0-alpha", Vec::new()),
                    resolver_version("1.0.0+linux", Vec::new()),
                ],
            })),
        }));

        let index = service
            .get("rux", "example")
            .await
            .expect("resolver index should load");

        assert_eq!(
            index
                .versions
                .iter()
                .map(|version| version.version.as_str())
                .collect::<Vec<_>>(),
            ["1.0.0-alpha", "1.0.0+linux", "1.0.0+windows"]
        );
        assert_eq!(index.versions[2].dependencies[0].alias.as_str(), "Alpha");
    }

    #[tokio::test]
    async fn resolver_index_reports_validation_lookup_and_repository_failures() {
        let found = ResolverIndexService::new(Arc::new(StubReader {
            result: Ok(Some(ResolverIndexRecord {
                namespace: identity("Rux"),
                package: identity("Example"),
                versions: Vec::new(),
            })),
        }));
        assert_eq!(
            found
                .get("bad namespace", "example")
                .await
                .unwrap_err()
                .kind(),
            ResolverIndexErrorKind::InvalidNamespace
        );
        assert_eq!(
            found.get("rux", "bad package").await.unwrap_err().kind(),
            ResolverIndexErrorKind::InvalidPackage
        );

        let missing = ResolverIndexService::new(Arc::new(StubReader { result: Ok(None) }));
        assert_eq!(
            missing.get("rux", "missing").await.unwrap_err().kind(),
            ResolverIndexErrorKind::PackageNotFound
        );

        let unavailable = ResolverIndexService::new(Arc::new(StubReader {
            result: Err(ResolverIndexErrorKind::Unavailable),
        }));
        assert_eq!(
            unavailable.get("rux", "example").await.unwrap_err().kind(),
            ResolverIndexErrorKind::Unavailable
        );
    }

    fn resolver_version(value: &str, dependencies: Vec<DependencyRecord>) -> ResolverVersionRecord {
        ResolverVersionRecord {
            version: SemanticVersion::new(value).expect("valid version fixture"),
            min_rux: SemanticVersion::new("0.4.0").expect("valid minimum version fixture"),
            yanked: false,
            dependencies,
        }
    }

    fn dependency(alias: &str) -> DependencyRecord {
        DependencyRecord {
            alias: identity(alias),
            target_namespace: identity("Rux"),
            target_package: identity(alias),
            version_range: VersionRange::new("^1").expect("valid range fixture"),
        }
    }

    fn identity(value: &str) -> IdentitySegment {
        IdentitySegment::new(value).expect("valid identity fixture")
    }
}
