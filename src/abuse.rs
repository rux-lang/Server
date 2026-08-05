use std::fmt;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use governor::middleware::NoOpMiddleware;
use ipnet::IpNet;
use tower_governor::governor::{GovernorConfig, GovernorConfigBuilder};
use tower_governor::key_extractor::KeyExtractor;
use tower_governor::{GovernorError, GovernorLayer};

use crate::contract::Problem;

const X_FORWARDED_FOR: &str = "x-forwarded-for";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RateLimit {
    pub per_minute: u32,
    pub burst: u32,
}

#[derive(Clone, Debug)]
pub struct AbuseConfig {
    pub trusted_proxy_cidrs: Arc<[IpNet]>,
    pub request_timeout: Duration,
    pub publication_timeout: Duration,
    pub playground_timeout: Duration,
    pub read: RateLimit,
    pub security: RateLimit,
    pub mutation: RateLimit,
    pub publication: RateLimit,
    pub playground: RateLimit,
    pub upload_max_concurrency: usize,
    pub playground_max_concurrency: usize,
}

#[derive(Clone, Copy, Debug)]
pub enum RateTier {
    Read,
    Security,
    Mutation,
    Publication,
    Playground,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct ClientKey(IpAddr);

impl ClientKey {
    fn new(address: IpAddr) -> Self {
        match address {
            IpAddr::V4(address) => Self(IpAddr::V4(address)),
            IpAddr::V6(address) => {
                let network = u128::from(address) & (u128::MAX << 64);
                Self(IpAddr::V6(Ipv6Addr::from(network)))
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ClientKeyExtractor;

impl KeyExtractor for ClientKeyExtractor {
    type Key = ClientKey;

    fn extract<T>(&self, request: &http::Request<T>) -> Result<Self::Key, GovernorError> {
        request
            .extensions()
            .get::<ClientKey>()
            .copied()
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

type LimiterConfig = GovernorConfig<ClientKeyExtractor, NoOpMiddleware>;
type LimiterLayer = GovernorLayer<ClientKeyExtractor, NoOpMiddleware, Body>;

#[derive(Clone)]
pub struct AbuseControls {
    config: AbuseConfig,
    read: Arc<LimiterConfig>,
    security: Arc<LimiterConfig>,
    mutation: Arc<LimiterConfig>,
    publication: Arc<LimiterConfig>,
    playground: Arc<LimiterConfig>,
    upload_slots: Arc<tokio::sync::Semaphore>,
    playground_slots: Arc<tokio::sync::Semaphore>,
}

impl fmt::Debug for AbuseControls {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AbuseControls")
            .field(
                "trusted_proxy_count",
                &self.config.trusted_proxy_cidrs.len(),
            )
            .field("request_timeout", &self.config.request_timeout)
            .field("publication_timeout", &self.config.publication_timeout)
            .finish_non_exhaustive()
    }
}

impl AbuseControls {
    #[must_use]
    pub fn new(config: AbuseConfig) -> Self {
        Self {
            read: limiter(config.read),
            security: limiter(config.security),
            mutation: limiter(config.mutation),
            publication: limiter(config.publication),
            playground: limiter(config.playground),
            upload_slots: Arc::new(tokio::sync::Semaphore::new(config.upload_max_concurrency)),
            playground_slots: Arc::new(tokio::sync::Semaphore::new(
                config.playground_max_concurrency,
            )),
            config,
        }
    }

    #[must_use]
    fn rate_limit_layer(&self, tier: RateTier) -> LimiterLayer {
        let config = match tier {
            RateTier::Read => self.read.clone(),
            RateTier::Security => self.security.clone(),
            RateTier::Mutation => self.mutation.clone(),
            RateTier::Publication => self.publication.clone(),
            RateTier::Playground => self.playground.clone(),
        };
        GovernorLayer::new(config).error_handler(rate_limit_error)
    }

    pub fn protect(&self, router: Router, tier: RateTier) -> Router {
        self.protect_with_timeout(router, tier, self.request_timeout())
    }

    pub fn protect_publication(&self, router: Router) -> Router {
        router
            .layer(middleware::from_fn_with_state(
                self.upload_concurrency(),
                limit_upload_concurrency,
            ))
            .layer(middleware::from_fn_with_state(
                RequestTimeout(self.publication_timeout()),
                enforce_timeout,
            ))
            .layer(self.rate_limit_layer(RateTier::Publication))
    }

    /// Protects the playground, which executes submitted code.
    ///
    /// Mirrors [`AbuseControls::protect_publication`]: an admission semaphore
    /// so a burst cannot occupy every sandbox slot, a timeout longer than an
    /// ordinary request because a run compiles and executes, and a tier of its
    /// own so playground traffic cannot exhaust the read budget.
    pub fn protect_playground(&self, router: Router) -> Router {
        router
            .layer(middleware::from_fn_with_state(
                self.playground_concurrency(),
                limit_playground_concurrency,
            ))
            .layer(middleware::from_fn_with_state(
                RequestTimeout(self.playground_timeout()),
                enforce_timeout,
            ))
            .layer(self.rate_limit_layer(RateTier::Playground))
    }

    pub fn timeout_only(&self, router: Router) -> Router {
        router.layer(middleware::from_fn_with_state(
            RequestTimeout(self.request_timeout()),
            enforce_timeout,
        ))
    }

    fn protect_with_timeout(&self, router: Router, tier: RateTier, timeout: Duration) -> Router {
        router
            .layer(middleware::from_fn_with_state(
                RequestTimeout(timeout),
                enforce_timeout,
            ))
            .layer(self.rate_limit_layer(tier))
    }

    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        self.config.request_timeout
    }

    #[must_use]
    pub const fn publication_timeout(&self) -> Duration {
        self.config.publication_timeout
    }

    #[must_use]
    pub const fn playground_timeout(&self) -> Duration {
        self.config.playground_timeout
    }

    #[must_use]
    pub fn trusted_proxies(&self) -> TrustedProxies {
        TrustedProxies(self.config.trusted_proxy_cidrs.clone())
    }

    #[must_use]
    pub fn upload_concurrency(&self) -> UploadConcurrency {
        UploadConcurrency(self.upload_slots.clone())
    }

    #[must_use]
    pub fn playground_concurrency(&self) -> PlaygroundConcurrency {
        PlaygroundConcurrency(self.playground_slots.clone())
    }

    pub fn prune_rate_limiters(&self) {
        self.read.limiter().retain_recent();
        self.security.limiter().retain_recent();
        self.mutation.limiter().retain_recent();
        self.publication.limiter().retain_recent();
        self.playground.limiter().retain_recent();
    }
}

pub async fn run_pruner(controls: AbuseControls, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await;
    loop {
        tokio::select! {
            _ = interval.tick() => controls.prune_rate_limiters(),
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

fn limiter(limit: RateLimit) -> Arc<LimiterConfig> {
    let period = Duration::from_secs(60) / limit.per_minute;
    let config = GovernorConfigBuilder::default()
        .period(period)
        .burst_size(limit.burst)
        .key_extractor(ClientKeyExtractor)
        .finish()
        .expect("validated rate limits produce a governor configuration");
    Arc::new(config)
}

#[allow(clippy::needless_pass_by_value)]
fn rate_limit_error(error: GovernorError) -> Response<Body> {
    match error {
        GovernorError::TooManyRequests { wait_time, .. } => {
            let mut response = Problem::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Too many requests",
            )
            .with_detail("The request rate limit was exceeded.")
            .into_response();
            let wait_time = wait_time.max(1).to_string();
            response.headers_mut().insert(
                RETRY_AFTER,
                HeaderValue::from_str(&wait_time).expect("integer retry delays are valid headers"),
            );
            response
        }
        GovernorError::UnableToExtractKey | GovernorError::Other { .. } => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "abuse_controls_unavailable",
            "Request controls are temporarily unavailable",
        )
        .into_response(),
    }
}

#[derive(Clone, Debug)]
pub struct TrustedProxies(Arc<[IpNet]>);

pub async fn resolve_client_ip(
    State(trusted): State<TrustedProxies>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut request: Request,
    next: Next,
) -> Response {
    match client_key(peer.ip(), request.headers(), &trusted.0) {
        Ok(key) => {
            request.extensions_mut().insert(key);
            next.run(request).await
        }
        Err(()) => Problem::new(
            StatusCode::BAD_REQUEST,
            "invalid_proxy_header",
            "The proxy forwarding header is invalid",
        )
        .into_response(),
    }
}

fn client_key(peer: IpAddr, headers: &HeaderMap, trusted: &[IpNet]) -> Result<ClientKey, ()> {
    if !trusted.iter().any(|network| network.contains(&peer)) {
        return Ok(ClientKey::new(peer));
    }
    let values = headers.get_all(X_FORWARDED_FOR);
    if values.iter().next().is_none() {
        return Ok(ClientKey::new(peer));
    }

    let mut chain = Vec::new();
    for value in values {
        let value = value.to_str().map_err(|_| ())?;
        for address in value.split(',') {
            chain.push(address.trim().parse::<IpAddr>().map_err(|_| ())?);
        }
    }
    if chain.is_empty() {
        return Err(());
    }
    let client = chain
        .iter()
        .rev()
        .copied()
        .find(|address| !trusted.iter().any(|network| network.contains(address)))
        .unwrap_or(chain[0]);
    Ok(ClientKey::new(client))
}

#[derive(Clone, Copy, Debug)]
pub struct RequestTimeout(pub Duration);

pub async fn enforce_timeout(
    State(timeout): State<RequestTimeout>,
    request: Request,
    next: Next,
) -> Response {
    match tokio::time::timeout(timeout.0, next.run(request)).await {
        Ok(response) => response,
        Err(_) => Problem::new(
            StatusCode::GATEWAY_TIMEOUT,
            "request_timeout",
            "The request timed out",
        )
        .into_response(),
    }
}

#[derive(Clone, Debug)]
pub struct UploadConcurrency(Arc<tokio::sync::Semaphore>);

pub async fn limit_upload_concurrency(
    State(limit): State<UploadConcurrency>,
    request: Request,
    next: Next,
) -> Response {
    let Ok(permit) = limit.0.clone().try_acquire_owned() else {
        let mut response = Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "publication_unavailable",
            "Package publication is temporarily unavailable",
        )
        .into_response();
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from_static("1"));
        return response;
    };
    let response = next.run(request).await;
    drop(permit);
    response
}

#[derive(Clone, Debug)]
pub struct PlaygroundConcurrency(Arc<tokio::sync::Semaphore>);

/// Refuses a run rather than queueing it when every admission slot is taken.
///
/// The sandbox daemon has an admission limit of its own; this one keeps a burst
/// from occupying API request slots while it waits for a sandbox that is
/// already full.
pub async fn limit_playground_concurrency(
    State(limit): State<PlaygroundConcurrency>,
    request: Request,
    next: Next,
) -> Response {
    let Ok(permit) = limit.0.clone().try_acquire_owned() else {
        let mut response = Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "playground_unavailable",
            "The playground is temporarily unavailable",
        )
        .into_response();
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from_static("1"));
        return response;
    };
    let response = next.run(request).await;
    drop(permit);
    response
}

pub async fn discard_supplied_request_id(mut request: Request, next: Next) -> Response {
    request.headers_mut().remove("x-request-id");
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use axum::http::Request as HttpRequest;
    use axum::routing::{get, post};
    use serde_json::Value;
    use tower::ServiceExt;
    use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

    use super::*;

    fn controls(request_timeout: Duration, upload_max_concurrency: usize) -> AbuseControls {
        AbuseControls::new(config_for(request_timeout, upload_max_concurrency))
    }

    fn config_for(request_timeout: Duration, upload_max_concurrency: usize) -> AbuseConfig {
        AbuseConfig {
            trusted_proxy_cidrs: Arc::from([]),
            request_timeout,
            publication_timeout: request_timeout,
            read: RateLimit {
                per_minute: 60,
                burst: 2,
            },
            security: RateLimit {
                per_minute: 60,
                burst: 2,
            },
            mutation: RateLimit {
                per_minute: 60,
                burst: 2,
            },
            publication: RateLimit {
                per_minute: 60,
                burst: 2,
            },
            playground: RateLimit {
                per_minute: 60,
                burst: 2,
            },
            playground_timeout: request_timeout,
            upload_max_concurrency,
            playground_max_concurrency: upload_max_concurrency,
        }
    }

    fn keyed_request_with_method(uri: &str, address: &str, method: &str) -> HttpRequest<Body> {
        let mut request = HttpRequest::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(ClientKey::new(address.parse().unwrap()));
        request
    }

    async fn problem_body(response: axum::response::Response) -> serde_json::Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    fn keyed_request(uri: &str, address: &str) -> HttpRequest<Body> {
        let mut request = HttpRequest::builder().uri(uri).body(Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(ClientKey::new(address.parse().unwrap()));
        request
    }

    #[test]
    fn trusted_forwarding_chains_select_the_first_untrusted_hop() {
        let trusted = [
            "127.0.0.0/8".parse::<IpNet>().unwrap(),
            "10.0.0.0/8".parse::<IpNet>().unwrap(),
        ];
        let mut headers = HeaderMap::new();
        headers.insert(
            X_FORWARDED_FOR,
            HeaderValue::from_static("203.0.113.9, 10.0.0.2"),
        );
        assert_eq!(
            client_key("127.0.0.1".parse().unwrap(), &headers, &trusted),
            Ok(ClientKey::new("203.0.113.9".parse().unwrap()))
        );
    }

    #[test]
    fn untrusted_peers_cannot_spoof_forwarding_headers() {
        let trusted = ["127.0.0.0/8".parse::<IpNet>().unwrap()];
        let mut headers = HeaderMap::new();
        headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("198.51.100.4"));
        assert_eq!(
            client_key("192.0.2.7".parse().unwrap(), &headers, &trusted),
            Ok(ClientKey::new("192.0.2.7".parse().unwrap()))
        );
    }

    #[test]
    fn malformed_trusted_forwarding_headers_are_rejected() {
        let trusted = ["127.0.0.0/8".parse::<IpNet>().unwrap()];
        let mut headers = HeaderMap::new();
        headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("not-an-address"));
        assert_eq!(
            client_key("127.0.0.1".parse().unwrap(), &headers, &trusted),
            Err(())
        );
    }

    #[test]
    fn ipv6_clients_share_a_64_bit_prefix_key() {
        assert_eq!(
            ClientKey::new("2001:db8:1234:5678::1".parse().unwrap()),
            ClientKey::new("2001:db8:1234:5678::ffff".parse().unwrap())
        );
        assert_ne!(
            ClientKey::new("2001:db8:1234:5678::1".parse().unwrap()),
            ClientKey::new("2001:db8:1234:5679::1".parse().unwrap())
        );
    }

    #[tokio::test]
    async fn rate_limits_are_isolated_by_client_and_return_problem_details() {
        let controls = controls(Duration::from_secs(1), 1);
        let app = controls.protect(
            Router::new().route("/", get(|| async { StatusCode::NO_CONTENT })),
            RateTier::Read,
        );

        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(keyed_request("/", "192.0.2.1"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
        }
        let limited = app
            .clone()
            .oneshot(keyed_request("/", "192.0.2.1"))
            .await
            .unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.headers()[RETRY_AFTER], "1");
        let body = to_bytes(limited.into_body(), usize::MAX).await.unwrap();
        let problem: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["code"], "rate_limited");

        let other_client = app.oneshot(keyed_request("/", "192.0.2.2")).await.unwrap();
        assert_eq!(other_client.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn request_deadlines_return_a_safe_problem() {
        let controls = controls(Duration::from_millis(1), 1);
        let app = controls.timeout_only(Router::new().route(
            "/",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                StatusCode::NO_CONTENT
            }),
        ));

        let response = app.oneshot(keyed_request("/", "192.0.2.1")).await.unwrap();
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let problem: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["code"], "request_timeout");
    }

    #[tokio::test]
    async fn saturated_upload_admission_fails_without_waiting() {
        let controls = controls(Duration::from_secs(1), 1);
        let concurrency = controls.upload_concurrency();
        let gate = Arc::new(tokio::sync::Notify::new());
        let handler_gate = gate.clone();
        let app = Router::new()
            .route(
                "/",
                post(move || {
                    let gate = handler_gate.clone();
                    async move {
                        gate.notified().await;
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .layer(middleware::from_fn_with_state(
                concurrency.clone(),
                limit_upload_concurrency,
            ));

        let first = tokio::spawn(
            app.clone().oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
        while concurrency.0.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
        let second = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(second.headers()[RETRY_AFTER], "1");
        gate.notify_one();
        assert_eq!(
            first.await.unwrap().unwrap().status(),
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn saturated_playground_admission_fails_without_waiting() {
        let controls = controls(Duration::from_secs(1), 1);
        let concurrency = controls.playground_concurrency();
        let gate = Arc::new(tokio::sync::Notify::new());
        let handler_gate = gate.clone();
        let app = Router::new()
            .route(
                "/",
                post(move || {
                    let gate = handler_gate.clone();
                    async move {
                        gate.notified().await;
                        StatusCode::NO_CONTENT
                    }
                }),
            )
            .layer(middleware::from_fn_with_state(
                concurrency.clone(),
                limit_playground_concurrency,
            ));

        let first = tokio::spawn(
            app.clone().oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
        while concurrency.0.available_permits() != 0 {
            tokio::task::yield_now().await;
        }
        let second = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Refused immediately rather than queued behind a sandbox that is
        // already full, and with the playground's own problem code.
        assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(second.headers()[RETRY_AFTER], "1");
        assert_eq!(problem_body(second).await["code"], "playground_unavailable");
        gate.notify_one();
        assert_eq!(
            first.await.unwrap().unwrap().status(),
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn the_playground_tier_has_a_budget_of_its_own() {
        // Exhausting the playground tier must not touch the read budget, and a
        // refusal must carry the shared rate-limit contract.
        let controls = AbuseControls::new(AbuseConfig {
            playground: RateLimit {
                per_minute: 1,
                burst: 1,
            },
            ..config_for(Duration::from_secs(1), 1)
        });
        let app = controls.protect_playground(
            Router::new().route("/", post(|| async { StatusCode::NO_CONTENT })),
        );

        let first = app
            .clone()
            .oneshot(keyed_request_with_method("/", "203.0.113.7", "POST"))
            .await
            .unwrap();
        let second = app
            .oneshot(keyed_request_with_method("/", "203.0.113.7", "POST"))
            .await
            .unwrap();

        assert_eq!(first.status(), StatusCode::NO_CONTENT);
        assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn caller_request_ids_are_discarded_and_server_ids_are_propagated() {
        let app = Router::new()
            .route("/", get(|| async { StatusCode::NO_CONTENT }))
            .layer(PropagateRequestIdLayer::x_request_id())
            .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
            .layer(middleware::from_fn(discard_supplied_request_id));
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/")
                    .header("x-request-id", "caller-controlled")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let request_id = response.headers()["x-request-id"].to_str().unwrap();
        assert_ne!(request_id, "caller-controlled");
        assert_eq!(request_id.len(), 36);
        assert_eq!(request_id.bytes().filter(|byte| *byte == b'-').count(), 4);
    }
}
