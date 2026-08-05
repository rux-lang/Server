#![doc = "Application ports and use cases for the Rux package registry."]

mod accounts;
mod audit;
mod authentication;
mod cleanup;
mod dashboard;
mod discovery;
mod downloads;
mod metadata;
mod namespaces;
mod persistence;
mod playground;
mod publication;
mod resolver;
mod search;
mod tokens;
mod yanks;

pub use accounts::*;
pub use audit::*;
pub use authentication::*;
pub use cleanup::*;
pub use dashboard::*;
pub use discovery::*;
pub use downloads::*;
pub use metadata::*;
pub use namespaces::*;
pub use persistence::*;
pub use playground::*;
pub use publication::*;
pub use resolver::*;
pub use search::*;
pub use tokens::*;
pub use yanks::*;

use async_trait::async_trait;
use std::time::Duration;

/// Readiness of one external dependency.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyStatus {
    /// Stable name used in health responses and telemetry.
    pub name: &'static str,
    /// Whether the dependency can serve normal traffic.
    pub healthy: bool,
    /// Time spent checking this dependency.
    pub duration: Duration,
}

/// Checks dependencies without leaking adapter errors.
#[async_trait]
pub trait DependencyProbe: Send + Sync {
    /// Checks everything required to serve normal API traffic.
    async fn readiness(&self) -> Vec<DependencyStatus>;
}
