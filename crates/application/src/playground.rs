use std::error::Error;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

/// What a playground run should do with the submitted source.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlaygroundMode {
    /// Compile, then execute the resulting program.
    #[default]
    Run,
    /// Compile only, and report diagnostics.
    Build,
    /// Reformat the source and return it.
    Fmt,
}

/// Which compiler profile a playground run should build with.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PlaygroundProfile {
    /// Unoptimized build with debug assertions.
    #[default]
    Debug,
    /// Optimized build.
    Release,
}

/// One submission to the playground.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlaygroundRun {
    /// What to do with the source.
    pub mode: PlaygroundMode,
    /// Which profile to build with.
    pub profile: PlaygroundProfile,
    /// The submitted program source.
    pub source: String,
    /// Text piped to the program's standard input.
    pub stdin: String,
}

/// The bounds a submission must respect and the envelope it runs in.
///
/// The executing daemon is authoritative for these; the values reported here
/// come from it. The copy held by [`PlaygroundService`] is a local guard so an
/// oversized submission is refused before it reaches a socket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaygroundLimits {
    /// Maximum accepted source length in bytes.
    pub max_source_bytes: usize,
    /// Maximum accepted standard input length in bytes.
    pub max_stdin_bytes: usize,
    /// Maximum returned length per output stream in bytes.
    pub max_output_bytes: usize,
    /// Seconds allowed for the compile step.
    pub compile_timeout_seconds: u32,
    /// Seconds allowed for the program step.
    pub run_timeout_seconds: u32,
    /// Memory ceiling for one run, in bytes.
    pub memory_bytes: u64,
    /// CPU quota in thousandths of a core.
    pub cpu_millis: u32,
}

impl Default for PlaygroundLimits {
    /// Mirrors the sandbox's documented envelope.
    ///
    /// This exists so a guard can be constructed without a running daemon. It
    /// is not a second source of truth: what a caller is told comes from
    /// [`PlaygroundExecution::limits`].
    fn default() -> Self {
        Self {
            max_source_bytes: 32 * 1024,
            max_stdin_bytes: 16 * 1024,
            max_output_bytes: 16 * 1024,
            compile_timeout_seconds: 5,
            run_timeout_seconds: 3,
            memory_bytes: 128 * 1024 * 1024,
            cpu_millis: 500,
        }
    }
}

/// What a completed playground run produced.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlaygroundResult {
    /// How the compile step went.
    pub build: PlaygroundBuild,
    /// How the program step went, when one ran.
    pub program: Option<PlaygroundProgram>,
    /// Reformatted source, for [`PlaygroundMode::Fmt`].
    pub formatted: Option<String>,
}

/// The compile step of a run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlaygroundBuild {
    /// Whether the compiler exited successfully.
    pub success: bool,
    /// Compiler diagnostics, already bounded.
    pub diagnostics: String,
    /// Whether `diagnostics` was cut short.
    pub diagnostics_truncated: bool,
    /// Wall-clock duration of the compile step.
    pub duration_ms: u64,
}

/// The program step of a run.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PlaygroundProgram {
    /// Program standard output, already bounded.
    pub stdout: String,
    /// Whether `stdout` was cut short.
    pub stdout_truncated: bool,
    /// Program standard error, already bounded.
    pub stderr: String,
    /// Whether `stderr` was cut short.
    pub stderr_truncated: bool,
    /// Process exit code, absent when a signal ended the program.
    pub exit_code: Option<i32>,
    /// Signal that ended the program, when one did.
    pub signal: Option<i32>,
    /// Whether the program hit its own run timeout.
    pub timed_out: bool,
    /// Wall-clock duration of the program step.
    pub duration_ms: u64,
}

/// Why a playground request could not be carried out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaygroundErrorKind {
    /// The submission was refused before anything ran.
    InvalidRequest,
    /// The sandbox is saturated or not answering.
    Unavailable,
    /// The run overran its deadline.
    Timeout,
    /// Something failed that the caller cannot act on.
    Internal,
}

/// A playground request that did not produce a result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaygroundError {
    kind: PlaygroundErrorKind,
    detail: Option<String>,
}

impl PlaygroundError {
    /// Builds an error with no further explanation.
    #[must_use]
    pub const fn new(kind: PlaygroundErrorKind) -> Self {
        Self { kind, detail: None }
    }

    /// Builds an error that explains itself to the caller.
    ///
    /// The detail is shown to whoever submitted the run, so it must describe
    /// the shape of the problem - a size, a count, an offset - and must never
    /// echo the submitted source, standard input, or any host detail.
    #[must_use]
    pub fn with_detail(kind: PlaygroundErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: Some(detail.into()),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> PlaygroundErrorKind {
        self.kind
    }

    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

impl fmt::Display for PlaygroundError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "playground run failed: {:?}", self.kind)
    }
}

impl Error for PlaygroundError {}

/// Runs playground submissions somewhere the API cannot reach directly.
#[async_trait]
pub trait PlaygroundExecution: Send + Sync {
    /// Compiles and optionally runs one submission.
    ///
    /// # Errors
    ///
    /// Returns a [`PlaygroundError`] when the submission is refused or the
    /// sandbox could not carry it out. A failed *compile* is a successful call
    /// reporting `build.success == false`, not an error.
    async fn execute(&self, request: PlaygroundRun) -> Result<PlaygroundResult, PlaygroundError>;

    /// Reports the bounds submissions are subject to.
    ///
    /// # Errors
    ///
    /// Returns a [`PlaygroundError`] when the sandbox cannot be reached.
    async fn limits(&self) -> Result<PlaygroundLimits, PlaygroundError>;
}

/// Applies the application layer's own bounds before delegating a run.
///
/// The HTTP layer has a body limit of its own, but it must not be the only
/// gate: this refuses an oversized or empty submission before it can occupy a
/// socket connection or a sandbox permit.
pub struct PlaygroundService {
    execution: Arc<dyn PlaygroundExecution>,
    guard: PlaygroundLimits,
}

impl PlaygroundService {
    #[must_use]
    pub fn new(execution: Arc<dyn PlaygroundExecution>, guard: PlaygroundLimits) -> Self {
        Self { execution, guard }
    }

    /// Checks a submission against the guard bounds.
    ///
    /// `mode` and `profile` need no check: they are enums, so an unrepresentable
    /// value cannot reach here. The full lexical rule for source - which control
    /// characters are permitted - stays with the sandbox that writes the file;
    /// what matters here is that nothing oversized or obviously malformed
    /// travels further.
    fn validate(&self, request: &PlaygroundRun) -> Result<(), PlaygroundError> {
        if request.source.is_empty() {
            return Err(PlaygroundError::with_detail(
                PlaygroundErrorKind::InvalidRequest,
                "source cannot be empty",
            ));
        }

        if request.source.len() > self.guard.max_source_bytes {
            return Err(too_large(
                "source",
                request.source.len(),
                self.guard.max_source_bytes,
            ));
        }

        if request.stdin.len() > self.guard.max_stdin_bytes {
            return Err(too_large(
                "standard input",
                request.stdin.len(),
                self.guard.max_stdin_bytes,
            ));
        }

        if request.source.contains('\0') || request.stdin.contains('\0') {
            return Err(PlaygroundError::with_detail(
                PlaygroundErrorKind::InvalidRequest,
                "submission contains a NUL byte",
            ));
        }

        Ok(())
    }
}

#[async_trait]
impl PlaygroundExecution for PlaygroundService {
    async fn execute(&self, request: PlaygroundRun) -> Result<PlaygroundResult, PlaygroundError> {
        self.validate(&request)?;
        self.execution.execute(request).await
    }

    async fn limits(&self) -> Result<PlaygroundLimits, PlaygroundError> {
        self.execution.limits().await
    }
}

fn too_large(what: &str, bytes: usize, maximum: usize) -> PlaygroundError {
    PlaygroundError::with_detail(
        PlaygroundErrorKind::InvalidRequest,
        format!("{what} is {bytes} bytes; maximum is {maximum}"),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::{
        PlaygroundBuild, PlaygroundError, PlaygroundErrorKind, PlaygroundExecution,
        PlaygroundLimits, PlaygroundMode, PlaygroundProfile, PlaygroundResult, PlaygroundRun,
        PlaygroundService,
    };

    /// A port that records whether it was reached.
    struct StubExecution {
        outcome: Result<PlaygroundResult, PlaygroundError>,
        reached: AtomicUsize,
    }

    impl StubExecution {
        fn ok() -> Arc<Self> {
            Self::new(Ok(PlaygroundResult {
                build: PlaygroundBuild {
                    success: true,
                    ..PlaygroundBuild::default()
                },
                ..PlaygroundResult::default()
            }))
        }

        fn failing(kind: PlaygroundErrorKind) -> Arc<Self> {
            Self::new(Err(PlaygroundError::new(kind)))
        }

        fn new(outcome: Result<PlaygroundResult, PlaygroundError>) -> Arc<Self> {
            Arc::new(Self {
                outcome,
                reached: AtomicUsize::new(0),
            })
        }

        fn reached(&self) -> usize {
            self.reached.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl PlaygroundExecution for StubExecution {
        async fn execute(
            &self,
            _request: PlaygroundRun,
        ) -> Result<PlaygroundResult, PlaygroundError> {
            self.reached.fetch_add(1, Ordering::SeqCst);
            self.outcome.clone()
        }

        async fn limits(&self) -> Result<PlaygroundLimits, PlaygroundError> {
            self.reached.fetch_add(1, Ordering::SeqCst);
            Ok(PlaygroundLimits {
                max_source_bytes: 4_096,
                ..PlaygroundLimits::default()
            })
        }
    }

    fn guard() -> PlaygroundLimits {
        PlaygroundLimits {
            max_source_bytes: 64,
            max_stdin_bytes: 16,
            ..PlaygroundLimits::default()
        }
    }

    fn run(source: &str) -> PlaygroundRun {
        PlaygroundRun {
            mode: PlaygroundMode::Run,
            profile: PlaygroundProfile::Debug,
            source: source.to_owned(),
            stdin: String::new(),
        }
    }

    #[tokio::test]
    async fn a_valid_submission_reaches_the_port_and_returns_its_result() {
        let port = StubExecution::ok();
        let service = PlaygroundService::new(Arc::clone(&port) as Arc<_>, guard());

        let result = service
            .execute(run("Fn Main() {}\n"))
            .await
            .expect("a valid submission should run");

        assert!(result.build.success);
        assert_eq!(port.reached(), 1);
    }

    #[tokio::test]
    async fn an_empty_submission_is_refused_without_reaching_the_port() {
        let port = StubExecution::ok();
        let service = PlaygroundService::new(Arc::clone(&port) as Arc<_>, guard());

        let error = service
            .execute(run(""))
            .await
            .expect_err("an empty submission should be refused");

        assert_eq!(error.kind(), PlaygroundErrorKind::InvalidRequest);
        assert_eq!(error.detail(), Some("source cannot be empty"));
        assert_eq!(port.reached(), 0, "nothing may reach the sandbox");
    }

    #[tokio::test]
    async fn an_oversized_source_is_refused_with_its_measurements() {
        let port = StubExecution::ok();
        let service = PlaygroundService::new(Arc::clone(&port) as Arc<_>, guard());

        let error = service
            .execute(run(&"x".repeat(65)))
            .await
            .expect_err("an oversized source should be refused");

        assert_eq!(error.kind(), PlaygroundErrorKind::InvalidRequest);
        assert_eq!(error.detail(), Some("source is 65 bytes; maximum is 64"));
        assert_eq!(port.reached(), 0);
    }

    #[tokio::test]
    async fn an_oversized_standard_input_is_refused() {
        let port = StubExecution::ok();
        let service = PlaygroundService::new(Arc::clone(&port) as Arc<_>, guard());
        let mut request = run("Fn Main() {}\n");
        request.stdin = "s".repeat(17);

        let error = service
            .execute(request)
            .await
            .expect_err("an oversized standard input should be refused");

        assert_eq!(
            error.detail(),
            Some("standard input is 17 bytes; maximum is 16")
        );
        assert_eq!(port.reached(), 0);
    }

    #[tokio::test]
    async fn a_nul_byte_in_either_field_is_refused() {
        for (source, stdin) in [("a\0b", ""), ("Fn Main() {}\n", "a\0b")] {
            let port = StubExecution::ok();
            let service = PlaygroundService::new(Arc::clone(&port) as Arc<_>, guard());
            let mut request = run(source);
            request.stdin = stdin.to_owned();

            let error = service
                .execute(request)
                .await
                .expect_err("a NUL byte should be refused");

            assert_eq!(error.kind(), PlaygroundErrorKind::InvalidRequest);
            assert_eq!(port.reached(), 0);
        }
    }

    #[tokio::test]
    async fn a_submission_at_exactly_the_bound_is_accepted() {
        let port = StubExecution::ok();
        let service = PlaygroundService::new(Arc::clone(&port) as Arc<_>, guard());
        let mut request = run(&"x".repeat(64));
        request.stdin = "s".repeat(16);

        assert!(service.execute(request).await.is_ok());
        assert_eq!(port.reached(), 1);
    }

    #[tokio::test]
    async fn the_size_bound_counts_bytes_rather_than_characters() {
        let port = StubExecution::ok();
        let service = PlaygroundService::new(Arc::clone(&port) as Arc<_>, guard());

        // 25 characters, 75 bytes.
        let error = service
            .execute(run(&"日".repeat(25)))
            .await
            .expect_err("a multi-byte source over the bound should be refused");

        assert_eq!(error.detail(), Some("source is 75 bytes; maximum is 64"));
    }

    #[tokio::test]
    async fn every_port_failure_kind_is_passed_through_unchanged() {
        for kind in [
            PlaygroundErrorKind::InvalidRequest,
            PlaygroundErrorKind::Unavailable,
            PlaygroundErrorKind::Timeout,
            PlaygroundErrorKind::Internal,
        ] {
            let service = PlaygroundService::new(StubExecution::failing(kind) as Arc<_>, guard());

            let error = service
                .execute(run("Fn Main() {}\n"))
                .await
                .expect_err("the port failure should surface");

            assert_eq!(error.kind(), kind);
        }
    }

    #[tokio::test]
    async fn limits_are_reported_by_the_sandbox_rather_than_by_the_local_guard() {
        let service = PlaygroundService::new(StubExecution::ok() as Arc<_>, guard());

        let limits = service.limits().await.expect("limits should be reported");

        assert_eq!(limits.max_source_bytes, 4_096);
        assert_ne!(limits.max_source_bytes, guard().max_source_bytes);
    }

    #[test]
    fn an_error_renders_its_kind_without_its_detail() {
        let error = PlaygroundError::with_detail(
            PlaygroundErrorKind::InvalidRequest,
            "source is 99 bytes; maximum is 64",
        );

        assert_eq!(error.to_string(), "playground run failed: InvalidRequest");
    }
}
