use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, LOCATION};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rux_application::{DownloadErrorKind, DownloadTarget, Downloads};
use url::Url;

use crate::contract::{Problem, ProblemResponse, ValidationError};

const DOWNLOAD_CACHE_CONTROL: &str = "no-store";

#[derive(Clone)]
pub(crate) struct DownloadState {
    downloads: Arc<dyn Downloads>,
    package_cdn_base_url: Url,
}

pub fn router(downloads: Arc<dyn Downloads>, package_cdn_base_url: Url) -> Router {
    Router::new()
        .route(
            "/v1/packages/{namespace}/{package}/{version}/download",
            get(package_download).head(package_download_head),
        )
        .with_state(DownloadState {
            downloads,
            package_cdn_base_url,
        })
}

#[utoipa::path(
    get,
    path = "/packages/{namespace}/{package}/{version}/download",
    params(
        ("namespace" = String, Path, description = "Registry namespace identity"),
        ("package" = String, Path, description = "Package identity"),
        ("version" = String, Path, description = "Exact strict Semantic Version")
    ),
    responses(
        (status = 307, description = "Download recorded and redirected to the immutable CDN object",
            headers(
                ("Location" = String, description = "Absolute public CDN object URL"),
                ("Cache-Control" = String, description = "No-store policy preserving per-request accounting")
            )
        ),
        (status = 404, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn package_download(
    State(state): State<DownloadState>,
    Path((namespace, package, version)): Path<(String, String, String)>,
) -> Response {
    match state
        .downloads
        .download(&namespace, &package, &version)
        .await
    {
        Ok(target) => redirect_response(&state.package_cdn_base_url, &target),
        Err(error) => download_problem(error.kind()).into_response(),
    }
}

#[utoipa::path(
    head,
    path = "/packages/{namespace}/{package}/{version}/download",
    params(
        ("namespace" = String, Path, description = "Registry namespace identity"),
        ("package" = String, Path, description = "Package identity"),
        ("version" = String, Path, description = "Exact strict Semantic Version")
    ),
    responses(
        (status = 307, description = "Known download target resolved without recording an event",
            headers(
                ("Location" = String, description = "Absolute public CDN object URL"),
                ("Cache-Control" = String, description = "No-store policy preserving GET accounting")
            )
        ),
        (status = 404, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse)
    )
)]
pub(crate) async fn package_download_head(
    State(state): State<DownloadState>,
    Path((namespace, package, version)): Path<(String, String, String)>,
) -> Response {
    match state
        .downloads
        .resolve(&namespace, &package, &version)
        .await
    {
        Ok(target) => redirect_response(&state.package_cdn_base_url, &target),
        Err(error) => download_problem(error.kind()).into_response(),
    }
}

fn redirect_response(base_url: &Url, target: &DownloadTarget) -> Response {
    let location = cdn_url(base_url, &target.storage_key);
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header(
            LOCATION,
            HeaderValue::from_str(location.as_str()).expect("URLs serialize as valid headers"),
        )
        .header(CACHE_CONTROL, DOWNLOAD_CACHE_CONTROL)
        .body(Body::empty())
        .expect("download redirect response should be valid")
}

fn cdn_url(base_url: &Url, storage_key: &str) -> Url {
    let mut target = base_url.clone();
    let mut segments = target
        .path_segments_mut()
        .expect("validated CDN URLs support path segments");
    segments.pop_if_empty().extend(storage_key.split('/'));
    drop(segments);
    target
}

fn download_problem(kind: DownloadErrorKind) -> Problem {
    match kind {
        DownloadErrorKind::InvalidNamespace => invalid_path(
            "invalid_namespace",
            "must satisfy the registry identity-segment syntax",
            "/namespace",
        ),
        DownloadErrorKind::InvalidPackage => invalid_path(
            "invalid_package",
            "must satisfy the registry identity-segment syntax",
            "/package",
        ),
        DownloadErrorKind::InvalidVersion => invalid_path(
            "invalid_version",
            "must be a strict Semantic Version",
            "/version",
        ),
        DownloadErrorKind::PackageVersionNotFound => Problem::new(
            StatusCode::NOT_FOUND,
            "package_version_not_found",
            "The package version was not found",
        ),
        DownloadErrorKind::Unavailable => Problem::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "download_unavailable",
            "Package downloads are temporarily unavailable",
        ),
    }
}

fn invalid_path(code: &str, detail: &str, pointer: &str) -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
        "The request is invalid",
    )
    .with_detail("One or more path parameters are invalid.")
    .with_errors(vec![
        ValidationError::new(code, detail).with_pointer(pointer),
    ])
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request};
    use rux_application::{DownloadError, DownloadTarget};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    struct StubDownloads {
        error: Option<DownloadErrorKind>,
        calls: Mutex<Vec<&'static str>>,
    }

    #[async_trait]
    impl Downloads for StubDownloads {
        async fn download(
            &self,
            _namespace: &str,
            _package: &str,
            _version: &str,
        ) -> Result<DownloadTarget, DownloadError> {
            self.calls.lock().unwrap().push("download");
            self.result()
        }

        async fn resolve(
            &self,
            _namespace: &str,
            _package: &str,
            _version: &str,
        ) -> Result<DownloadTarget, DownloadError> {
            self.calls.lock().unwrap().push("resolve");
            self.result()
        }
    }

    impl StubDownloads {
        fn result(&self) -> Result<DownloadTarget, DownloadError> {
            self.error.map_or_else(
                || {
                    Ok(DownloadTarget {
                        storage_key: "packages/rux/example/1.0.0+native/checksum.ruxpkg".into(),
                    })
                },
                |kind| Err(DownloadError::new(kind)),
            )
        }
    }

    #[tokio::test]
    async fn get_redirects_to_the_cdn_without_allowing_redirect_caching() {
        let downloads = Arc::new(StubDownloads {
            error: None,
            calls: Mutex::new(Vec::new()),
        });
        let response = test_router(downloads.clone())
            .oneshot(request(Method::GET))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(response.headers()[CACHE_CONTROL], DOWNLOAD_CACHE_CONTROL);
        assert_eq!(
            response.headers()[LOCATION],
            "https://cdn.example.test/artifacts/packages/rux/example/1.0.0+native/checksum.ruxpkg"
        );
        assert!(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(*downloads.calls.lock().unwrap(), ["download"]);
    }

    #[tokio::test]
    async fn explicit_head_redirects_without_calling_the_recording_operation() {
        let downloads = Arc::new(StubDownloads {
            error: None,
            calls: Mutex::new(Vec::new()),
        });
        let response = test_router(downloads.clone())
            .oneshot(request(Method::HEAD))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(response.headers()[CACHE_CONTROL], DOWNLOAD_CACHE_CONTROL);
        assert_eq!(*downloads.calls.lock().unwrap(), ["resolve"]);
    }

    #[tokio::test]
    async fn errors_use_the_stable_problem_contract() {
        let cases = [
            (
                DownloadErrorKind::InvalidNamespace,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                DownloadErrorKind::InvalidPackage,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                DownloadErrorKind::InvalidVersion,
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_request",
            ),
            (
                DownloadErrorKind::PackageVersionNotFound,
                StatusCode::NOT_FOUND,
                "package_version_not_found",
            ),
            (
                DownloadErrorKind::Unavailable,
                StatusCode::SERVICE_UNAVAILABLE,
                "download_unavailable",
            ),
        ];
        for (kind, status, code) in cases {
            let response = test_router(Arc::new(StubDownloads {
                error: Some(kind),
                calls: Mutex::new(Vec::new()),
            }))
            .oneshot(request(Method::GET))
            .await
            .unwrap();
            assert_eq!(response.status(), status);
            let document: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                    .unwrap();
            assert_eq!(document["code"], code);
        }
    }

    fn test_router(downloads: Arc<StubDownloads>) -> Router {
        router(
            downloads,
            Url::parse("https://cdn.example.test/artifacts/").unwrap(),
        )
    }

    fn request(method: Method) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri("/v1/packages/rux/example/1.0.0+native/download")
            .body(Body::empty())
            .unwrap()
    }
}
