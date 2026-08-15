use std::time::Duration;

use axum::extract::{MatchedPath, Request};
use axum::http::Response;
use tracing::{Span, info, info_span};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Installs the process-wide structured logging subscriber.
///
/// Both binaries share this: the API and the playground broker emit the same
/// JSON shape, so one log pipeline reads both.
///
/// # Errors
///
/// Returns an error when a global subscriber has already been installed.
pub fn init() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_current_span(true)
                .with_span_list(true),
        )
        .try_init()?;
    Ok(())
}

#[must_use]
pub fn request_span(request: &Request) -> Span {
    let route = matched_route(request);
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("<missing>");
    info_span!(
        "http_request",
        request_id,
        method = %request.method(),
        route = %route,
        status_code = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
    )
}

pub fn record_response<B>(response: &Response<B>, latency: Duration, span: &Span) {
    span.record("status_code", response.status().as_u16());
    span.record("duration_ms", latency.as_secs_f64() * 1_000.0);
    info!(
        parent: span,
        status_code = response.status().as_u16(),
        duration_ms = latency.as_secs_f64() * 1_000.0,
        "HTTP request completed"
    );
}

/// The route template rather than the concrete path, so a logged route never
/// carries a package name, a token, or any other submitted value.
fn matched_route(request: &Request) -> String {
    request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "<unmatched>".to_owned(), |path| path.as_str().to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::middleware;
    use axum::routing::get;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn logged_routes_are_templates_rather_than_submitted_values() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let app = Router::new()
            .route("/items/{item}", get(|| async { StatusCode::CREATED }))
            .layer(middleware::from_fn(
                move |request: Request, next: middleware::Next| {
                    let recorder = Arc::clone(&recorder);
                    async move {
                        recorder
                            .lock()
                            .expect("recorder should lock")
                            .push(matched_route(&request));
                        next.run(request).await
                    }
                },
            ));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/items/private-value?token=secret")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(response.status(), StatusCode::CREATED);
        let missing = app
            .oneshot(
                Request::builder()
                    .uri("/missing/private-value?token=secret")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);

        let routes = seen.lock().expect("recorder should lock").clone();
        assert_eq!(routes, vec!["/items/{item}", "<unmatched>"]);
    }
}
