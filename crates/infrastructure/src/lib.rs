#![doc = "Infrastructure adapters for the Rux package registry."]

use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::Client as S3Client;
use rux_application::{DependencyProbe, DependencyStatus};
use sqlx::postgres::PgPoolOptions;
use sqlx::query;
use std::fmt;
use std::time::Instant;
use thiserror::Error;

mod authentication;
mod object_storage;
mod playground;
mod postgres;

pub use authentication::{
    GitHubOAuthClient, GitHubOAuthConfig, OsCredentialGenerator, SystemClock,
};
pub use object_storage::SpacesArtifactStorage;
/// The playground adapter is constructed on its own rather than as part of
/// [`Infrastructure`]: the registry must boot without it, and it is
/// deliberately absent from [`DependencyProbe`] so a stopped sandbox cannot
/// fail `/health/ready` and pull the registry out of rotation.
pub use playground::UnixSocketPlayground;
pub use postgres::PostgresRepository;

/// Environment-backed infrastructure configuration.
#[derive(Clone)]
pub struct InfrastructureConfig {
    pub database_url: String,
    pub storage_endpoint: String,
    pub storage_bucket: String,
    pub storage_access_key: String,
    pub storage_secret_key: String,
    pub storage_region: String,
    pub storage_force_path_style: bool,
}

/// Failures that prevent infrastructure startup.
#[derive(Error)]
pub enum InfrastructureError {
    #[error("database pool could not be configured")]
    Database(
        #[from]
        #[source]
        sqlx::Error,
    ),
}

impl fmt::Debug for InfrastructureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(_) => formatter.write_str("InfrastructureError::Database"),
        }
    }
}

/// Shared production adapters used by API handlers.
#[derive(Clone)]
pub struct Infrastructure {
    repository: PostgresRepository,
    storage: S3Client,
    storage_bucket: String,
}

impl Infrastructure {
    /// Creates lazy clients. Readiness reports connectivity.
    ///
    /// # Errors
    ///
    /// Returns an error when the database URL cannot configure a lazy pool.
    pub async fn new(config: InfrastructureConfig) -> Result<Self, InfrastructureError> {
        let database = PgPoolOptions::new()
            .max_connections(10)
            .connect_lazy(&config.database_url)?;

        let credentials = Credentials::new(
            config.storage_access_key,
            config.storage_secret_key,
            None,
            None,
            "rux-server-environment",
        );
        let mut storage_loader = aws_config::defaults(BehaviorVersion::latest())
            .credentials_provider(credentials)
            .region(aws_config::Region::new(config.storage_region))
            .endpoint_url(&config.storage_endpoint);
        if config.storage_endpoint.starts_with("http://") {
            storage_loader =
                storage_loader.http_client(aws_smithy_http_client::Builder::new().build_http());
        }
        let shared = storage_loader.load().await;
        let storage_config = aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(config.storage_force_path_style)
            .build();

        Ok(Self {
            repository: PostgresRepository::new(database),
            storage: S3Client::from_conf(storage_config),
            storage_bucket: config.storage_bucket,
        })
    }

    #[must_use]
    pub const fn repository(&self) -> &PostgresRepository {
        &self.repository
    }

    #[must_use]
    pub fn artifact_storage(&self) -> SpacesArtifactStorage {
        SpacesArtifactStorage::new(self.storage.clone(), self.storage_bucket.clone())
    }
}

#[async_trait]
impl DependencyProbe for Infrastructure {
    async fn readiness(&self) -> Vec<DependencyStatus> {
        let database_started = Instant::now();
        let database = query("SELECT 1")
            .execute(self.repository.pool())
            .await
            .is_ok();
        let database_duration = database_started.elapsed();
        let storage_started = Instant::now();
        let storage = self
            .storage
            .head_bucket()
            .bucket(&self.storage_bucket)
            .send()
            .await
            .is_ok();
        let storage_duration = storage_started.elapsed();

        vec![
            DependencyStatus {
                name: "postgresql",
                healthy: database,
                duration: database_duration,
            },
            DependencyStatus {
                name: "object_storage",
                healthy: storage,
                duration: storage_duration,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_errors_do_not_format_secret_bearing_sources() {
        let source = std::io::Error::other("postgres://user:secret@private-host/registry");
        let error = InfrastructureError::Database(sqlx::Error::Configuration(Box::new(source)));

        for formatted in [format!("{error}"), format!("{error:?}")] {
            assert!(!formatted.contains("secret"));
            assert!(!formatted.contains("private-host"));
        }
    }
}
