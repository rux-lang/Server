use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, Response, StatusCode, header};
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::routing::get;
use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::metrics::{Counter, Gauge, Histogram, MeterProvider, UpDownCounter};
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::{TraceContextExt, TracerProvider};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use prometheus::{Encoder, Registry, TextEncoder};
use rux_application::DependencyProbe;
use tokio::sync::watch;
use tracing::{Instrument, Span, info, info_span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceExporter {
    None,
    Otlp,
}

pub struct Telemetry {
    registry: Registry,
    metrics: Metrics,
    tracer_provider: SdkTracerProvider,
    meter_provider: SdkMeterProvider,
}

impl Telemetry {
    /// Installs the process-wide tracing and metrics providers.
    ///
    /// # Errors
    ///
    /// Returns an error when an exporter cannot be configured or the global
    /// tracing subscriber has already been installed.
    pub fn init(trace_exporter: TraceExporter) -> Result<Self, Box<dyn std::error::Error>> {
        global::set_text_map_propagator(TraceContextPropagator::new());

        let service_name =
            env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "rux-server".to_owned());
        let resource = Resource::builder()
            .with_service_name(service_name)
            .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
            .build();

        let registry = Registry::new();
        let prometheus_exporter = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .without_scope_info()
            .build()?;
        let meter_provider = SdkMeterProvider::builder()
            .with_resource(resource.clone())
            .with_reader(prometheus_exporter)
            .build();
        global::set_meter_provider(meter_provider.clone());
        let metrics = Metrics::new(&meter_provider);

        let tracer_provider_builder = SdkTracerProvider::builder().with_resource(resource);
        let tracer_provider = match trace_exporter {
            TraceExporter::None => tracer_provider_builder.build(),
            TraceExporter::Otlp => {
                let exporter = opentelemetry_otlp::SpanExporter::builder()
                    .with_tonic()
                    .build()?;
                tracer_provider_builder
                    .with_batch_exporter(exporter)
                    .build()
            }
        };
        global::set_tracer_provider(tracer_provider.clone());
        let tracer = tracer_provider.tracer("rux-server");

        tracing_subscriber::registry()
            .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_span_list(true),
            )
            .try_init()?;

        Ok(Self {
            registry,
            metrics,
            tracer_provider,
            meter_provider,
        })
    }

    #[must_use]
    pub fn registry(&self) -> Registry {
        self.registry.clone()
    }

    #[must_use]
    pub fn metrics(&self) -> Metrics {
        self.metrics.clone()
    }

    pub fn shutdown(self) {
        if let Err(error) = self.tracer_provider.shutdown() {
            warn!(kind = "tracer_provider", %error, "telemetry shutdown failed");
        }
        if let Err(error) = self.meter_provider.shutdown() {
            warn!(kind = "meter_provider", %error, "telemetry shutdown failed");
        }
    }
}

#[derive(Clone)]
pub struct Metrics {
    http_requests: Counter<u64>,
    http_duration: Histogram<f64>,
    http_active: UpDownCounter<i64>,
    dependency_ready: Gauge<f64>,
    dependency_probes: Counter<u64>,
    dependency_duration: Histogram<f64>,
    dependency_last_probe: Gauge<f64>,
    cleanup_runs: Counter<u64>,
    cleanup_duration: Histogram<f64>,
    cleanup_objects: Counter<u64>,
    cleanup_last_success: Gauge<f64>,
}

impl Metrics {
    fn new(provider: &SdkMeterProvider) -> Self {
        let meter = provider.meter("rux-server");
        Self {
            http_requests: meter
                .u64_counter("rux_http_server_requests")
                .with_description("Completed HTTP requests")
                .build(),
            http_duration: meter
                .f64_histogram("rux_http_server_request_duration")
                .with_description("HTTP request duration")
                .with_unit("s")
                .with_boundaries(vec![
                    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
                ])
                .build(),
            http_active: meter
                .i64_up_down_counter("rux_http_server_active_requests")
                .with_description("Currently active HTTP requests")
                .build(),
            dependency_ready: meter
                .f64_gauge("rux_dependency_ready")
                .with_description("Whether a dependency is ready")
                .build(),
            dependency_probes: meter
                .u64_counter("rux_dependency_probes")
                .with_description("Dependency probe attempts")
                .build(),
            dependency_duration: meter
                .f64_histogram("rux_dependency_probe_duration")
                .with_description("Dependency probe duration")
                .with_unit("s")
                .with_boundaries(vec![
                    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
                ])
                .build(),
            dependency_last_probe: meter
                .f64_gauge("rux_dependency_last_probe_unixtime")
                .with_description("Unix time of the most recent dependency probe")
                .with_unit("s")
                .build(),
            cleanup_runs: meter
                .u64_counter("rux_orphan_cleanup_runs")
                .with_description("Orphan cleanup sweeps")
                .build(),
            cleanup_duration: meter
                .f64_histogram("rux_orphan_cleanup_duration")
                .with_description("Orphan cleanup sweep duration")
                .with_unit("s")
                .with_boundaries(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0])
                .build(),
            cleanup_objects: meter
                .u64_counter("rux_orphan_cleanup_objects")
                .with_description("Objects observed by orphan cleanup")
                .build(),
            cleanup_last_success: meter
                .f64_gauge("rux_orphan_cleanup_last_success_unixtime")
                .with_description("Unix time of the most recent successful cleanup sweep")
                .with_unit("s")
                .build(),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self::new(&SdkMeterProvider::builder().build())
    }

    pub fn record_dependency(&self, name: &'static str, healthy: bool, duration: Duration) {
        let outcome = if healthy { "healthy" } else { "unhealthy" };
        let labels = [
            KeyValue::new("dependency", name),
            KeyValue::new("outcome", outcome),
        ];
        self.dependency_ready.record(
            if healthy { 1.0 } else { 0.0 },
            &[KeyValue::new("dependency", name)],
        );
        self.dependency_probes.add(1, &labels);
        self.dependency_duration
            .record(duration.as_secs_f64(), &labels);
        self.dependency_last_probe
            .record(unix_timestamp(), &[KeyValue::new("dependency", name)]);
    }

    pub fn record_cleanup_success(&self, duration: Duration, values: &[(&'static str, u64)]) {
        let labels = [KeyValue::new("outcome", "success")];
        self.cleanup_runs.add(1, &labels);
        self.cleanup_duration
            .record(duration.as_secs_f64(), &labels);
        for (result, value) in values {
            self.cleanup_objects
                .add(*value, &[KeyValue::new("result", *result)]);
        }
        self.cleanup_last_success.record(unix_timestamp(), &[]);
    }

    pub fn record_cleanup_failure(&self, duration: Duration) {
        let labels = [KeyValue::new("outcome", "failure")];
        self.cleanup_runs.add(1, &labels);
        self.cleanup_duration
            .record(duration.as_secs_f64(), &labels);
    }
}

fn unix_timestamp() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(axum::http::HeaderName::as_str).collect()
    }
}

#[must_use]
pub fn request_span(request: &Request) -> Span {
    let route = matched_route(request);
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("<missing>");
    let span = info_span!(
        "http_request",
        request_id,
        method = %request.method(),
        route = %route,
        status_code = tracing::field::Empty,
        duration_ms = tracing::field::Empty,
        trace_id = tracing::field::Empty,
        span_id = tracing::field::Empty,
    );
    let parent = global::get_text_map_propagator(|propagator| {
        propagator.extract(&HeaderExtractor(request.headers()))
    });
    let _ = span.set_parent(parent);
    record_trace_context(&span);
    span
}

pub fn record_response<B>(response: &Response<B>, latency: Duration, span: &Span) {
    span.record("status_code", response.status().as_u16());
    span.record("duration_ms", latency.as_secs_f64() * 1_000.0);
    record_trace_context(span);
    info!(
        parent: span,
        status_code = response.status().as_u16(),
        duration_ms = latency.as_secs_f64() * 1_000.0,
        "HTTP request completed"
    );
}

pub fn record_trace_context(span: &Span) {
    let context = span.context();
    let current = context.span();
    let span_context = current.span_context();
    if span_context.is_valid() {
        span.record("trace_id", tracing::field::display(span_context.trace_id()));
        span.record("span_id", tracing::field::display(span_context.span_id()));
    }
}

fn matched_route(request: &Request) -> String {
    request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "<unmatched>".to_owned(), |path| path.as_str().to_owned())
}

pub async fn record_http_metrics(
    State(metrics): State<Metrics>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let method = request.method().as_str().to_owned();
    let route = matched_route(&request);
    let guard = ActiveRequest::new(metrics, method, route);
    let response = next.run(request).await;
    guard.complete(response.status());
    response
}

struct ActiveRequest {
    metrics: Metrics,
    method: String,
    route: String,
    started: Instant,
    completed: bool,
}

impl ActiveRequest {
    fn new(metrics: Metrics, method: String, route: String) -> Self {
        metrics.http_active.add(
            1,
            &[
                KeyValue::new("method", method.clone()),
                KeyValue::new("route", route.clone()),
            ],
        );
        Self {
            metrics,
            method,
            route,
            started: Instant::now(),
            completed: false,
        }
    }

    fn complete(mut self, status: StatusCode) {
        self.record(status.as_u16().to_string());
        self.completed = true;
    }

    fn record(&self, status: String) {
        let labels = [
            KeyValue::new("method", self.method.clone()),
            KeyValue::new("route", self.route.clone()),
            KeyValue::new("status_code", status),
        ];
        self.metrics.http_requests.add(1, &labels);
        self.metrics
            .http_duration
            .record(self.started.elapsed().as_secs_f64(), &labels);
    }
}

impl Drop for ActiveRequest {
    fn drop(&mut self) {
        if !self.completed {
            self.record("cancelled".to_owned());
        }
        self.metrics.http_active.add(
            -1,
            &[
                KeyValue::new("method", self.method.clone()),
                KeyValue::new("route", self.route.clone()),
            ],
        );
    }
}

pub fn metrics_router(registry: Registry) -> Router {
    Router::new()
        .route("/metrics", get(render_metrics))
        .with_state(registry)
}

async fn render_metrics(State(registry): State<Registry>) -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let mut output = Vec::new();
    match encoder.encode(&registry.gather(), &mut output) {
        Ok(()) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, encoder.format_type().to_owned())],
            output,
        )
            .into_response(),
        Err(error) => {
            warn!(kind = "prometheus_encode", %error, "metrics encoding failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn run_dependency_monitor(
    probe: Arc<dyn DependencyProbe>,
    metrics: Metrics,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        let span = info_span!(
            "dependency_readiness_probe",
            trace_id = tracing::field::Empty,
            span_id = tracing::field::Empty,
        );
        record_trace_context(&span);
        let statuses = probe.readiness().instrument(span.clone()).await;
        let span_guard = span.enter();
        for status in statuses {
            metrics.record_dependency(status.name, status.healthy, status.duration);
            info!(
                dependency = status.name,
                outcome = if status.healthy {
                    "healthy"
                } else {
                    "unhealthy"
                },
                duration_ms = status.duration.as_secs_f64() * 1_000.0,
                "dependency probe completed"
            );
        }
        drop(span_guard);
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::body::to_bytes;
    use axum::middleware;
    use axum::routing::get;
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry::trace::{SpanId, TraceId};
    use tower::ServiceExt;

    use super::*;

    fn test_telemetry() -> (SdkMeterProvider, Registry, Metrics) {
        let registry = Registry::new();
        let exporter = opentelemetry_prometheus::exporter()
            .with_registry(registry.clone())
            .without_scope_info()
            .build()
            .expect("exporter should build");
        let provider = SdkMeterProvider::builder().with_reader(exporter).build();
        let metrics = Metrics::new(&provider);
        (provider, registry, metrics)
    }

    #[tokio::test]
    async fn metrics_endpoint_exposes_stable_metric_names() {
        let (_provider, registry, metrics) = test_telemetry();
        metrics.record_dependency("postgresql", true, Duration::from_millis(10));
        metrics.record_cleanup_success(Duration::from_millis(20), &[("deleted", 2)]);

        let response = metrics_router(registry)
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("metrics router should respond");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body should read");
        let text = String::from_utf8(body.to_vec()).expect("metrics should be text");
        assert!(text.contains("rux_dependency_probes_total"));
        assert!(text.contains("rux_dependency_probe_duration_seconds"));
        assert!(text.contains("rux_orphan_cleanup_objects_total"));
        assert!(text.contains("rux_orphan_cleanup_last_success_unixtime_seconds"));
    }

    #[tokio::test]
    async fn http_metrics_use_route_templates_and_bounded_statuses() {
        let (_provider, registry, metrics) = test_telemetry();
        let app = Router::new()
            .route("/items/{item}", get(|| async { StatusCode::CREATED }))
            .layer(middleware::from_fn_with_state(metrics, record_http_metrics));

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

        let encoder = TextEncoder::new();
        let mut output = Vec::new();
        encoder
            .encode(&registry.gather(), &mut output)
            .expect("metrics should encode");
        let text = String::from_utf8(output).expect("metrics should be text");
        assert!(text.contains("rux_http_server_requests_total"));
        assert!(text.contains("route=\"/items/{item}\""));
        assert!(text.contains("status_code=\"201\""));
        assert!(text.contains("route=\"<unmatched>\""));
        assert!(text.contains("status_code=\"404\""));
        assert!(!text.contains("private-value"));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn w3c_trace_context_is_extracted_from_request_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
                .parse()
                .expect("traceparent should parse"),
        );
        let context = TraceContextPropagator::new().extract(&HeaderExtractor(&headers));
        let current = context.span();
        let span_context = current.span_context();
        assert!(span_context.is_remote());
        assert_eq!(
            span_context.trace_id(),
            TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").expect("trace id")
        );
        assert_eq!(
            span_context.span_id(),
            SpanId::from_hex("b7ad6b7169203331").expect("span id")
        );
    }
}
