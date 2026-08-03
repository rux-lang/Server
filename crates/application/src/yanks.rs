use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use rux_domain::{IdentitySegment, SemanticVersion};

use crate::{
    ApiTokenErrorKind, AuditActor, AuditEvent, Clock, RegistryRepository, RegistryTransaction,
    TokenAuthorizer, WriteOutcome,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageVersionYankState {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub version: SemanticVersion,
    pub yanked: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum YankErrorKind {
    InvalidNamespace,
    InvalidPackage,
    InvalidVersion,
    AuthenticationRequired,
    InsufficientScope,
    Forbidden,
    PackageVersionNotFound,
    Unavailable,
}

#[derive(Debug)]
pub struct YankError {
    kind: YankErrorKind,
}

impl YankError {
    #[must_use]
    pub const fn new(kind: YankErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> YankErrorKind {
        self.kind
    }
}

impl fmt::Display for YankError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "package version yank operation failed: {:?}",
            self.kind
        )
    }
}

impl Error for YankError {}

#[async_trait]
pub trait Yanks: Send + Sync {
    async fn set_yanked(
        &self,
        credential: &str,
        namespace: &str,
        package: &str,
        version: &str,
        yanked: bool,
    ) -> Result<PackageVersionYankState, YankError>;
}

pub struct YankService {
    repository: Arc<dyn RegistryRepository>,
    token_authorizer: Arc<dyn TokenAuthorizer>,
    clock: Arc<dyn Clock>,
}

impl YankService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn RegistryRepository>,
        token_authorizer: Arc<dyn TokenAuthorizer>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            repository,
            token_authorizer,
            clock,
        }
    }

    async fn set_in_transaction(
        &self,
        transaction: &mut dyn RegistryTransaction,
        credential: &str,
        namespace: IdentitySegment,
        package: IdentitySegment,
        version: SemanticVersion,
        yanked: bool,
    ) -> Result<PackageVersionYankState, YankError> {
        let actor = self
            .token_authorizer
            .authorize_yank(transaction, credential)
            .await
            .map_err(|error| map_token_error(error.kind()))?;
        let namespace = transaction
            .lock_namespace_by_name(&namespace)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(not_found)?;
        transaction
            .lock_namespace_role(namespace.id, actor.user_id)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| YankError::new(YankErrorKind::Forbidden))?;
        let package = transaction
            .lock_package_by_name(&namespace.name, &package)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(not_found)?;
        let version_id = transaction
            .lock_version_id_by_name(&namespace.name, &package.name, &version)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(not_found)?;
        let yank = yanked.then(|| (self.clock.now(), actor.user_id));
        let outcome = transaction
            .set_yank(version_id, yank)
            .await
            .map_err(|_| unavailable())?;
        if outcome == WriteOutcome::NotFound {
            return Err(not_found());
        }
        if outcome == WriteOutcome::Applied {
            let audit_actor = AuditActor::token(actor.user_id, actor.id);
            let event = if yanked {
                AuditEvent::package_version_yanked(
                    audit_actor,
                    version_id,
                    &namespace.name,
                    &package.name,
                    &version,
                )
            } else {
                AuditEvent::package_version_unyanked(
                    audit_actor,
                    version_id,
                    &namespace.name,
                    &package.name,
                    &version,
                )
            };
            transaction
                .append_audit(&event)
                .await
                .map_err(|_| unavailable())?;
        }

        Ok(PackageVersionYankState {
            namespace: namespace.name,
            package: package.name,
            version,
            yanked,
        })
    }
}

#[async_trait]
impl Yanks for YankService {
    async fn set_yanked(
        &self,
        credential: &str,
        namespace: &str,
        package: &str,
        version: &str,
        yanked: bool,
    ) -> Result<PackageVersionYankState, YankError> {
        let namespace = IdentitySegment::new(namespace)
            .map_err(|_| YankError::new(YankErrorKind::InvalidNamespace))?;
        let package = IdentitySegment::new(package)
            .map_err(|_| YankError::new(YankErrorKind::InvalidPackage))?;
        let version = SemanticVersion::new(version)
            .map_err(|_| YankError::new(YankErrorKind::InvalidVersion))?;
        let mut transaction = self.repository.begin().await.map_err(|_| unavailable())?;
        let result = self
            .set_in_transaction(
                &mut *transaction,
                credential,
                namespace,
                package,
                version,
                yanked,
            )
            .await;
        match result {
            Ok(state) => {
                transaction.commit().await.map_err(|_| unavailable())?;
                Ok(state)
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }
}

fn map_token_error(kind: ApiTokenErrorKind) -> YankError {
    match kind {
        ApiTokenErrorKind::AuthenticationRequired => {
            YankError::new(YankErrorKind::AuthenticationRequired)
        }
        ApiTokenErrorKind::InsufficientScope => YankError::new(YankErrorKind::InsufficientScope),
        _ => unavailable(),
    }
}

const fn not_found() -> YankError {
    YankError::new(YankErrorKind::PackageVersionNotFound)
}

const fn unavailable() -> YankError {
    YankError::new(YankErrorKind::Unavailable)
}
