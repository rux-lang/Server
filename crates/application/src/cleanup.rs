use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rux_domain::{IdentitySegment, SemanticVersion};
use time::{Duration, OffsetDateTime};

use crate::{Clock, RepositoryError};

pub const PACKAGE_OBJECT_PREFIX: &str = "packages/";
pub const MAX_OBJECT_VERSION_PAGE_SIZE: u16 = 1_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectVersionCursor {
    pub key_marker: String,
    pub version_id_marker: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredObjectVersion {
    pub key: Option<String>,
    pub version_id: Option<String>,
    pub last_modified: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectVersionPage {
    pub versions: Vec<StoredObjectVersion>,
    pub next_cursor: Option<ObjectVersionCursor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectVersionStorageErrorKind {
    ListUnavailable,
    DeleteUnavailable,
    InvalidResponse,
}

#[derive(Debug)]
pub struct ObjectVersionStorageError {
    kind: ObjectVersionStorageErrorKind,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ObjectVersionStorageError {
    #[must_use]
    pub const fn new(kind: ObjectVersionStorageErrorKind) -> Self {
        Self { kind, source: None }
    }

    pub fn with_source(
        kind: ObjectVersionStorageErrorKind,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            source: Some(Box::new(source)),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ObjectVersionStorageErrorKind {
        self.kind
    }
}

impl fmt::Display for ObjectVersionStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "object-version storage failed: {:?}", self.kind)
    }
}

impl Error for ObjectVersionStorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[async_trait]
pub trait VersionedArtifactStorage: Send + Sync {
    async fn list_object_versions(
        &self,
        cursor: Option<&ObjectVersionCursor>,
        limit: u16,
    ) -> Result<ObjectVersionPage, ObjectVersionStorageError>;

    async fn delete_object_version(
        &self,
        key: &str,
        version_id: &str,
    ) -> Result<(), ObjectVersionStorageError>;
}

#[async_trait]
pub trait ArtifactReferenceReader: Send + Sync {
    async fn referenced_storage_keys(
        &self,
        keys: &[String],
    ) -> Result<Vec<String>, RepositoryError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrphanCleanupPolicy {
    minimum_age: Duration,
    scan_limit: u16,
    delete_limit: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrphanCleanupPolicyError {
    MinimumAge,
    ScanLimit,
    DeleteLimit,
}

impl fmt::Display for OrphanCleanupPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid orphan cleanup policy: {self:?}")
    }
}

impl Error for OrphanCleanupPolicyError {}

impl OrphanCleanupPolicy {
    /// Creates a bounded cleanup policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the age is not positive, the scan limit exceeds the provider bound,
    /// or the deletion limit is zero or greater than the scan limit.
    pub const fn new(
        minimum_age: Duration,
        scan_limit: u16,
        delete_limit: u16,
    ) -> Result<Self, OrphanCleanupPolicyError> {
        if !minimum_age.is_positive() {
            return Err(OrphanCleanupPolicyError::MinimumAge);
        }
        if scan_limit == 0 || scan_limit > MAX_OBJECT_VERSION_PAGE_SIZE {
            return Err(OrphanCleanupPolicyError::ScanLimit);
        }
        if delete_limit == 0 || delete_limit > scan_limit {
            return Err(OrphanCleanupPolicyError::DeleteLimit);
        }
        Ok(Self {
            minimum_age,
            scan_limit,
            delete_limit,
        })
    }

    #[must_use]
    pub const fn minimum_age(self) -> Duration {
        self.minimum_age
    }

    #[must_use]
    pub const fn scan_limit(self) -> u16 {
        self.scan_limit
    }

    #[must_use]
    pub const fn delete_limit(self) -> u16 {
        self.delete_limit
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrphanSweepResult {
    pub scanned: usize,
    pub recognizable: usize,
    pub old: usize,
    pub referenced: usize,
    pub delete_attempted: usize,
    pub deleted: usize,
    pub delete_failed: usize,
    pub next_cursor: Option<ObjectVersionCursor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrphanCleanupErrorKind {
    ListingUnavailable,
    ReferenceLookupUnavailable,
}

#[derive(Debug)]
pub struct OrphanCleanupError {
    kind: OrphanCleanupErrorKind,
}

impl OrphanCleanupError {
    #[must_use]
    pub const fn kind(&self) -> OrphanCleanupErrorKind {
        self.kind
    }
}

impl fmt::Display for OrphanCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "orphan cleanup failed: {:?}", self.kind)
    }
}

impl Error for OrphanCleanupError {}

pub struct OrphanCleanupService {
    storage: Arc<dyn VersionedArtifactStorage>,
    references: Arc<dyn ArtifactReferenceReader>,
    clock: Arc<dyn Clock>,
    policy: OrphanCleanupPolicy,
}

impl OrphanCleanupService {
    #[must_use]
    pub fn new(
        storage: Arc<dyn VersionedArtifactStorage>,
        references: Arc<dyn ArtifactReferenceReader>,
        clock: Arc<dyn Clock>,
        policy: OrphanCleanupPolicy,
    ) -> Self {
        Self {
            storage,
            references,
            clock,
            policy,
        }
    }

    /// Runs one bounded object-version sweep.
    ///
    /// # Errors
    ///
    /// Returns an availability category when listing or the fail-closed reference lookup fails.
    pub async fn sweep(
        &self,
        cursor: Option<&ObjectVersionCursor>,
    ) -> Result<OrphanSweepResult, OrphanCleanupError> {
        let page = self
            .storage
            .list_object_versions(cursor, self.policy.scan_limit())
            .await
            .map_err(|_| OrphanCleanupError {
                kind: OrphanCleanupErrorKind::ListingUnavailable,
            })?;
        let mut result = OrphanSweepResult {
            scanned: page.versions.len(),
            next_cursor: page.next_cursor,
            ..OrphanSweepResult::default()
        };
        let Some(cutoff) = self.clock.now().checked_sub(self.policy.minimum_age()) else {
            return Ok(result);
        };
        let mut candidates = Vec::new();

        for version in page.versions {
            let (Some(key), Some(version_id), Some(last_modified)) =
                (version.key, version.version_id, version.last_modified)
            else {
                continue;
            };
            if !recognizable_package_key(&key) {
                continue;
            }
            result.recognizable += 1;
            if last_modified > cutoff {
                continue;
            }
            result.old += 1;
            candidates.push((key, version_id));
        }

        if candidates.is_empty() {
            return Ok(result);
        }

        let mut keys = candidates
            .iter()
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.sort_unstable();
        keys.dedup();
        let referenced = self
            .references
            .referenced_storage_keys(&keys)
            .await
            .map_err(|_| OrphanCleanupError {
                kind: OrphanCleanupErrorKind::ReferenceLookupUnavailable,
            })?
            .into_iter()
            .collect::<HashSet<_>>();

        for (key, version_id) in candidates {
            if referenced.contains(&key) {
                result.referenced += 1;
                continue;
            }
            if result.delete_attempted >= usize::from(self.policy.delete_limit()) {
                break;
            }
            result.delete_attempted += 1;
            match self.storage.delete_object_version(&key, &version_id).await {
                Ok(()) => result.deleted += 1,
                Err(_) => result.delete_failed += 1,
            }
        }

        Ok(result)
    }
}

fn recognizable_package_key(key: &str) -> bool {
    let mut segments = key.split('/');
    let Some("packages") = segments.next() else {
        return false;
    };
    let (Some(namespace), Some(package), Some(version), Some(file_name)) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return false;
    };
    if segments.next().is_some() {
        return false;
    }
    if !canonical_identity(namespace) || !canonical_identity(package) {
        return false;
    }
    if SemanticVersion::new(version).is_err() {
        return false;
    }
    let Some(checksum) = file_name.strip_suffix(".ruxpkg") else {
        return false;
    };
    checksum.len() == 64
        && checksum
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_identity(value: &str) -> bool {
    IdentitySegment::new(value).is_ok_and(|identity| identity.normalized() == value)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;
    use crate::{RepositoryError, RepositoryErrorKind};

    const KEY: &str = concat!(
        "packages/rux-tools/example-pkg/1.2.3/",
        "abababababababababababababababababababababababababababababababab.ruxpkg"
    );

    struct FixedClock;

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            now()
        }
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::days(20_000)
    }

    fn old() -> OffsetDateTime {
        now() - Duration::hours(24)
    }

    fn young() -> OffsetDateTime {
        old() + Duration::seconds(1)
    }

    struct FakeStorage {
        page: Mutex<Option<Result<ObjectVersionPage, ObjectVersionStorageError>>>,
        deletes: Mutex<Vec<(String, String)>>,
        delete_results: Mutex<VecDeque<Result<(), ObjectVersionStorageError>>>,
        list_calls: Mutex<Vec<(Option<ObjectVersionCursor>, u16)>>,
    }

    impl FakeStorage {
        fn with_page(page: ObjectVersionPage) -> Self {
            Self {
                page: Mutex::new(Some(Ok(page))),
                deletes: Mutex::new(Vec::new()),
                delete_results: Mutex::new(VecDeque::new()),
                list_calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl VersionedArtifactStorage for FakeStorage {
        async fn list_object_versions(
            &self,
            cursor: Option<&ObjectVersionCursor>,
            limit: u16,
        ) -> Result<ObjectVersionPage, ObjectVersionStorageError> {
            self.list_calls
                .lock()
                .expect("list calls should not be poisoned")
                .push((cursor.cloned(), limit));
            self.page
                .lock()
                .expect("page should not be poisoned")
                .take()
                .expect("one list call is expected")
        }

        async fn delete_object_version(
            &self,
            key: &str,
            version_id: &str,
        ) -> Result<(), ObjectVersionStorageError> {
            self.deletes
                .lock()
                .expect("deletes should not be poisoned")
                .push((key.to_owned(), version_id.to_owned()));
            self.delete_results
                .lock()
                .expect("delete results should not be poisoned")
                .pop_front()
                .unwrap_or(Ok(()))
        }
    }

    struct FakeReferences {
        referenced: Vec<String>,
        fail: bool,
        calls: Mutex<Vec<Vec<String>>>,
    }

    #[async_trait]
    impl ArtifactReferenceReader for FakeReferences {
        async fn referenced_storage_keys(
            &self,
            keys: &[String],
        ) -> Result<Vec<String>, RepositoryError> {
            self.calls
                .lock()
                .expect("reference calls should not be poisoned")
                .push(keys.to_vec());
            if self.fail {
                Err(RepositoryError::new(RepositoryErrorKind::Unavailable))
            } else {
                Ok(self.referenced.clone())
            }
        }
    }

    fn version(
        key: Option<&str>,
        version_id: Option<&str>,
        at: Option<OffsetDateTime>,
    ) -> StoredObjectVersion {
        StoredObjectVersion {
            key: key.map(str::to_owned),
            version_id: version_id.map(str::to_owned),
            last_modified: at,
        }
    }

    fn policy(delete_limit: u16) -> OrphanCleanupPolicy {
        OrphanCleanupPolicy::new(Duration::hours(24), 1_000, delete_limit)
            .expect("test policy should be valid")
    }

    fn service(
        storage: Arc<FakeStorage>,
        references: Arc<FakeReferences>,
        delete_limit: u16,
    ) -> OrphanCleanupService {
        OrphanCleanupService::new(
            storage,
            references,
            Arc::new(FixedClock),
            policy(delete_limit),
        )
    }

    #[test]
    fn policy_enforces_provider_and_deletion_bounds() {
        assert!(OrphanCleanupPolicy::new(Duration::ZERO, 1_000, 100).is_err());
        assert!(OrphanCleanupPolicy::new(Duration::hours(24), 1_001, 100).is_err());
        assert!(OrphanCleanupPolicy::new(Duration::hours(24), 100, 101).is_err());
    }

    #[tokio::test]
    async fn sweep_deletes_only_old_recognizable_unreferenced_versions() {
        let other_key = KEY.replace("example-pkg", "other-pkg");
        let storage = Arc::new(FakeStorage::with_page(ObjectVersionPage {
            versions: vec![
                version(Some(KEY), Some("old-unreferenced"), Some(old())),
                version(Some(KEY), Some("young"), Some(young())),
                version(Some(&other_key), Some("referenced"), Some(old())),
                version(
                    Some(
                        "packages/Rux/bad/1.0.0/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ruxpkg",
                    ),
                    Some("bad-case"),
                    Some(old()),
                ),
                version(Some(KEY), None, Some(old())),
                version(None, Some("missing-key"), Some(old())),
            ],
            next_cursor: Some(ObjectVersionCursor {
                key_marker: "next-key".into(),
                version_id_marker: Some("next-version".into()),
            }),
        }));
        let references = Arc::new(FakeReferences {
            referenced: vec![other_key.clone()],
            fail: false,
            calls: Mutex::new(Vec::new()),
        });

        let result = service(storage.clone(), references.clone(), 100)
            .sweep(None)
            .await
            .expect("sweep should succeed");

        assert_eq!(result.scanned, 6);
        assert_eq!(result.recognizable, 3);
        assert_eq!(result.old, 2);
        assert_eq!(result.referenced, 1);
        assert_eq!(result.delete_attempted, 1);
        assert_eq!(result.deleted, 1);
        assert_eq!(result.delete_failed, 0);
        assert_eq!(
            storage
                .deletes
                .lock()
                .expect("deletes should not be poisoned")
                .as_slice(),
            &[(KEY.to_owned(), "old-unreferenced".to_owned())]
        );
        assert_eq!(
            references
                .calls
                .lock()
                .expect("calls should not be poisoned")
                .as_slice(),
            &[vec![KEY.to_owned(), other_key]]
        );
        assert_eq!(
            result.next_cursor,
            Some(ObjectVersionCursor {
                key_marker: "next-key".into(),
                version_id_marker: Some("next-version".into()),
            })
        );
    }

    #[tokio::test]
    async fn sweep_honors_delete_cap_and_continues_after_delete_failure() {
        let storage = Arc::new(FakeStorage::with_page(ObjectVersionPage {
            versions: vec![
                version(Some(KEY), Some("one"), Some(old())),
                version(Some(KEY), Some("two"), Some(old())),
                version(Some(KEY), Some("three"), Some(old())),
            ],
            next_cursor: None,
        }));
        storage
            .delete_results
            .lock()
            .expect("results should not be poisoned")
            .push_back(Err(ObjectVersionStorageError::new(
                ObjectVersionStorageErrorKind::DeleteUnavailable,
            )));
        let references = Arc::new(FakeReferences {
            referenced: Vec::new(),
            fail: false,
            calls: Mutex::new(Vec::new()),
        });

        let result = service(storage.clone(), references, 2)
            .sweep(None)
            .await
            .expect("sweep should succeed");

        assert_eq!(result.delete_attempted, 2);
        assert_eq!(result.deleted, 1);
        assert_eq!(result.delete_failed, 1);
        assert_eq!(
            storage
                .deletes
                .lock()
                .expect("deletes should not be poisoned")
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn reference_lookup_failure_is_fail_closed() {
        let storage = Arc::new(FakeStorage::with_page(ObjectVersionPage {
            versions: vec![version(Some(KEY), Some("candidate"), Some(old()))],
            next_cursor: None,
        }));
        let references = Arc::new(FakeReferences {
            referenced: Vec::new(),
            fail: true,
            calls: Mutex::new(Vec::new()),
        });

        let error = service(storage.clone(), references, 100)
            .sweep(None)
            .await
            .expect_err("reference failure should fail the sweep");

        assert_eq!(
            error.kind(),
            OrphanCleanupErrorKind::ReferenceLookupUnavailable
        );
        assert!(
            storage
                .deletes
                .lock()
                .expect("deletes should not be poisoned")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn listing_receives_cursor_and_configured_bound() {
        let cursor = ObjectVersionCursor {
            key_marker: "key".into(),
            version_id_marker: Some("version".into()),
        };
        let storage = Arc::new(FakeStorage::with_page(ObjectVersionPage {
            versions: Vec::new(),
            next_cursor: None,
        }));
        let references = Arc::new(FakeReferences {
            referenced: Vec::new(),
            fail: false,
            calls: Mutex::new(Vec::new()),
        });

        service(storage.clone(), references, 100)
            .sweep(Some(&cursor))
            .await
            .expect("sweep should succeed");

        assert_eq!(
            storage
                .list_calls
                .lock()
                .expect("list calls should not be poisoned")
                .as_slice(),
            &[(Some(cursor), 1_000)]
        );
    }
}
