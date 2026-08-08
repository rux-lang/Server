use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rux_domain::{IdentitySegment, SemanticVersion};
use time::OffsetDateTime;

use crate::{
    ApiTokenErrorKind, ArtifactSha256, AuditActor, AuditEvent, BlockedIdentityKind,
    DependencyRecord, JsonObject, NamespaceRecord, NewPackageVersion, PackageKind, PackageRecord,
    RegistryRepository, RegistryTransaction, RepositoryConflict, RepositoryError,
    RepositoryErrorKind, TokenAuthorizer,
};

#[derive(Clone, Debug)]
pub struct PublicationMetadata {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub version: SemanticVersion,
    pub manifest_schema_version: u16,
    pub min_rux: SemanticVersion,
    pub package_type: PackageKind,
    pub description: Option<String>,
    pub repository_url: Option<String>,
    pub homepage_url: Option<String>,
    pub readme_file: Option<(String, String)>,
    pub license_expression: Option<String>,
    pub license_file: Option<(String, String)>,
    pub normalized_manifest: JsonObject,
    pub artifact_file_count: u32,
    pub artifact_expanded_bytes: u64,
    pub source_file_count: u32,
    pub source_line_count: u64,
    pub authors: Vec<String>,
    pub keywords: Vec<IdentitySegment>,
    pub dependencies: Vec<DependencyRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredArtifact {
    pub sha256: ArtifactSha256,
    pub byte_size: u64,
    pub storage_key: String,
}

/// A validated, process-local artifact ready for durable object storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactUpload {
    pub path: PathBuf,
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub version: SemanticVersion,
    pub sha256: ArtifactSha256,
    pub byte_size: u64,
}

/// An operational object-storage failure whose provider details stay internal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArtifactStorageErrorKind {
    SourceUnavailable,
    UploadUnavailable,
    ChecksumMismatch,
}

#[derive(Debug)]
pub struct ArtifactStorageError {
    kind: ArtifactStorageErrorKind,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl ArtifactStorageError {
    #[must_use]
    pub const fn new(kind: ArtifactStorageErrorKind) -> Self {
        Self { kind, source: None }
    }

    pub fn with_source(
        kind: ArtifactStorageErrorKind,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            source: Some(Box::new(source)),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ArtifactStorageErrorKind {
        self.kind
    }
}

impl fmt::Display for ArtifactStorageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "artifact storage failed: {:?}", self.kind)
    }
}

impl Error for ArtifactStorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[async_trait]
pub trait ArtifactStorage: Send + Sync {
    async fn store(&self, artifact: ArtifactUpload)
    -> Result<StoredArtifact, ArtifactStorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublishedPackageVersion {
    pub namespace: IdentitySegment,
    pub package: IdentitySegment,
    pub version: SemanticVersion,
    pub published_at: OffsetDateTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationErrorKind {
    AuthenticationRequired,
    InsufficientScope,
    NamespaceNotFound,
    Forbidden,
    NamespaceBlocked,
    PackageBlocked,
    VersionConflict,
    Unavailable,
}

#[derive(Debug)]
pub struct PublicationError {
    kind: PublicationErrorKind,
}

impl PublicationError {
    #[must_use]
    pub const fn new(kind: PublicationErrorKind) -> Self {
        Self { kind }
    }

    #[must_use]
    pub const fn kind(&self) -> PublicationErrorKind {
        self.kind
    }
}

impl fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "publication failed: {:?}", self.kind)
    }
}

impl Error for PublicationError {}

#[async_trait]
pub trait Publications: Send + Sync {
    async fn publish(
        &self,
        credential: &str,
        metadata: PublicationMetadata,
        artifact: ArtifactUpload,
    ) -> Result<PublishedPackageVersion, PublicationError>;
}

pub struct PublicationService {
    repository: Arc<dyn RegistryRepository>,
    token_authorizer: Arc<dyn TokenAuthorizer>,
}

impl PublicationService {
    #[must_use]
    pub fn new(
        repository: Arc<dyn RegistryRepository>,
        token_authorizer: Arc<dyn TokenAuthorizer>,
    ) -> Self {
        Self {
            repository,
            token_authorizer,
        }
    }

    async fn validate_prepared_transaction(
        &self,
        transaction: &mut dyn RegistryTransaction,
        credential: &str,
        metadata: &PublicationMetadata,
    ) -> Result<
        (
            crate::AuthorizedApiToken,
            NamespaceRecord,
            Option<PackageRecord>,
        ),
        PublicationError,
    > {
        let actor = self
            .token_authorizer
            .authorize_publish(transaction, credential)
            .await
            .map_err(|error| map_token_error(error.kind()))?;
        let namespace = transaction
            .lock_namespace_by_name(&metadata.namespace)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| PublicationError::new(PublicationErrorKind::NamespaceNotFound))?;
        transaction
            .lock_namespace_role(namespace.id, actor.user_id)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(|| PublicationError::new(PublicationErrorKind::Forbidden))?;
        if blocked(
            transaction,
            BlockedIdentityKind::Namespace,
            &metadata.namespace,
        )
        .await?
        {
            return Err(PublicationError::new(
                PublicationErrorKind::NamespaceBlocked,
            ));
        }
        if blocked(transaction, BlockedIdentityKind::Package, &metadata.package).await? {
            return Err(PublicationError::new(PublicationErrorKind::PackageBlocked));
        }
        let package = transaction
            .lock_package_by_name(&metadata.namespace, &metadata.package)
            .await
            .map_err(|_| unavailable())?;
        if package.is_some()
            && transaction
                .lock_version_id_by_name(&metadata.namespace, &metadata.package, &metadata.version)
                .await
                .map_err(|_| unavailable())?
                .is_some()
        {
            return Err(PublicationError::new(PublicationErrorKind::VersionConflict));
        }
        Ok((actor, namespace, package))
    }

    /// Prepares and locks an immutable publication before its artifact is uploaded.
    ///
    /// # Errors
    ///
    /// Returns a stable authorization, policy, conflict, or availability category.
    pub async fn prepare(
        &self,
        credential: &str,
        metadata: PublicationMetadata,
    ) -> Result<PreparedPublication, PublicationError> {
        let mut transaction = self.repository.begin().await.map_err(|_| unavailable())?;
        let (actor, namespace, package) = match self
            .validate_prepared_transaction(&mut *transaction, credential, &metadata)
            .await
        {
            Ok(prepared) => prepared,
            Err(error) => return rollback_error(transaction, error).await,
        };

        Ok(PreparedPublication {
            transaction,
            metadata,
            namespace,
            package,
            actor,
        })
    }
}

pub struct PreparedPublication {
    transaction: Box<dyn RegistryTransaction>,
    metadata: PublicationMetadata,
    namespace: NamespaceRecord,
    package: Option<PackageRecord>,
    actor: crate::AuthorizedApiToken,
}

impl PreparedPublication {
    /// Persists the publication after its artifact has been durably stored.
    ///
    /// # Errors
    ///
    /// Returns a stable conflict or availability category. Every handled failure rolls back the
    /// prepared database transaction.
    pub async fn complete(
        self,
        artifact: StoredArtifact,
    ) -> Result<PublishedPackageVersion, PublicationError> {
        let Self {
            mut transaction,
            metadata,
            namespace,
            package,
            actor,
        } = self;

        let namespace_name = namespace.name;
        let submitted_package = metadata.package.clone();
        let submitted_version = metadata.version.clone();

        let package = match package {
            Some(package) => package,
            None => match transaction
                .create_package(namespace.id, &metadata.package, Some(actor.user_id))
                .await
            {
                Ok(package) => package,
                Err(error) => return rollback_error(transaction, map_write_error(&error)).await,
            },
        };

        let new_version = NewPackageVersion {
            package_id: package.id,
            version: metadata.version.clone(),
            manifest_schema_version: metadata.manifest_schema_version,
            min_rux: metadata.min_rux,
            package_type: metadata.package_type,
            description: metadata.description,
            repository_url: metadata.repository_url,
            homepage_url: metadata.homepage_url,
            readme_file: metadata.readme_file,
            license_expression: metadata.license_expression,
            license_file: metadata.license_file,
            normalized_manifest: metadata.normalized_manifest,
            artifact_sha256: artifact.sha256,
            artifact_size: artifact.byte_size,
            storage_key: artifact.storage_key,
            artifact_file_count: metadata.artifact_file_count,
            artifact_expanded_bytes: metadata.artifact_expanded_bytes,
            source_file_count: metadata.source_file_count,
            source_line_count: metadata.source_line_count,
            published_by_user_id: Some(actor.user_id),
            authors: metadata.authors,
            keywords: metadata.keywords,
            dependencies: metadata.dependencies,
        };
        let version = match transaction.create_package_version(&new_version).await {
            Ok(version) => version,
            Err(error) => return rollback_error(transaction, map_write_error(&error)).await,
        };
        let audit_actor = AuditActor::token(actor.user_id, actor.id);
        if transaction
            .append_audit(&AuditEvent::package_version_published(
                audit_actor,
                version.id,
                &namespace_name,
                &submitted_package,
                &submitted_version,
            ))
            .await
            .is_err()
        {
            return rollback_error(transaction, unavailable()).await;
        }
        if transaction.commit().await.is_err() {
            return Err(unavailable());
        }

        Ok(PublishedPackageVersion {
            namespace: namespace_name,
            package: package.name,
            version: version.version,
            published_at: version.published_at,
        })
    }

    /// Explicitly rolls back a publication when artifact storage fails.
    ///
    /// # Errors
    ///
    /// Returns `Unavailable` when `PostgreSQL` cannot complete the rollback.
    pub async fn abort(self) -> Result<(), PublicationError> {
        self.transaction.rollback().await.map_err(|_| unavailable())
    }
}

/// Composes prepared `PostgreSQL` publication with durable artifact storage.
pub struct PublicationWorkflow {
    publications: Arc<PublicationService>,
    storage: Arc<dyn ArtifactStorage>,
}

impl PublicationWorkflow {
    #[must_use]
    pub fn new(publications: Arc<PublicationService>, storage: Arc<dyn ArtifactStorage>) -> Self {
        Self {
            publications,
            storage,
        }
    }
}

#[async_trait]
impl Publications for PublicationWorkflow {
    async fn publish(
        &self,
        credential: &str,
        metadata: PublicationMetadata,
        artifact: ArtifactUpload,
    ) -> Result<PublishedPackageVersion, PublicationError> {
        let prepared = self.publications.prepare(credential, metadata).await?;
        let Ok(stored) = self.storage.store(artifact).await else {
            prepared.abort().await?;
            return Err(unavailable());
        };
        prepared.complete(stored).await
    }
}

async fn blocked(
    transaction: &mut dyn RegistryTransaction,
    kind: BlockedIdentityKind,
    name: &IdentitySegment,
) -> Result<bool, PublicationError> {
    transaction
        .is_identity_blocked(kind, name)
        .await
        .map_err(|_| unavailable())
}

async fn rollback_error<T>(
    transaction: Box<dyn RegistryTransaction>,
    error: PublicationError,
) -> Result<T, PublicationError> {
    let _ = transaction.rollback().await;
    Err(error)
}

fn map_token_error(kind: ApiTokenErrorKind) -> PublicationError {
    match kind {
        ApiTokenErrorKind::AuthenticationRequired => {
            PublicationError::new(PublicationErrorKind::AuthenticationRequired)
        }
        ApiTokenErrorKind::InsufficientScope => {
            PublicationError::new(PublicationErrorKind::InsufficientScope)
        }
        _ => unavailable(),
    }
}

fn map_write_error(error: &RepositoryError) -> PublicationError {
    if matches!(
        error.kind(),
        RepositoryErrorKind::Conflict(RepositoryConflict::PackageVersion)
    ) {
        PublicationError::new(PublicationErrorKind::VersionConflict)
    } else {
        unavailable()
    }
}

const fn unavailable() -> PublicationError {
    PublicationError::new(PublicationErrorKind::Unavailable)
}
