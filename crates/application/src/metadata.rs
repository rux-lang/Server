use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rux_domain::{IdentitySegment, SemanticVersion};

use crate::{PackageMetadataReader, PackageSummaryRecord, PackageVersionMetadataRecord};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageMetadataErrorKind {
    InvalidNamespace,
    InvalidPackage,
    InvalidVersion,
    PackageNotFound,
    PackageVersionNotFound,
    Unavailable,
}

#[derive(Debug)]
pub struct PackageMetadataError {
    kind: PackageMetadataErrorKind,
}

impl PackageMetadataError {
    #[must_use]
    pub const fn new(kind: PackageMetadataErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> PackageMetadataErrorKind {
        self.kind
    }
}

impl fmt::Display for PackageMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "package metadata operation failed: {:?}",
            self.kind
        )
    }
}

impl Error for PackageMetadataError {}

#[async_trait]
pub trait PackageMetadata: Send + Sync {
    async fn package(
        &self,
        namespace: &str,
        package: &str,
    ) -> Result<PackageSummaryRecord, PackageMetadataError>;

    async fn version(
        &self,
        namespace: &str,
        package: &str,
        version: &str,
    ) -> Result<PackageVersionMetadataRecord, PackageMetadataError>;
}

pub struct PackageMetadataService {
    repository: Arc<dyn PackageMetadataReader>,
}

impl PackageMetadataService {
    #[must_use]
    pub fn new(repository: Arc<dyn PackageMetadataReader>) -> Self {
        Self { repository }
    }
}

#[async_trait]
impl PackageMetadata for PackageMetadataService {
    async fn package(
        &self,
        namespace: &str,
        package: &str,
    ) -> Result<PackageSummaryRecord, PackageMetadataError> {
        let namespace = parse_namespace(namespace)?;
        let package = parse_package(package)?;
        self.repository
            .package_summary_by_name(&namespace, &package)
            .await
            .map_err(|_| PackageMetadataError::new(PackageMetadataErrorKind::Unavailable))?
            .ok_or_else(|| PackageMetadataError::new(PackageMetadataErrorKind::PackageNotFound))
    }

    async fn version(
        &self,
        namespace: &str,
        package: &str,
        version: &str,
    ) -> Result<PackageVersionMetadataRecord, PackageMetadataError> {
        let namespace = parse_namespace(namespace)?;
        let package = parse_package(package)?;
        let version = SemanticVersion::new(version)
            .map_err(|_| PackageMetadataError::new(PackageMetadataErrorKind::InvalidVersion))?;
        let mut metadata = self
            .repository
            .package_version_metadata_by_name(&namespace, &package, &version)
            .await
            .map_err(|_| PackageMetadataError::new(PackageMetadataErrorKind::Unavailable))?
            .ok_or_else(|| {
                PackageMetadataError::new(PackageMetadataErrorKind::PackageVersionNotFound)
            })?;
        metadata
            .dependencies
            .sort_by(|left, right| left.alias.cmp(&right.alias));
        Ok(metadata)
    }
}

fn parse_namespace(value: &str) -> Result<IdentitySegment, PackageMetadataError> {
    IdentitySegment::new(value)
        .map_err(|_| PackageMetadataError::new(PackageMetadataErrorKind::InvalidNamespace))
}

fn parse_package(value: &str) -> Result<IdentitySegment, PackageMetadataError> {
    IdentitySegment::new(value)
        .map_err(|_| PackageMetadataError::new(PackageMetadataErrorKind::InvalidPackage))
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use rux_domain::VersionRange;
    use serde_json::Map;
    use time::OffsetDateTime;

    use super::*;
    use crate::{
        ArtifactSha256, DependencyRecord, PackageKind, RepositoryError, RepositoryErrorKind,
    };

    struct StubReader {
        summary: Result<Option<PackageSummaryRecord>, ()>,
        version: Result<Option<PackageVersionMetadataRecord>, ()>,
    }

    #[async_trait]
    impl PackageMetadataReader for StubReader {
        async fn package_summary_by_name(
            &self,
            _namespace: &IdentitySegment,
            _package: &IdentitySegment,
        ) -> Result<Option<PackageSummaryRecord>, RepositoryError> {
            self.summary
                .clone()
                .map_err(|()| RepositoryError::new(RepositoryErrorKind::Unavailable))
        }

        async fn package_version_metadata_by_name(
            &self,
            _namespace: &IdentitySegment,
            _package: &IdentitySegment,
            _version: &SemanticVersion,
        ) -> Result<Option<PackageVersionMetadataRecord>, RepositoryError> {
            self.version
                .clone()
                .map_err(|()| RepositoryError::new(RepositoryErrorKind::Unavailable))
        }
    }

    #[tokio::test]
    async fn metadata_validates_paths_and_sorts_dependencies() {
        let service = PackageMetadataService::new(Arc::new(StubReader {
            summary: Ok(Some(summary_fixture())),
            version: Ok(Some(version_fixture())),
        }));

        assert_eq!(
            service
                .package("rux", "example")
                .await
                .unwrap()
                .namespace
                .as_str(),
            "Rux"
        );
        let version = service
            .version("rux", "example", "1.0.0+linux")
            .await
            .unwrap();
        assert_eq!(version.dependencies[0].alias.as_str(), "Alpha");
        assert_eq!(
            service
                .package("bad namespace", "example")
                .await
                .unwrap_err()
                .kind(),
            PackageMetadataErrorKind::InvalidNamespace
        );
        assert_eq!(
            service
                .package("rux", "bad package")
                .await
                .unwrap_err()
                .kind(),
            PackageMetadataErrorKind::InvalidPackage
        );
        assert_eq!(
            service
                .version("rux", "example", "v1.0.0")
                .await
                .unwrap_err()
                .kind(),
            PackageMetadataErrorKind::InvalidVersion
        );
    }

    #[tokio::test]
    async fn metadata_maps_missing_and_repository_failures() {
        let missing = PackageMetadataService::new(Arc::new(StubReader {
            summary: Ok(None),
            version: Ok(None),
        }));
        assert_eq!(
            missing.package("rux", "missing").await.unwrap_err().kind(),
            PackageMetadataErrorKind::PackageNotFound
        );
        assert_eq!(
            missing
                .version("rux", "example", "1.0.0")
                .await
                .unwrap_err()
                .kind(),
            PackageMetadataErrorKind::PackageVersionNotFound
        );

        let unavailable = PackageMetadataService::new(Arc::new(StubReader {
            summary: Err(()),
            version: Err(()),
        }));
        assert_eq!(
            unavailable
                .package("rux", "example")
                .await
                .unwrap_err()
                .kind(),
            PackageMetadataErrorKind::Unavailable
        );
        assert_eq!(
            unavailable
                .version("rux", "example", "1.0.0")
                .await
                .unwrap_err()
                .kind(),
            PackageMetadataErrorKind::Unavailable
        );
    }

    fn summary_fixture() -> PackageSummaryRecord {
        PackageSummaryRecord {
            namespace: identity("Rux"),
            package: identity("Example"),
            created_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn version_fixture() -> PackageVersionMetadataRecord {
        PackageVersionMetadataRecord {
            namespace: identity("Rux"),
            package: identity("Example"),
            version: SemanticVersion::new("1.0.0+linux").unwrap(),
            manifest_schema_version: 1,
            min_rux: SemanticVersion::new("0.4.0").unwrap(),
            package_type: PackageKind::SourceLibrary,
            description: None,
            repository_url: None,
            homepage_url: None,
            readme_file: None,
            license_expression: None,
            license_file: None,
            normalized_manifest: Map::new(),
            artifact_sha256: ArtifactSha256::new([1; 32]),
            artifact_size: 1,
            artifact_file_count: 2,
            artifact_expanded_bytes: 1,
            source_file_count: 1,
            source_line_count: 0,
            published_at: OffsetDateTime::UNIX_EPOCH,
            yanked: false,
            authors: Vec::new(),
            keywords: Vec::new(),
            dependencies: vec![dependency("Zed"), dependency("Alpha")],
        }
    }

    fn dependency(alias: &str) -> DependencyRecord {
        DependencyRecord {
            alias: identity(alias),
            target_namespace: identity("Rux"),
            target_package: identity(alias),
            version_range: VersionRange::new("^1").unwrap(),
            target_os: Vec::new(),
        }
    }

    fn identity(value: &str) -> IdentitySegment {
        IdentitySegment::new(value).unwrap()
    }
}
