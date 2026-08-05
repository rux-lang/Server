use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rux_application::{
    PlaygroundError, PlaygroundErrorKind, PlaygroundExecution, PlaygroundLimits, PlaygroundMode,
    PlaygroundProfile, PlaygroundResult, PlaygroundRun,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::auth::origin_matches;
use crate::contract::{DataEnvelope, Problem, ProblemResponse, ValidationError};
use crate::observability::Metrics;

/// Largest accepted request body.
///
/// Comfortably above the source and standard-input limits together, so a
/// legitimate submission is refused by the application layer with an
/// explanation rather than by the framework with a bare rejection.
const MAX_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub(crate) struct PlaygroundState {
    playground: Arc<dyn PlaygroundExecution>,
    allowed_web_origin: String,
    metrics: Metrics,
}

/// Builds the playground routes.
///
/// `metrics` is a parameter rather than a layer because the mode and the
/// outcome of a run are only known inside the handler, and they are the two
/// labels that make the instrument useful.
pub fn router(
    playground: Arc<dyn PlaygroundExecution>,
    allowed_web_origin: String,
    metrics: Metrics,
) -> Router {
    Router::new()
        .route("/v1/playground/run", post(run_playground))
        .route("/v1/playground/limits", get(playground_limits))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(PlaygroundState {
            playground,
            allowed_web_origin,
            metrics,
        })
}

/// What to do with a submission.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlaygroundModeDocument {
    /// Compile, then execute the resulting program.
    #[default]
    Run,
    /// Compile only, and report diagnostics.
    Build,
    /// Reformat the source and return it.
    Fmt,
}

/// Which compiler profile to build with.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PlaygroundProfileDocument {
    /// Unoptimized build with debug assertions.
    #[default]
    Debug,
    /// Optimized build.
    Release,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub(crate) struct PlaygroundRunRequest {
    #[serde(default)]
    mode: PlaygroundModeDocument,
    #[serde(default)]
    profile: PlaygroundProfileDocument,
    source: String,
    #[serde(default)]
    stdin: String,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PlaygroundRunDocument {
    build: PlaygroundBuildDocument,
    #[serde(skip_serializing_if = "Option::is_none")]
    program: Option<PlaygroundProgramDocument>,
    #[serde(skip_serializing_if = "Option::is_none")]
    formatted: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PlaygroundBuildDocument {
    success: bool,
    diagnostics: String,
    diagnostics_truncated: bool,
    duration_ms: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PlaygroundProgramDocument {
    stdout: String,
    stdout_truncated: bool,
    stderr: String,
    stderr_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal: Option<i32>,
    timed_out: bool,
    duration_ms: u64,
}

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) struct PlaygroundLimitsDocument {
    max_source_bytes: u64,
    max_stdin_bytes: u64,
    max_output_bytes: u64,
    compile_timeout_seconds: u32,
    run_timeout_seconds: u32,
    memory_bytes: u64,
    cpu_millis: u32,
}

#[utoipa::path(
    post,
    path = "/playground/run",
    request_body = PlaygroundRunRequest,
    responses(
        (status = 200, description = "The run completed; a failed compile is reported in the body, not as a problem", body = DataEnvelope<PlaygroundRunDocument>),
        (status = 403, response = ProblemResponse),
        (status = 422, response = ProblemResponse),
        (status = 503, response = ProblemResponse),
        (status = 504, response = ProblemResponse)
    )
)]
pub(crate) async fn run_playground(
    State(state): State<PlaygroundState>,
    headers: HeaderMap,
    payload: Result<Json<PlaygroundRunRequest>, JsonRejection>,
) -> Response {
    // The endpoint is anonymous and executes code, so the origin is checked
    // before anything else: without it any page could drive the sandbox from a
    // visitor's browser.
    if !origin_matches(&headers, &state.allowed_web_origin) {
        return origin_not_allowed().into_response();
    }

    let Ok(Json(payload)) = payload else {
        return invalid_playground_request(
            "body must be a JSON object with known fields, within the size limit",
        )
        .into_response();
    };

    let run = to_run(payload);
    let mode = mode_label(run.mode);
    let started = Instant::now();
    // Dropped on every exit path, including a cancelled request, so the
    // in-flight gauge cannot drift upward.
    let _in_flight = state.metrics.playground_run_started();

    let outcome = state.playground.execute(run).await;
    let label = match &outcome {
        Ok(result) if result.build.success => "succeeded",
        Ok(_) => "build_failed",
        Err(error) => outcome_label(error.kind()),
    };
    state
        .metrics
        .record_playground_run(mode, label, started.elapsed());

    match outcome {
        Ok(result) => Json(DataEnvelope::new(run_document(result))).into_response(),
        Err(error) => playground_problem(&error),
    }
}

/// Fixed label for the requested mode.
const fn mode_label(mode: PlaygroundMode) -> &'static str {
    match mode {
        PlaygroundMode::Run => "run",
        PlaygroundMode::Build => "build",
        PlaygroundMode::Fmt => "fmt",
    }
}

/// Fixed label for how a run ended.
const fn outcome_label(kind: PlaygroundErrorKind) -> &'static str {
    match kind {
        PlaygroundErrorKind::InvalidRequest => "rejected",
        PlaygroundErrorKind::Unavailable => "unavailable",
        PlaygroundErrorKind::Timeout => "timed_out",
        PlaygroundErrorKind::Internal => "internal",
    }
}

#[utoipa::path(
    get,
    path = "/playground/limits",
    responses(
        (status = 200, description = "The bounds and resource envelope every run is subject to", body = DataEnvelope<PlaygroundLimitsDocument>),
        (status = 503, response = ProblemResponse),
        (status = 504, response = ProblemResponse)
    )
)]
pub(crate) async fn playground_limits(State(state): State<PlaygroundState>) -> Response {
    // Deliberately not origin-checked: this is a side-effect-free public
    // document with no credentials and no user data, and requiring an exact
    // origin would make it unreadable to anything but the web client.
    match state.playground.limits().await {
        Ok(limits) => Json(DataEnvelope::new(limits_document(&limits))).into_response(),
        Err(error) => playground_problem(&error),
    }
}

fn to_run(request: PlaygroundRunRequest) -> PlaygroundRun {
    PlaygroundRun {
        mode: match request.mode {
            PlaygroundModeDocument::Run => PlaygroundMode::Run,
            PlaygroundModeDocument::Build => PlaygroundMode::Build,
            PlaygroundModeDocument::Fmt => PlaygroundMode::Fmt,
        },
        profile: match request.profile {
            PlaygroundProfileDocument::Debug => PlaygroundProfile::Debug,
            PlaygroundProfileDocument::Release => PlaygroundProfile::Release,
        },
        source: request.source,
        stdin: request.stdin,
    }
}

fn run_document(result: PlaygroundResult) -> PlaygroundRunDocument {
    PlaygroundRunDocument {
        build: PlaygroundBuildDocument {
            success: result.build.success,
            diagnostics: result.build.diagnostics,
            diagnostics_truncated: result.build.diagnostics_truncated,
            duration_ms: result.build.duration_ms,
        },
        program: result.program.map(|program| PlaygroundProgramDocument {
            stdout: program.stdout,
            stdout_truncated: program.stdout_truncated,
            stderr: program.stderr,
            stderr_truncated: program.stderr_truncated,
            exit_code: program.exit_code,
            signal: program.signal,
            timed_out: program.timed_out,
            duration_ms: program.duration_ms,
        }),
        formatted: result.formatted,
    }
}

fn limits_document(limits: &PlaygroundLimits) -> PlaygroundLimitsDocument {
    PlaygroundLimitsDocument {
        max_source_bytes: as_u64(limits.max_source_bytes),
        max_stdin_bytes: as_u64(limits.max_stdin_bytes),
        max_output_bytes: as_u64(limits.max_output_bytes),
        compile_timeout_seconds: limits.compile_timeout_seconds,
        run_timeout_seconds: limits.run_timeout_seconds,
        memory_bytes: limits.memory_bytes,
        cpu_millis: limits.cpu_millis,
    }
}

/// Widens a host-sized count for the wire, where sizes are always 64-bit.
fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn playground_problem(error: &PlaygroundError) -> Response {
    match error.kind() {
        PlaygroundErrorKind::InvalidRequest => {
            invalid_playground_request(error.detail().unwrap_or("the submission was rejected"))
                .into_response()
        }
        // A malfunctioning sandbox is reported the same way as a stopped one:
        // the caller's situation is identical, and a problem must never expose
        // that something internal went wrong.
        PlaygroundErrorKind::Unavailable | PlaygroundErrorKind::Internal => unavailable(),
        PlaygroundErrorKind::Timeout => Problem::new(
            StatusCode::GATEWAY_TIMEOUT,
            "request_timeout",
            "The request exceeded the server time limit",
        )
        .into_response(),
    }
}

/// Builds the 503, which always asks for a prompt retry.
fn unavailable() -> Response {
    let mut response = Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "playground_unavailable",
        "The playground is temporarily unavailable",
    )
    .into_response();
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

fn origin_not_allowed() -> Problem {
    Problem::new(
        StatusCode::FORBIDDEN,
        "origin_not_allowed",
        "The request origin is not allowed",
    )
    .with_detail("The playground is only callable from the Rux web client.")
}

/// Builds the 422 for a submission the server will not run.
///
/// `detail` describes the shape of the problem - a size, a bound, a field - and
/// never echoes the submitted source or standard input.
fn invalid_playground_request(detail: &str) -> Problem {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_request",
        "The request is invalid",
    )
    .with_detail("The playground submission is invalid.")
    .with_errors(vec![ValidationError::new(
        "invalid_playground_request",
        detail,
    )])
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use axum::body::{Body, to_bytes};
    use axum::http::header::ORIGIN;
    use axum::http::{Method, Request};
    use rux_application::{PlaygroundBuild, PlaygroundProgram};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;

    const ORIGIN_VALUE: &str = "https://rux-lang.dev";

    struct StubPlayground {
        result: Result<PlaygroundResult, PlaygroundError>,
    }

    impl StubPlayground {
        fn returning(result: PlaygroundResult) -> Arc<Self> {
            Arc::new(Self { result: Ok(result) })
        }

        fn failing(error: PlaygroundError) -> Arc<Self> {
            Arc::new(Self { result: Err(error) })
        }
    }

    #[async_trait]
    impl PlaygroundExecution for StubPlayground {
        async fn execute(
            &self,
            _request: PlaygroundRun,
        ) -> Result<PlaygroundResult, PlaygroundError> {
            self.result.clone()
        }

        async fn limits(&self) -> Result<PlaygroundLimits, PlaygroundError> {
            match &self.result {
                Ok(_) => Ok(PlaygroundLimits::default()),
                Err(error) => Err(error.clone()),
            }
        }
    }

    fn succeeded() -> PlaygroundResult {
        PlaygroundResult {
            build: PlaygroundBuild {
                success: true,
                diagnostics: String::new(),
                diagnostics_truncated: false,
                duration_ms: 120,
            },
            program: Some(PlaygroundProgram {
                stdout: "hello\n".to_owned(),
                stdout_truncated: false,
                stderr: String::new(),
                stderr_truncated: false,
                exit_code: Some(0),
                signal: None,
                timed_out: false,
                duration_ms: 7,
            }),
            formatted: None,
        }
    }

    fn test_router(playground: Arc<dyn PlaygroundExecution>) -> Router {
        router(playground, ORIGIN_VALUE.to_owned(), Metrics::for_tests())
    }

    fn run_request(body: &str, origin: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/v1/playground/run")
            .header("content-type", "application/json");
        if let Some(origin) = origin {
            builder = builder.header(ORIGIN, origin);
        }
        builder
            .body(Body::from(body.to_owned()))
            .expect("request should build")
    }

    fn hello() -> String {
        json!({"mode": "run", "profile": "debug", "source": "Fn Main() {}\n"}).to_string()
    }

    async fn response_json(response: Response) -> Value {
        serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body should read"),
        )
        .expect("body should be JSON")
    }

    #[tokio::test]
    async fn a_successful_run_is_returned_in_a_data_envelope() {
        let response = test_router(StubPlayground::returning(succeeded()))
            .oneshot(run_request(&hello(), Some(ORIGIN_VALUE)))
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response_json(response).await,
            json!({
                "data": {
                    "build": {
                        "success": true,
                        "diagnostics": "",
                        "diagnostics_truncated": false,
                        "duration_ms": 120
                    },
                    "program": {
                        "stdout": "hello\n",
                        "stdout_truncated": false,
                        "stderr": "",
                        "stderr_truncated": false,
                        "exit_code": 0,
                        "timed_out": false,
                        "duration_ms": 7
                    }
                }
            })
        );
    }

    #[tokio::test]
    async fn a_failed_compile_is_a_200_rather_than_a_problem() {
        let failed = PlaygroundResult {
            build: PlaygroundBuild {
                success: false,
                diagnostics: "error: expected `)`".to_owned(),
                diagnostics_truncated: false,
                duration_ms: 12,
            },
            program: None,
            formatted: None,
        };

        let response = test_router(StubPlayground::returning(failed))
            .oneshot(run_request(&hello(), Some(ORIGIN_VALUE)))
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["data"]["build"]["success"], false);
        assert_eq!(body["data"]["build"]["diagnostics"], "error: expected `)`");
        assert!(
            body.get("code").is_none(),
            "a failed build is not a problem"
        );
        assert!(body["data"].get("program").is_none());
    }

    #[tokio::test]
    async fn a_run_without_the_allowed_origin_is_refused_before_any_work() {
        for origin in [None, Some("https://evil.example")] {
            let response = test_router(StubPlayground::failing(PlaygroundError::new(
                // A port that would panic the test if it were ever reached.
                PlaygroundErrorKind::Internal,
            )))
            .oneshot(run_request(&hello(), origin))
            .await
            .expect("router should respond");

            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert_eq!(response_json(response).await["code"], "origin_not_allowed");
        }
    }

    #[tokio::test]
    async fn an_unknown_field_is_refused_without_the_framework_error_body() {
        let body = json!({"source": "Fn Main() {}\n", "surprise": 1}).to_string();

        let response = test_router(StubPlayground::returning(succeeded()))
            .oneshot(run_request(&body, Some(ORIGIN_VALUE)))
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let problem = response_json(response).await;
        assert_eq!(problem["code"], "invalid_request");
        assert_eq!(problem["errors"][0]["code"], "invalid_playground_request");
    }

    #[tokio::test]
    async fn a_body_over_the_limit_is_refused_as_a_problem() {
        let body = json!({"source": "x".repeat(128 * 1024)}).to_string();

        let response = test_router(StubPlayground::returning(succeeded()))
            .oneshot(run_request(&body, Some(ORIGIN_VALUE)))
            .await
            .expect("router should respond");

        assert!(
            response.status().is_client_error(),
            "an oversized body must be refused"
        );
        let problem = response_json(response).await;
        assert_eq!(problem["code"], "invalid_request");
        assert_eq!(problem["errors"][0]["code"], "invalid_playground_request");
    }

    #[tokio::test]
    async fn mode_and_profile_default_when_omitted() {
        let body = json!({"source": "Fn Main() {}\n"}).to_string();

        let response = test_router(StubPlayground::returning(succeeded()))
            .oneshot(run_request(&body, Some(ORIGIN_VALUE)))
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_rejected_submission_reports_its_bound_without_echoing_content() {
        let error = PlaygroundError::with_detail(
            PlaygroundErrorKind::InvalidRequest,
            "source is 99999 bytes; maximum is 32768",
        );

        let response = test_router(StubPlayground::failing(error))
            .oneshot(run_request(&hello(), Some(ORIGIN_VALUE)))
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let problem = response_json(response).await;
        assert_eq!(problem["errors"][0]["code"], "invalid_playground_request");
        assert_eq!(
            problem["errors"][0]["detail"],
            "source is 99999 bytes; maximum is 32768"
        );
    }

    #[tokio::test]
    async fn an_unavailable_sandbox_is_a_503_that_asks_for_a_retry() {
        for kind in [
            PlaygroundErrorKind::Unavailable,
            PlaygroundErrorKind::Internal,
        ] {
            let response = test_router(StubPlayground::failing(PlaygroundError::new(kind)))
                .oneshot(run_request(&hello(), Some(ORIGIN_VALUE)))
                .await
                .expect("router should respond");

            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[RETRY_AFTER], "1");
            assert_eq!(
                response_json(response).await["code"],
                "playground_unavailable"
            );
        }
    }

    #[tokio::test]
    async fn an_overrun_run_is_a_504() {
        let response = test_router(StubPlayground::failing(PlaygroundError::new(
            PlaygroundErrorKind::Timeout,
        )))
        .oneshot(run_request(&hello(), Some(ORIGIN_VALUE)))
        .await
        .expect("router should respond");

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(response_json(response).await["code"], "request_timeout");
    }

    #[tokio::test]
    async fn limits_are_readable_without_an_origin_and_carry_no_side_effects() {
        let response = test_router(StubPlayground::returning(succeeded()))
            .oneshot(
                Request::builder()
                    .uri("/v1/playground/limits")
                    .body(Body::empty())
                    .expect("request should build"),
            )
            .await
            .expect("router should respond");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_json(response).await;
        assert_eq!(body["data"]["max_source_bytes"], 32_768);
        assert_eq!(body["data"]["compile_timeout_seconds"], 5);
        assert_eq!(body["data"]["memory_bytes"], 134_217_728);
    }

    #[tokio::test]
    async fn limits_report_the_sandbox_being_down() {
        let response = test_router(StubPlayground::failing(PlaygroundError::new(
            PlaygroundErrorKind::Unavailable,
        )))
        .oneshot(
            Request::builder()
                .uri("/v1/playground/limits")
                .body(Body::empty())
                .expect("request should build"),
        )
        .await
        .expect("router should respond");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response_json(response).await["code"],
            "playground_unavailable"
        );
    }

    #[test]
    fn every_problem_this_module_can_emit_constructs_without_panicking() {
        // `Problem::new` and `ValidationError::new` assert on their codes, so
        // constructing each one is what proves the codes are well formed.
        let kinds = [
            PlaygroundErrorKind::InvalidRequest,
            PlaygroundErrorKind::Unavailable,
            PlaygroundErrorKind::Timeout,
            PlaygroundErrorKind::Internal,
        ];

        for kind in kinds {
            let response = playground_problem(&PlaygroundError::new(kind));
            assert!(response.status().is_client_error() || response.status().is_server_error());
        }

        assert_eq!(
            origin_not_allowed().into_response().status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            invalid_playground_request("a bound was exceeded")
                .into_response()
                .status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }
}
