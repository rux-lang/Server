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

async fn live() -> Json<HealthDocument> {
    Json(HealthDocument {
        status: "healthy",
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
    use super::*;

    #[tokio::test]
    async fn liveness_has_no_dependency_checks() {
        let Json(document) = live().await;
        assert_eq!(document.status, "healthy");
        assert!(document.checks.is_empty());
    }
}
