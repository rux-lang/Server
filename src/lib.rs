pub mod abuse;
pub mod account;
pub mod auth;
pub mod cleanup;
pub mod contract;
pub mod dashboard;
pub mod discovery;
pub mod downloads;
pub mod metadata;
pub mod namespaces;
pub mod observability;
pub mod openapi;
pub mod playground;
pub mod publication;
pub mod resolver;
pub mod search;
pub mod tokens;
pub mod upload;
pub mod yanks;

mod paths;

use http::HeaderValue;
use tower_http::cors::{AllowOrigin, CorsLayer};

pub fn cors_layer(origin: HeaderValue) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::list([origin]))
        .allow_methods([
            http::Method::GET,
            http::Method::POST,
            http::Method::PATCH,
            http::Method::DELETE,
        ])
        .allow_headers([
            http::header::AUTHORIZATION,
            http::header::CONTENT_TYPE,
            http::header::IF_NONE_MATCH,
            http::HeaderName::from_static("x-csrf-token"),
        ])
        .expose_headers([
            http::header::ETAG,
            http::header::RETRY_AFTER,
            http::HeaderName::from_static("x-request-id"),
        ])
        .allow_credentials(true)
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::header::{
        ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_HEADERS,
        ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN, ACCESS_CONTROL_EXPOSE_HEADERS,
        ACCESS_CONTROL_REQUEST_HEADERS, ACCESS_CONTROL_REQUEST_METHOD, ORIGIN,
    };
    use axum::http::{Method, Request, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt;

    use super::*;

    fn router() -> Router {
        Router::new()
            .route("/mutation", post(|| async { StatusCode::NO_CONTENT }))
            .route("/read", get(|| async { StatusCode::NO_CONTENT }))
            .layer(cors_layer(
                "https://rux-lang.dev".parse().expect("origin should parse"),
            ))
    }

    #[tokio::test]
    async fn cors_allows_credentials_and_csrf_preflight_only_for_the_exact_origin() {
        let allowed = router()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/mutation")
                    .header(ORIGIN, "https://rux-lang.dev")
                    .header(ACCESS_CONTROL_REQUEST_METHOD, "DELETE")
                    .header(
                        ACCESS_CONTROL_REQUEST_HEADERS,
                        "authorization,content-type,if-none-match,x-csrf-token",
                    )
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert_eq!(allowed.status(), StatusCode::OK);
        assert_eq!(
            allowed.headers()[ACCESS_CONTROL_ALLOW_ORIGIN],
            "https://rux-lang.dev"
        );
        assert_eq!(allowed.headers()[ACCESS_CONTROL_ALLOW_CREDENTIALS], "true");
        assert!(
            allowed.headers()[ACCESS_CONTROL_ALLOW_METHODS]
                .to_str()
                .expect("methods should be text")
                .contains("DELETE")
        );
        let allowed_headers = allowed.headers()[ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .expect("headers should be text");
        assert!(allowed_headers.contains("content-type"));
        assert!(allowed_headers.contains("if-none-match"));
        assert!(allowed_headers.contains("x-csrf-token"));
        assert!(allowed_headers.contains("authorization"));

        let read = router()
            .oneshot(
                Request::builder()
                    .uri("/read")
                    .header(ORIGIN, "https://rux-lang.dev")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        let exposed = read.headers()[ACCESS_CONTROL_EXPOSE_HEADERS]
            .to_str()
            .expect("exposed headers should be text");
        for header in ["etag", "retry-after", "x-request-id"] {
            assert!(exposed.contains(header), "missing exposed header {header}");
        }

        let hostile = router()
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/mutation")
                    .header(ORIGIN, "https://evil.example")
                    .header(ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(ACCESS_CONTROL_REQUEST_HEADERS, "x-csrf-token")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");
        assert!(hostile.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).is_none());
    }
}
