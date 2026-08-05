// On a host without Unix sockets the adapter can only ever report the
// playground as unavailable, so the socket path and the connect path are
// legitimately unreferenced there. Everything else still compiles and is still
// unit-tested on every platform.
#![cfg_attr(not(unix), allow(dead_code, clippy::unused_async))]

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use rux_application::{
    PlaygroundBuild, PlaygroundError, PlaygroundErrorKind, PlaygroundExecution, PlaygroundLimits,
    PlaygroundMode, PlaygroundProfile, PlaygroundProgram, PlaygroundResult, PlaygroundRun,
};
use rux_sandbox::protocol::{
    FailureKind, ProtocolError, Request, Response, read_frame, write_frame,
};
use rux_sandbox::{SandboxLimits, SandboxMode, SandboxOutcome, SandboxProfile, SandboxRequest};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::time::timeout;

/// Talks to `rux-playgroundd` over its Unix socket.
///
/// A connection carries exactly one request, matching the daemon's own model,
/// so there is no pool to keep warm and a wedged exchange cannot poison a later
/// one.
#[derive(Clone)]
pub struct UnixSocketPlayground {
    socket_path: PathBuf,
    deadline: Duration,
}

impl UnixSocketPlayground {
    /// Creates an adapter for the daemon listening at `socket_path`.
    ///
    /// `deadline` bounds the connect and the round trip together, so a daemon
    /// that accepts and then stalls cannot hold an API request open.
    #[must_use]
    pub const fn new(socket_path: PathBuf, deadline: Duration) -> Self {
        Self {
            socket_path,
            deadline,
        }
    }

    /// Carries out one request/response exchange.
    async fn exchange(&self, request: &Request) -> Result<Response, PlaygroundError> {
        #[cfg(unix)]
        {
            let connected = timeout(
                self.deadline,
                tokio::net::UnixStream::connect(&self.socket_path),
            )
            .await;

            let stream = match connected {
                Ok(Ok(stream)) => stream,
                // Every way of failing to reach the socket - absent, refused,
                // not permitted - means the daemon is not serving. None of them
                // is something the caller did, so they share one kind.
                Ok(Err(_)) => return Err(PlaygroundError::new(PlaygroundErrorKind::Unavailable)),
                Err(_elapsed) => return Err(PlaygroundError::new(PlaygroundErrorKind::Timeout)),
            };

            exchange_over(stream, request, self.deadline).await
        }

        #[cfg(not(unix))]
        {
            // The daemon is a Unix-socket service; a host without them has no
            // playground rather than a broken one.
            let _ = request;
            Err(PlaygroundError::new(PlaygroundErrorKind::Unavailable))
        }
    }
}

impl fmt::Debug for UnixSocketPlayground {
    /// Deliberately opaque: the socket path is a host detail, and this type is
    /// reachable from request-scoped state that may be logged.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UnixSocketPlayground")
    }
}

#[async_trait]
impl PlaygroundExecution for UnixSocketPlayground {
    async fn execute(&self, request: PlaygroundRun) -> Result<PlaygroundResult, PlaygroundError> {
        let response = self
            .exchange(&Request::Run {
                request: Box::new(to_submission(request)),
            })
            .await?;

        result_from(response)
    }

    async fn limits(&self) -> Result<PlaygroundLimits, PlaygroundError> {
        limits_from(self.exchange(&Request::Limits).await?)
    }
}

/// Reads a run's answer, refusing one that does not match the question asked.
fn result_from(response: Response) -> Result<PlaygroundResult, PlaygroundError> {
    match response {
        Response::Completed { outcome } => Ok(to_result(*outcome)),
        Response::Failed { kind, detail } => Err(to_error(kind, detail)),
        // A daemon that answers a run with limits is malfunctioning, and there
        // is nothing the caller can do about it.
        Response::Limits { .. } => Err(PlaygroundError::new(PlaygroundErrorKind::Internal)),
    }
}

/// Reads a limits answer, refusing one that does not match the question asked.
fn limits_from(response: Response) -> Result<PlaygroundLimits, PlaygroundError> {
    match response {
        Response::Limits { limits } => Ok(to_limits(limits)),
        Response::Failed { kind, detail } => Err(to_error(kind, detail)),
        Response::Completed { .. } => Err(PlaygroundError::new(PlaygroundErrorKind::Internal)),
    }
}

/// Writes one frame and reads one back, under a shared deadline.
async fn exchange_over<S>(
    stream: S,
    request: &Request,
    deadline: Duration,
) -> Result<Response, PlaygroundError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let exchange = async {
        let (read_half, mut write_half) = tokio::io::split(stream);
        write_frame(&mut write_half, request).await?;

        let mut reader = BufReader::new(read_half);
        read_frame(&mut reader).await
    };

    match timeout(deadline, exchange).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(exchange_failure(&error)),
        Err(_elapsed) => Err(PlaygroundError::new(PlaygroundErrorKind::Timeout)),
    }
}

/// Maps a failure part-way through an exchange.
fn exchange_failure(error: &ProtocolError) -> PlaygroundError {
    let kind = match error {
        // The daemon went away mid-exchange; a retry may well succeed.
        ProtocolError::Closed | ProtocolError::Truncated | ProtocolError::Io(_) => {
            PlaygroundErrorKind::Unavailable
        }
        // The daemon is answering, but not with something we can act on. The
        // caller cannot fix this, so it must not read as a client error.
        ProtocolError::Malformed | ProtocolError::TooLarge => PlaygroundErrorKind::Internal,
    };

    PlaygroundError::new(kind)
}

/// Maps a typed daemon failure onto the application's vocabulary.
fn to_error(kind: FailureKind, detail: Option<String>) -> PlaygroundError {
    let kind = match kind {
        FailureKind::InvalidRequest => PlaygroundErrorKind::InvalidRequest,
        FailureKind::Unavailable => PlaygroundErrorKind::Unavailable,
        FailureKind::Timeout => PlaygroundErrorKind::Timeout,
        FailureKind::Internal => PlaygroundErrorKind::Internal,
    };

    // Only a rejected submission explains itself, and that explanation carries
    // sizes and offsets rather than any part of the submission.
    match detail.filter(|_| kind == PlaygroundErrorKind::InvalidRequest) {
        Some(detail) => PlaygroundError::with_detail(kind, detail),
        None => PlaygroundError::new(kind),
    }
}

fn to_submission(request: PlaygroundRun) -> SandboxRequest {
    SandboxRequest {
        mode: match request.mode {
            PlaygroundMode::Run => SandboxMode::Run,
            PlaygroundMode::Build => SandboxMode::Build,
            PlaygroundMode::Fmt => SandboxMode::Fmt,
        },
        profile: match request.profile {
            PlaygroundProfile::Debug => SandboxProfile::Debug,
            PlaygroundProfile::Release => SandboxProfile::Release,
        },
        source: request.source,
        stdin: request.stdin,
    }
}

fn to_result(outcome: SandboxOutcome) -> PlaygroundResult {
    PlaygroundResult {
        build: PlaygroundBuild {
            success: outcome.build.success,
            diagnostics: outcome.build.diagnostics,
            diagnostics_truncated: outcome.build.diagnostics_truncated,
            duration_ms: outcome.build.duration_ms,
        },
        program: outcome.program.map(|program| PlaygroundProgram {
            stdout: program.stdout,
            stdout_truncated: program.stdout_truncated,
            stderr: program.stderr,
            stderr_truncated: program.stderr_truncated,
            exit_code: program.exit_code,
            signal: program.signal,
            timed_out: program.timed_out,
            duration_ms: program.duration_ms,
        }),
        formatted: outcome.formatted,
    }
}

fn to_limits(limits: SandboxLimits) -> PlaygroundLimits {
    PlaygroundLimits {
        max_source_bytes: limits.max_source_bytes,
        max_stdin_bytes: limits.max_stdin_bytes,
        max_output_bytes: limits.max_output_bytes,
        compile_timeout_seconds: limits.compile_timeout_seconds,
        run_timeout_seconds: limits.run_timeout_seconds,
        memory_bytes: limits.memory_bytes,
        cpu_millis: limits.cpu_millis,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rux_application::{PlaygroundErrorKind, PlaygroundMode, PlaygroundProfile, PlaygroundRun};
    use rux_sandbox::protocol::{FailureKind, Request, Response, read_frame, write_frame};
    use rux_sandbox::{
        BuildOutcome, ProgramOutcome, SandboxLimits, SandboxMode, SandboxOutcome, SandboxProfile,
    };
    use tokio::io::BufReader;

    use super::{
        UnixSocketPlayground, exchange_over, limits_from, result_from, to_error, to_limits,
        to_result, to_submission,
    };

    fn run() -> PlaygroundRun {
        PlaygroundRun {
            mode: PlaygroundMode::Fmt,
            profile: PlaygroundProfile::Release,
            source: "Fn Main() {}\n".to_owned(),
            stdin: "input\n".to_owned(),
        }
    }

    /// Runs one exchange against a scripted daemon and returns both sides.
    async fn against(
        request: Request,
        answer: Option<Response>,
    ) -> (Result<Response, rux_application::PlaygroundError>, Request) {
        let (client, server) = tokio::io::duplex(1024 * 1024);

        let daemon = tokio::spawn(async move {
            let (read_half, mut write_half) = tokio::io::split(server);
            let mut reader = BufReader::new(read_half);
            let received: Request = read_frame(&mut reader)
                .await
                .expect("a frame should arrive");
            if let Some(answer) = answer {
                write_frame(&mut write_half, &answer)
                    .await
                    .expect("the answer should write");
            }
            received
        });

        let outcome = exchange_over(client, &request, Duration::from_secs(5)).await;
        let received = daemon.await.expect("the daemon should finish");

        (outcome, received)
    }

    #[tokio::test]
    async fn a_request_reaches_the_daemon_and_its_answer_comes_back() {
        let answer = Response::Limits {
            limits: SandboxLimits::default(),
        };

        let (outcome, received) = against(Request::Limits, Some(answer.clone())).await;

        assert_eq!(received, Request::Limits);
        assert_eq!(outcome.expect("the answer should arrive"), answer);
    }

    #[tokio::test]
    async fn a_daemon_that_answers_nothing_reads_as_unavailable() {
        let (outcome, _) = against(Request::Limits, None).await;

        assert_eq!(
            outcome.expect_err("a silent daemon should fail").kind(),
            PlaygroundErrorKind::Unavailable
        );
    }

    #[tokio::test]
    async fn a_daemon_that_stalls_is_cut_off_by_the_deadline() {
        let (client, server) = tokio::io::duplex(1024);
        // Held open, never answered.
        let _server = server;

        let outcome = exchange_over(client, &Request::Limits, Duration::from_millis(50)).await;

        assert_eq!(
            outcome.expect_err("a stalled daemon should fail").kind(),
            PlaygroundErrorKind::Timeout
        );
    }

    #[test]
    fn a_daemon_answering_the_wrong_question_is_an_internal_failure() {
        // Well-formed frames, but not answers to what was asked. The caller can
        // do nothing about a malfunctioning daemon, so neither may read as a
        // client error.
        let mismatched_run = result_from(Response::Limits {
            limits: SandboxLimits::default(),
        });
        let mismatched_limits = limits_from(Response::Completed {
            outcome: Box::new(SandboxOutcome::default()),
        });

        assert_eq!(
            mismatched_run
                .expect_err("limits are not a run result")
                .kind(),
            PlaygroundErrorKind::Internal
        );
        assert_eq!(
            mismatched_limits
                .expect_err("a run result is not limits")
                .kind(),
            PlaygroundErrorKind::Internal
        );
    }

    #[test]
    fn a_matching_answer_is_unwrapped_on_both_paths() {
        assert!(
            result_from(Response::Completed {
                outcome: Box::new(SandboxOutcome::default()),
            })
            .is_ok()
        );
        assert!(
            limits_from(Response::Limits {
                limits: SandboxLimits::default(),
            })
            .is_ok()
        );
    }

    #[test]
    fn a_failure_frame_surfaces_on_both_paths() {
        let failed = || Response::Failed {
            kind: FailureKind::Timeout,
            detail: None,
        };

        assert_eq!(
            result_from(failed())
                .expect_err("a failure should surface")
                .kind(),
            PlaygroundErrorKind::Timeout
        );
        assert_eq!(
            limits_from(failed())
                .expect_err("a failure should surface")
                .kind(),
            PlaygroundErrorKind::Timeout
        );
    }

    #[test]
    fn every_daemon_failure_kind_maps_onto_an_application_kind() {
        for (wire, expected) in [
            (
                FailureKind::InvalidRequest,
                PlaygroundErrorKind::InvalidRequest,
            ),
            (FailureKind::Unavailable, PlaygroundErrorKind::Unavailable),
            (FailureKind::Timeout, PlaygroundErrorKind::Timeout),
            (FailureKind::Internal, PlaygroundErrorKind::Internal),
        ] {
            assert_eq!(to_error(wire, None).kind(), expected);
        }
    }

    #[test]
    fn only_a_rejected_submission_carries_its_explanation_onward() {
        let rejected = to_error(
            FailureKind::InvalidRequest,
            Some("source is 99 bytes; maximum is 64".to_owned()),
        );
        assert_eq!(rejected.detail(), Some("source is 99 bytes; maximum is 64"));

        // A daemon that attaches a detail to anything else must not have it
        // relayed: only the validation path is known to be content-free.
        for kind in [
            FailureKind::Unavailable,
            FailureKind::Timeout,
            FailureKind::Internal,
        ] {
            assert_eq!(
                to_error(kind, Some("/var/lib/rux-playground/jobs/abc".to_owned())).detail(),
                None
            );
        }
    }

    #[test]
    fn a_submission_keeps_its_mode_profile_and_payload_across_the_boundary() {
        let submission = to_submission(run());

        assert_eq!(submission.mode, SandboxMode::Fmt);
        assert_eq!(submission.profile, SandboxProfile::Release);
        assert_eq!(submission.source, "Fn Main() {}\n");
        assert_eq!(submission.stdin, "input\n");
    }

    #[test]
    fn every_mode_and_profile_has_a_counterpart() {
        for (mode, expected) in [
            (PlaygroundMode::Run, SandboxMode::Run),
            (PlaygroundMode::Build, SandboxMode::Build),
            (PlaygroundMode::Fmt, SandboxMode::Fmt),
        ] {
            let mut request = run();
            request.mode = mode;
            assert_eq!(to_submission(request).mode, expected);
        }

        for (profile, expected) in [
            (PlaygroundProfile::Debug, SandboxProfile::Debug),
            (PlaygroundProfile::Release, SandboxProfile::Release),
        ] {
            let mut request = run();
            request.profile = profile;
            assert_eq!(to_submission(request).profile, expected);
        }
    }

    #[test]
    fn an_outcome_survives_the_boundary_field_for_field() {
        let outcome = SandboxOutcome {
            build: BuildOutcome {
                success: true,
                diagnostics: "warning: unused".to_owned(),
                diagnostics_truncated: true,
                duration_ms: 120,
            },
            program: Some(ProgramOutcome {
                stdout: "hello".to_owned(),
                stdout_truncated: false,
                stderr: "oops".to_owned(),
                stderr_truncated: true,
                exit_code: Some(3),
                signal: None,
                timed_out: true,
                duration_ms: 8,
            }),
            formatted: Some("Fn Main() {}\n".to_owned()),
        };

        let result = to_result(outcome);

        assert!(result.build.success);
        assert_eq!(result.build.diagnostics, "warning: unused");
        assert!(result.build.diagnostics_truncated);
        assert_eq!(result.build.duration_ms, 120);
        let program = result.program.expect("a program should be reported");
        assert_eq!(program.stdout, "hello");
        assert_eq!(program.stderr, "oops");
        assert!(program.stderr_truncated);
        assert_eq!(program.exit_code, Some(3));
        assert!(program.timed_out);
        assert_eq!(program.duration_ms, 8);
        assert_eq!(result.formatted.as_deref(), Some("Fn Main() {}\n"));
    }

    #[test]
    fn an_absent_program_stays_absent() {
        assert!(to_result(SandboxOutcome::default()).program.is_none());
    }

    #[test]
    fn limits_survive_the_boundary_field_for_field() {
        let sandbox = SandboxLimits::default();
        let limits = to_limits(sandbox);

        assert_eq!(limits.max_source_bytes, sandbox.max_source_bytes);
        assert_eq!(limits.max_stdin_bytes, sandbox.max_stdin_bytes);
        assert_eq!(limits.max_output_bytes, sandbox.max_output_bytes);
        assert_eq!(
            limits.compile_timeout_seconds,
            sandbox.compile_timeout_seconds
        );
        assert_eq!(limits.run_timeout_seconds, sandbox.run_timeout_seconds);
        assert_eq!(limits.memory_bytes, sandbox.memory_bytes);
        assert_eq!(limits.cpu_millis, sandbox.cpu_millis);
    }

    #[tokio::test]
    async fn a_socket_that_is_not_there_reads_as_unavailable_without_naming_it() {
        use rux_application::PlaygroundExecution as _;

        let socket = UnixSocketPlayground::new(
            "/nonexistent/rux-playground.sock".into(),
            Duration::from_millis(200),
        );

        let error = socket
            .limits()
            .await
            .expect_err("a missing socket should fail");

        assert_eq!(error.kind(), PlaygroundErrorKind::Unavailable);
        assert!(error.detail().is_none());
        assert!(!format!("{socket:?}").contains("nonexistent"));
    }
}
