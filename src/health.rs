use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use rux_application::DependencyProbe;
use serde::Serialize;

#[derive(Clone)]
struct HealthState {
    probe: Arc<dyn DependencyProbe>,
}

/// The build this process was compiled from, so an operator can tell which
/// binary is answering without shelling into the host. Compile-time, because a
/// liveness probe must not do work to answer.
const BUILD_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Liveness answers "which process is this, and is it up". It carries the build
/// version; readiness deliberately does not, because that document is polled as
/// a dependency verdict and repeating the version in it would invite treating
/// two documents as one contract.
#[derive(Serialize)]
struct LiveDocument {
    status: &'static str,
    version: &'static str,
    checks: Vec<HealthCheckDocument>,
}

#[derive(Serialize)]
struct HealthDocument {
    status: &'static str,
    checks: Vec<HealthCheckDocument>,
}

#[derive(Serialize)]
struct HealthCheckDocument {
    name: &'static str,
    status: &'static str,
}

pub fn router(probe: Arc<dyn DependencyProbe>) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .with_state(HealthState { probe })
}

async fn live() -> Json<LiveDocument> {
    Json(LiveDocument {
        status: "healthy",
        version: BUILD_VERSION,
        checks: Vec::new(),
    })
}

async fn ready(State(state): State<HealthState>) -> impl IntoResponse {
    let checks = state.probe.readiness().await;
    let healthy = checks.iter().all(|check| check.healthy);
    let document = HealthDocument {
        status: if healthy { "healthy" } else { "unhealthy" },
        checks: checks
            .into_iter()
            .map(|check| HealthCheckDocument {
                name: check.name,
                status: if check.healthy {
                    "healthy"
                } else {
                    "unhealthy"
                },
            })
            .collect(),
    };

    (
        if healthy {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(document),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use rux_application::DependencyStatus;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    struct StubProbe;

    #[async_trait]
    impl DependencyProbe for StubProbe {
        async fn readiness(&self) -> Vec<DependencyStatus> {
            vec![DependencyStatus {
                name: "postgresql",
                healthy: true,
                duration: Duration::ZERO,
            }]
        }
    }

    #[tokio::test]
    async fn liveness_has_no_dependency_checks() {
        let Json(document) = live().await;
        assert_eq!(document.status, "healthy");
        assert!(document.checks.is_empty());
    }

    #[tokio::test]
    async fn liveness_reports_the_build_version() {
        let Json(document) = live().await;
        assert_eq!(document.version, env!("CARGO_PKG_VERSION"));
        // A blank version would still serialize and still pass an equality
        // check against itself, so assert the field carries something.
        assert!(!document.version.is_empty());
    }

    // The handler test above proves the value; this proves the wire name, which
    // is the half an operator's tooling actually depends on.
    #[tokio::test]
    async fn liveness_serializes_the_version_field() {
        let response = router(Arc::new(StubProbe))
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let document: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(document["status"], "healthy");
        assert_eq!(document["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn readiness_does_not_report_a_version() {
        let response = router(Arc::new(StubProbe))
            .oneshot(
                Request::builder()
                    .uri("/health/ready")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let document: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert!(document.get("version").is_none());
    }
}
