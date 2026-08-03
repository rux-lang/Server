use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rux_domain::{IdentitySegment, SemanticVersion};

use crate::{Clock, DownloadRepository, DownloadTargetRecord};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DownloadTarget {
    pub storage_key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DownloadErrorKind {
    InvalidNamespace,
    InvalidPackage,
    InvalidVersion,
    PackageVersionNotFound,
    Unavailable,
}

#[derive(Debug)]
pub struct DownloadError {
    kind: DownloadErrorKind,
}

impl DownloadError {
    #[must_use]
    pub const fn new(kind: DownloadErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> DownloadErrorKind {
        self.kind
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "download operation failed: {:?}", self.kind)
    }
}

impl Error for DownloadError {}

#[async_trait]
pub trait Downloads: Send + Sync {
    async fn download(
        &self,
        namespace: &str,
        package: &str,
        version: &str,
    ) -> Result<DownloadTarget, DownloadError>;

    async fn resolve(
        &self,
        namespace: &str,
        package: &str,
        version: &str,
    ) -> Result<DownloadTarget, DownloadError>;
}

pub struct DownloadService {
    repository: Arc<dyn DownloadRepository>,
    clock: Arc<dyn Clock>,
}

impl DownloadService {
    #[must_use]
    pub fn new(repository: Arc<dyn DownloadRepository>, clock: Arc<dyn Clock>) -> Self {
        Self { repository, clock }
    }

    async fn target(
        &self,
        namespace: &str,
        package: &str,
        version: &str,
    ) -> Result<DownloadTargetRecord, DownloadError> {
        let namespace = IdentitySegment::new(namespace)
            .map_err(|_| DownloadError::new(DownloadErrorKind::InvalidNamespace))?;
        let package = IdentitySegment::new(package)
            .map_err(|_| DownloadError::new(DownloadErrorKind::InvalidPackage))?;
        let version = SemanticVersion::new(version)
            .map_err(|_| DownloadError::new(DownloadErrorKind::InvalidVersion))?;

        self.repository
            .download_target_by_name(&namespace, &package, &version)
            .await
            .map_err(|_| DownloadError::new(DownloadErrorKind::Unavailable))?
            .ok_or_else(|| DownloadError::new(DownloadErrorKind::PackageVersionNotFound))
    }
}

#[async_trait]
impl Downloads for DownloadService {
    async fn download(
        &self,
        namespace: &str,
        package: &str,
        version: &str,
    ) -> Result<DownloadTarget, DownloadError> {
        let target = self.target(namespace, package, version).await?;
        let mut transaction = self
            .repository
            .begin_download()
            .await
            .map_err(|_| DownloadError::new(DownloadErrorKind::Unavailable))?;
        if transaction
            .append_download(target.package_version_id, self.clock.now())
            .await
            .is_err()
        {
            let _ = transaction.rollback().await;
            return Err(DownloadError::new(DownloadErrorKind::Unavailable));
        }
        transaction
            .commit()
            .await
            .map_err(|_| DownloadError::new(DownloadErrorKind::Unavailable))?;

        Ok(DownloadTarget {
            storage_key: target.storage_key,
        })
    }

    async fn resolve(
        &self,
        namespace: &str,
        package: &str,
        version: &str,
    ) -> Result<DownloadTarget, DownloadError> {
        let target = self.target(namespace, package, version).await?;
        Ok(DownloadTarget {
            storage_key: target.storage_key,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use uuid::Uuid;

    use super::*;
    use crate::{
        DownloadReader, DownloadTransaction, DownloadUnitOfWork, DownloadWriter, PackageVersionId,
        RepositoryError, RepositoryErrorKind,
    };
    use time::OffsetDateTime;

    #[derive(Default)]
    struct TransactionState {
        appended: Vec<(PackageVersionId, OffsetDateTime)>,
        commits: usize,
        rollbacks: usize,
        begins: usize,
    }

    struct StubRepository {
        target: Result<Option<DownloadTargetRecord>, ()>,
        begin_fails: bool,
        append_fails: bool,
        commit_fails: bool,
        state: Arc<Mutex<TransactionState>>,
    }

    #[async_trait]
    impl DownloadReader for StubRepository {
        async fn download_target_by_name(
            &self,
            _namespace: &IdentitySegment,
            _package: &IdentitySegment,
            _version: &SemanticVersion,
        ) -> Result<Option<DownloadTargetRecord>, RepositoryError> {
            self.target
                .clone()
                .map_err(|()| RepositoryError::new(RepositoryErrorKind::Unavailable))
        }
    }

    #[async_trait]
    impl DownloadUnitOfWork for StubRepository {
        async fn begin_download(&self) -> Result<Box<dyn DownloadTransaction>, RepositoryError> {
            if self.begin_fails {
                return Err(RepositoryError::new(RepositoryErrorKind::Unavailable));
            }
            self.state.lock().unwrap().begins += 1;
            Ok(Box::new(StubTransaction {
                append_fails: self.append_fails,
                commit_fails: self.commit_fails,
                state: self.state.clone(),
            }))
        }
    }

    struct StubTransaction {
        append_fails: bool,
        commit_fails: bool,
        state: Arc<Mutex<TransactionState>>,
    }

    #[async_trait]
    impl DownloadWriter for StubTransaction {
        async fn append_download(
            &mut self,
            version_id: PackageVersionId,
            at: OffsetDateTime,
        ) -> Result<(), RepositoryError> {
            if self.append_fails {
                return Err(RepositoryError::new(RepositoryErrorKind::Unavailable));
            }
            self.state.lock().unwrap().appended.push((version_id, at));
            Ok(())
        }
    }

    #[async_trait]
    impl DownloadTransaction for StubTransaction {
        async fn commit(self: Box<Self>) -> Result<(), RepositoryError> {
            if self.commit_fails {
                return Err(RepositoryError::new(RepositoryErrorKind::Unavailable));
            }
            self.state.lock().unwrap().commits += 1;
            Ok(())
        }

        async fn rollback(self: Box<Self>) -> Result<(), RepositoryError> {
            self.state.lock().unwrap().rollbacks += 1;
            Ok(())
        }
    }

    struct FixedClock(OffsetDateTime);

    impl Clock for FixedClock {
        fn now(&self) -> OffsetDateTime {
            self.0
        }
    }

    #[tokio::test]
    async fn get_records_and_commits_before_returning_the_target() {
        let state = Arc::new(Mutex::new(TransactionState::default()));
        let service = service(repository(Ok(Some(target())), state.clone()));

        let result = service.download("rux", "example", "1.0.0").await.unwrap();

        assert_eq!(result.storage_key, "packages/rux/example/object.ruxpkg");
        let state = state.lock().unwrap();
        assert_eq!(
            state.appended,
            [(PackageVersionId::new(Uuid::from_u128(7)), timestamp())]
        );
        assert_eq!(state.commits, 1);
        assert_eq!(state.begins, 1);
    }

    #[tokio::test]
    async fn head_resolves_without_opening_a_transaction() {
        let state = Arc::new(Mutex::new(TransactionState::default()));
        let service = service(repository(Ok(Some(target())), state.clone()));

        assert_eq!(
            service.resolve("rux", "example", "1.0.0").await.unwrap(),
            DownloadTarget {
                storage_key: "packages/rux/example/object.ruxpkg".into()
            }
        );
        assert_eq!(state.lock().unwrap().begins, 0);
    }

    #[tokio::test]
    async fn validation_and_missing_targets_have_stable_errors() {
        let found = service(repository(
            Ok(Some(target())),
            Arc::new(Mutex::new(TransactionState::default())),
        ));
        assert_eq!(
            found
                .download("bad namespace", "example", "1.0.0")
                .await
                .unwrap_err()
                .kind(),
            DownloadErrorKind::InvalidNamespace
        );
        assert_eq!(
            found
                .download("rux", "bad package", "1.0.0")
                .await
                .unwrap_err()
                .kind(),
            DownloadErrorKind::InvalidPackage
        );
        assert_eq!(
            found
                .download("rux", "example", "v1.0.0")
                .await
                .unwrap_err()
                .kind(),
            DownloadErrorKind::InvalidVersion
        );
        let missing = service(repository(
            Ok(None),
            Arc::new(Mutex::new(TransactionState::default())),
        ));
        assert_eq!(
            missing
                .download("rux", "example", "1.0.0")
                .await
                .unwrap_err()
                .kind(),
            DownloadErrorKind::PackageVersionNotFound
        );
    }

    #[tokio::test]
    async fn repository_and_transaction_failures_are_unavailable() {
        let unavailable = service(repository(
            Err(()),
            Arc::new(Mutex::new(TransactionState::default())),
        ));
        assert_eq!(
            unavailable
                .download("rux", "example", "1.0.0")
                .await
                .unwrap_err()
                .kind(),
            DownloadErrorKind::Unavailable
        );

        for failure in ["begin", "append", "commit"] {
            let state = Arc::new(Mutex::new(TransactionState::default()));
            let mut repository = repository(Ok(Some(target())), state.clone());
            repository.begin_fails = failure == "begin";
            repository.append_fails = failure == "append";
            repository.commit_fails = failure == "commit";
            let service = service(repository);
            assert_eq!(
                service
                    .download("rux", "example", "1.0.0")
                    .await
                    .unwrap_err()
                    .kind(),
                DownloadErrorKind::Unavailable
            );
            if failure == "append" {
                assert_eq!(state.lock().unwrap().rollbacks, 1);
            }
        }
    }

    fn service(repository: StubRepository) -> DownloadService {
        DownloadService::new(Arc::new(repository), Arc::new(FixedClock(timestamp())))
    }

    fn repository(
        target: Result<Option<DownloadTargetRecord>, ()>,
        state: Arc<Mutex<TransactionState>>,
    ) -> StubRepository {
        StubRepository {
            target,
            begin_fails: false,
            append_fails: false,
            commit_fails: false,
            state,
        }
    }

    fn target() -> DownloadTargetRecord {
        DownloadTargetRecord {
            package_version_id: PackageVersionId::new(Uuid::from_u128(7)),
            storage_key: "packages/rux/example/object.ruxpkg".into(),
        }
    }

    const fn timestamp() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }
}
