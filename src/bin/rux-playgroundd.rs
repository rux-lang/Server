#![doc = "The playground sandbox daemon."]
// On a host without Unix sockets only the stub `main` is reachable, so the
// configuration and connection handling below are legitimately unreferenced
// there. They still compile and are still unit-tested on every platform.
#![cfg_attr(not(unix), allow(dead_code))]
//!
//! This process exists only because docker-socket access is root-equivalent.
//! Keeping it out of `rux-server` preserves the hardening that protects the
//! package registry and its database on the same host: the API reaches the
//! sandbox through a Unix socket and never talks to a container runtime itself.
//!
//! The exchange is one newline-terminated JSON frame each way, defined in
//! `rux_sandbox::protocol`. Submitted source is never logged.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use rux_sandbox::protocol::{
    FailureKind, ProtocolError, Request, Response, read_frame, write_frame,
};
use rux_sandbox::{
    DockerSandbox, DockerSandboxConfig, IdentitySegment, PackageAllowlist, SandboxError,
    SandboxLimits, SandboxOutcome, SandboxRequest,
};
use rux_server::config::{self, ConfigError, ConfigFile};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::Semaphore;
use tokio::time::timeout;

/// Default path of the socket the API connects to.
#[cfg(test)]
const DEFAULT_SOCKET: &str = "/run/rux-playground/run.sock";
/// Permissions the socket is published with: the `rux-server` user reaches it
/// through a shared group, and nothing else on the host may.
#[cfg(unix)]
const SOCKET_MODE: u32 = 0o660;

fn main() -> ExitCode {
    #[cfg(unix)]
    {
        unix::main()
    }

    #[cfg(not(unix))]
    {
        eprintln!("rux-playgroundd requires a Unix host with a container runtime");
        ExitCode::FAILURE
    }
}

/// How the daemon is wired to its host.
#[derive(Clone, Debug)]
struct DaemonConfig {
    socket_path: PathBuf,
    sandbox: DockerSandboxConfig,
    max_concurrency: usize,
    request_timeout: Duration,
}

impl DaemonConfig {
    /// Reads, deserializes, and validates the configuration at `path`.
    fn load(path: &Path) -> Result<Self, ConfigError> {
        let file = config::load(path)?;
        Self::from_file(&file).map_err(|detail| ConfigError::invalid(path, detail))
        // The whole document, including the registry's credentials, is dropped
        // here: only the broker's own settings outlive this call.
    }

    /// Validates the daemon's own sections of an already-parsed document.
    fn from_file(file: &ConfigFile) -> Result<Self, String> {
        let broker = file
            .playground
            .broker
            .as_ref()
            .ok_or("playground.broker is required to run the sandbox broker")?;

        let image = config::required_text("playground.broker.image", broker.image.as_deref())?;

        let limits = SandboxLimits {
            memory_bytes: config::bounded(
                "playground.broker.limits.memory_bytes",
                broker.limits.memory_bytes,
                16 * 1024 * 1024,
                4 * 1024 * 1024 * 1024,
            )?,
            cpu_millis: bounded_u32(
                "playground.broker.limits.cpu_millis",
                broker.limits.cpu_millis,
                100,
                16_000,
            )?,
            compile_timeout_seconds: bounded_u32(
                "playground.broker.limits.compile_timeout_seconds",
                broker.limits.compile_timeout_seconds,
                1,
                120,
            )?,
            run_timeout_seconds: bounded_u32(
                "playground.broker.limits.run_timeout_seconds",
                broker.limits.run_timeout_seconds,
                1,
                120,
            )?,
            ..SandboxLimits::default()
        };
        // Reject a nonsensical combination here rather than on the first run.
        limits
            .validate()
            .map_err(|error| format!("playground.broker.limits are not usable: {error}"))?;

        let socket = file.playground.socket.trim();
        if socket.is_empty() {
            return Err("playground.socket is required".to_owned());
        }

        Ok(Self {
            socket_path: PathBuf::from(socket),
            sandbox: DockerSandboxConfig {
                jobs_root: broker.jobs_root.clone(),
                image,
                docker_binary: broker.docker_binary.clone(),
                limits,
                allowlist: parse_allowlist(&broker.packages)?,
                probe_timeout: Duration::from_secs(2),
            },
            max_concurrency: usize::try_from(config::bounded(
                "playground.broker.max_concurrency",
                broker.max_concurrency,
                1,
                16,
            )?)
            .expect("the configured upper bound fits usize"),
            request_timeout: Duration::from_secs(config::bounded(
                "playground.broker.request_timeout_seconds",
                broker.request_timeout_seconds,
                1,
                120,
            )?),
        })
    }
}

/// Range-checks a setting whose target type is narrower than the schema's.
fn bounded_u32(key: &str, value: u64, minimum: u64, maximum: u64) -> Result<u32, String> {
    let checked = config::bounded(key, value, minimum, maximum)?;
    Ok(u32::try_from(checked).expect("the configured upper bound fits u32"))
}

/// Parses `Root:Namespace` pairs into the import allowlist.
///
/// The sandbox has no network, so this may only name packages already seeded
/// into the image; adding one means rebuilding the image.
fn parse_allowlist(entries: &[String]) -> Result<PackageAllowlist, String> {
    let mut parsed = Vec::new();

    for entry in entries.iter().map(|entry| entry.trim()) {
        let invalid =
            || format!("playground.broker.packages entry {entry:?} must be \"Root:Namespace\"");
        let (root, namespace) = entry.split_once(':').ok_or_else(invalid)?;
        parsed.push((
            IdentitySegment::new(root.trim()).map_err(|_| invalid())?,
            IdentitySegment::new(namespace.trim()).map_err(|_| invalid())?,
        ));
    }

    Ok(PackageAllowlist::new(parsed))
}

/// What the daemon can do with a submission.
///
/// A trait so the connection handling can be exercised without a container
/// runtime, and so a host without Docker can still run those tests.
trait JobRunner: Send + Sync + 'static {
    fn limits(&self) -> SandboxLimits;

    fn execute(
        &self,
        request: &SandboxRequest,
    ) -> impl Future<Output = Result<SandboxOutcome, SandboxError>> + Send;
}

impl JobRunner for DockerSandbox {
    fn limits(&self) -> SandboxLimits {
        *DockerSandbox::limits(self)
    }

    fn execute(
        &self,
        request: &SandboxRequest,
    ) -> impl Future<Output = Result<SandboxOutcome, SandboxError>> + Send {
        DockerSandbox::execute(self, request)
    }
}

/// Serves exactly one request on one connection.
///
/// The API opens a connection per request, so there is no keep-alive loop to
/// hold a permit across an idle peer.
async fn serve_connection<S, R>(
    stream: S,
    runner: Arc<R>,
    permits: Arc<Semaphore>,
    deadline: Duration,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send,
    R: JobRunner,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);

    let response = match timeout(deadline, read_frame::<_, Request>(&mut reader)).await {
        Ok(Ok(request)) => dispatch(request, &runner, &permits).await,
        Ok(Err(ProtocolError::Closed)) => {
            // The peer went away without asking anything; nothing to answer.
            return;
        }
        Ok(Err(error)) => {
            tracing::warn!(reason = %error, "rejected a malformed playground frame");
            Response::failed(FailureKind::InvalidRequest, None)
        }
        Err(_elapsed) => {
            tracing::warn!("a playground peer did not send a frame in time");
            Response::failed(FailureKind::Timeout, None)
        }
    };

    // A peer that will not read its answer must not hold the connection open.
    if let Err(error) = timeout(deadline, write_frame(&mut write_half, &response))
        .await
        .unwrap_or(Err(ProtocolError::Truncated))
    {
        tracing::warn!(reason = %error, "could not answer a playground peer");
    }
}

/// Carries out one request.
async fn dispatch<R: JobRunner>(
    request: Request,
    runner: &Arc<R>,
    permits: &Arc<Semaphore>,
) -> Response {
    match request {
        Request::Limits => Response::Limits {
            limits: runner.limits(),
        },
        Request::Run { request } => {
            // Saturation answers immediately rather than queueing, so a caller
            // learns to back off instead of waiting behind a full runway.
            let Ok(_permit) = Arc::clone(permits).try_acquire_owned() else {
                tracing::warn!("refused a playground run because the daemon is saturated");
                return Response::failed(FailureKind::Unavailable, None);
            };

            let mode = request.mode.as_arg();
            match runner.execute(&request).await {
                Ok(outcome) => {
                    tracing::info!(
                        mode,
                        build_succeeded = outcome.build.success,
                        build_ms = outcome.build.duration_ms,
                        "completed a playground run"
                    );
                    Response::Completed {
                        outcome: Box::new(outcome),
                    }
                }
                Err(error) => {
                    let response = failure_for(&error);
                    tracing::warn!(mode, ?error, "a playground run failed");
                    response
                }
            }
        }
    }
}

/// Maps a sandbox failure onto the wire.
///
/// Only validation failures carry a detail, and those carry offsets and sizes
/// rather than any part of the submission.
fn failure_for(error: &SandboxError) -> Response {
    match error {
        SandboxError::InvalidSource(reason) | SandboxError::InvalidStdin(reason) => {
            Response::failed(FailureKind::InvalidRequest, Some(&reason.to_string()))
        }
        SandboxError::Timeout => Response::failed(FailureKind::Timeout, None),
        SandboxError::Unavailable | SandboxError::Runtime(_) => {
            Response::failed(FailureKind::Unavailable, None)
        }
        SandboxError::InvalidLimits(_)
        | SandboxError::JobDirectory(_)
        | SandboxError::Framing(_)
        | SandboxError::Internal => Response::failed(FailureKind::Internal, None),
    }
}

/// Shared with the API so both processes emit the same JSON log shape.
///
/// Already-installed is not an error here: the daemon logs either way.
fn init_logging() {
    let _ = rux_server::logging::init();
}

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::process::ExitCode;
    use std::sync::Arc;

    use tokio::net::{UnixListener, UnixStream};
    use tokio::signal::unix::{SignalKind, signal};
    use tokio::sync::Semaphore;

    // Imported here rather than at the top of the file: on a host without
    // Unix sockets this module does not exist, and the import would be dead.
    use rux_server::config::config_path;

    use super::{DaemonConfig, DockerSandbox, SOCKET_MODE, init_logging, serve_connection};

    pub fn main() -> ExitCode {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("rux-playgroundd could not start a runtime: {error}");
                return ExitCode::FAILURE;
            }
        };

        match runtime.block_on(run()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                // Before logging is up this is the only channel there is.
                eprintln!("rux-playgroundd could not start: {error}");
                ExitCode::FAILURE
            }
        }
    }

    async fn run() -> Result<(), Box<dyn std::error::Error>> {
        // Configuration is read before logging is installed, so a bad file is
        // reported on stderr rather than swallowed by the JSON subscriber.
        let path = config_path(std::env::args())?;
        let config = DaemonConfig::load(&path)?;
        init_logging();

        let listener = bind(&config.socket_path).await?;
        let sandbox = Arc::new(DockerSandbox::new(config.sandbox.clone())?);
        let permits = Arc::new(Semaphore::new(config.max_concurrency));

        tracing::info!(
            max_concurrency = config.max_concurrency,
            "rux-playgroundd is accepting playground runs"
        );

        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;

        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, _address)) => {
                        tokio::spawn(serve_connection(
                            stream,
                            Arc::clone(&sandbox),
                            Arc::clone(&permits),
                            config.request_timeout,
                        ));
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not accept a playground connection");
                    }
                },
                _ = terminate.recv() => break,
                _ = interrupt.recv() => break,
            }
        }

        drain(
            listener,
            &permits,
            config.max_concurrency,
            &config.socket_path,
        )
        .await;

        Ok(())
    }

    /// Stops accepting, waits for in-flight runs, and removes the socket.
    ///
    /// The listener is taken by value: dropping it is what actually stops new
    /// connections being accepted, so a borrow here would silently do nothing.
    async fn drain(
        listener: UnixListener,
        permits: &Arc<Semaphore>,
        max_concurrency: usize,
        socket_path: &Path,
    ) {
        tracing::info!("rux-playgroundd is draining in-flight playground runs");
        drop(listener);

        // Holding every permit means no run is still in flight.
        let total = u32::try_from(max_concurrency).unwrap_or(u32::MAX);
        let _ = permits.acquire_many(total).await;

        if let Err(error) = fs::remove_file(socket_path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "could not remove the playground socket");
        }

        tracing::info!("rux-playgroundd has stopped");
    }

    /// Binds the socket, refusing to displace a daemon that is already running.
    async fn bind(path: &Path) -> Result<UnixListener, Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        if path.exists() {
            // A socket that still answers belongs to a live daemon; only a
            // socket nobody is listening on is safe to unlink.
            if UnixStream::connect(path).await.is_ok() {
                return Err(format!(
                    "another rux-playgroundd is already listening on {}",
                    path.display()
                )
                .into());
            }
            fs::remove_file(path)?;
        }

        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(SOCKET_MODE))?;

        Ok(listener)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use rux_sandbox::protocol::{FailureKind, Request, Response, read_frame, write_frame};
    use rux_sandbox::{
        BuildOutcome, SandboxError, SandboxLimits, SandboxMode, SandboxOutcome, SandboxProfile,
        SandboxRequest, SourceError,
    };
    use tokio::io::BufReader;
    use tokio::sync::{Semaphore, oneshot};

    use rux_server::config::ConfigFile;

    use super::{DaemonConfig, JobRunner, failure_for, parse_allowlist, serve_connection};

    /// A runner that answers without a container runtime.
    struct StubRunner {
        outcome: Box<dyn Fn() -> Result<SandboxOutcome, SandboxError> + Send + Sync>,
        started: AtomicUsize,
        gate: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
    }

    impl StubRunner {
        fn ok() -> Arc<Self> {
            Self::new(|| {
                Ok(SandboxOutcome {
                    build: BuildOutcome {
                        success: true,
                        ..BuildOutcome::default()
                    },
                    ..SandboxOutcome::default()
                })
            })
        }

        fn new(
            outcome: impl Fn() -> Result<SandboxOutcome, SandboxError> + Send + Sync + 'static,
        ) -> Arc<Self> {
            Arc::new(Self {
                outcome: Box::new(outcome),
                started: AtomicUsize::new(0),
                gate: std::sync::Mutex::new(None),
            })
        }
    }

    impl JobRunner for StubRunner {
        fn limits(&self) -> SandboxLimits {
            SandboxLimits::default()
        }

        async fn execute(&self, _request: &SandboxRequest) -> Result<SandboxOutcome, SandboxError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            let gate = self
                .gate
                .lock()
                .expect("gate should not be poisoned")
                .take();
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            (self.outcome)()
        }
    }

    fn run_request() -> Request {
        Request::Run {
            request: Box::new(SandboxRequest {
                mode: SandboxMode::Run,
                profile: SandboxProfile::Debug,
                source: "Fn Main() {}\n".to_owned(),
                stdin: String::new(),
            }),
        }
    }

    /// Drives one request through a served connection and returns the answer.
    async fn exchange<R: JobRunner>(
        runner: Arc<R>,
        permits: Arc<Semaphore>,
        request: &Request,
    ) -> Option<Response> {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let handler = tokio::spawn(serve_connection(
            server,
            runner,
            permits,
            Duration::from_secs(5),
        ));

        write_frame(&mut client, request)
            .await
            .expect("the request should write");

        let mut reader = BufReader::new(client);
        let response = read_frame(&mut reader).await.ok();
        handler.await.expect("the connection should be served");

        response
    }

    /// Configures the daemon from a TOML fragment.
    ///
    /// A fragment need only carry the broker's own sections: the registry's
    /// required values are checked when the API converts the same document,
    /// not when it deserializes.
    fn broker(fragment: &str) -> Result<DaemonConfig, String> {
        let file = toml_edit::de::from_str::<ConfigFile>(fragment)
            .map_err(|error| error.message().to_owned())?;
        DaemonConfig::from_file(&file)
    }

    fn allowlist(entries: &[&str]) -> Result<rux_sandbox::PackageAllowlist, String> {
        let owned = entries
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect::<Vec<_>>();
        parse_allowlist(&owned)
    }

    #[test]
    fn the_image_is_the_only_required_setting() {
        let config = broker("[playground.broker]\nimage = \"rux-playground:1\"\n")
            .expect("an image alone should configure the daemon");

        assert_eq!(config.sandbox.image, "rux-playground:1");
        assert_eq!(config.max_concurrency, 2);
        assert_eq!(config.sandbox.limits, SandboxLimits::default());
        assert_eq!(
            config.socket_path,
            std::path::PathBuf::from(super::DEFAULT_SOCKET)
        );
        assert!(broker("[playground.broker]\n").is_err());
        assert!(broker("[playground.broker]\nimage = \"  \"\n").is_err());
        // A host that never configured the broker must not start one.
        assert!(broker("").is_err());
    }

    #[test]
    fn tunable_limits_are_read_from_the_configuration_file() {
        let config = broker(
            "[playground.broker]\nimage = \"rux-playground:1\"\nmax_concurrency = 8\n\
             [playground.broker.limits]\ncompile_timeout_seconds = 20\nrun_timeout_seconds = 9\n",
        )
        .expect("bounded overrides should configure the daemon");

        assert_eq!(config.sandbox.limits.compile_timeout_seconds, 20);
        assert_eq!(config.sandbox.limits.run_timeout_seconds, 9);
        assert_eq!(config.max_concurrency, 8);
    }

    #[test]
    fn every_knob_is_bounded_rather_than_trusted() {
        for fragment in [
            "[playground.broker]\nmax_concurrency = 0\n",
            "[playground.broker]\nmax_concurrency = 17\n",
            "[playground.broker]\nrequest_timeout_seconds = 0\n",
            "[playground.broker.limits]\ncompile_timeout_seconds = 0\n",
            "[playground.broker.limits]\ncompile_timeout_seconds = 121\n",
            "[playground.broker.limits]\nrun_timeout_seconds = 600\n",
            "[playground.broker.limits]\nmemory_bytes = 1024\n",
            "[playground.broker.limits]\ncpu_millis = 0\n",
            // The format refuses a non-numeric value before any bound is
            // consulted, which the environment form could not do.
            "[playground.broker]\nmax_concurrency = \"many\"\n",
            // A fixed limit is not configurable, and naming one is a typo
            // rather than a request.
            "[playground.broker.limits]\npid_limit = 4\n",
            "[playground.broker.limits]\nmax_source_bytes = 65536\n",
            // A misspelled section must not be silently ignored.
            "[playground.brokerr]\nimage = \"rux-playground:1\"\n",
        ] {
            let document = format!("[playground.broker]\nimage = \"rux-playground:1\"\n{fragment}");
            assert!(
                broker(&document).is_err(),
                "expected this to be refused:\n{fragment}"
            );
        }
    }

    #[test]
    fn the_package_allowlist_is_parsed_as_root_and_namespace_pairs() {
        let parsed =
            allowlist(&["Std:Rux", " Json:Rux "]).expect("well-formed entries should parse");

        assert!(!parsed.is_empty());
        assert!(allowlist(&[]).expect("an empty list is allowed").is_empty());
        assert!(allowlist(&["Std"]).is_err());
        assert!(allowlist(&["Std:"]).is_err());
        assert!(allowlist(&["not a segment:Rux"]).is_err());
    }

    #[tokio::test]
    async fn a_limits_request_is_answered_without_taking_a_permit() {
        let permits = Arc::new(Semaphore::new(1));
        let response = exchange(StubRunner::ok(), Arc::clone(&permits), &Request::Limits)
            .await
            .expect("limits should be answered");

        assert!(matches!(
            response,
            Response::Limits { limits } if limits == SandboxLimits::default()
        ));
        assert_eq!(permits.available_permits(), 1);
    }

    #[tokio::test]
    async fn a_completed_run_is_returned_whole() {
        let response = exchange(
            StubRunner::ok(),
            Arc::new(Semaphore::new(1)),
            &run_request(),
        )
        .await
        .expect("a run should be answered");

        assert!(matches!(
            response,
            Response::Completed { outcome } if outcome.build.success
        ));
    }

    #[tokio::test]
    async fn saturation_is_refused_immediately_rather_than_queued() {
        let runner = StubRunner::ok();
        let permits = Arc::new(Semaphore::new(1));
        let held = Arc::clone(&permits)
            .try_acquire_owned()
            .expect("the only permit should be free");

        let response = tokio::time::timeout(
            Duration::from_millis(500),
            exchange(runner, Arc::clone(&permits), &run_request()),
        )
        .await
        .expect("saturation must answer without waiting for a permit")
        .expect("a response should be sent");

        assert!(matches!(
            response,
            Response::Failed {
                kind: FailureKind::Unavailable,
                ..
            }
        ));
        drop(held);
    }

    #[tokio::test]
    async fn a_permit_is_released_once_a_run_finishes() {
        let permits = Arc::new(Semaphore::new(1));
        let _ = exchange(StubRunner::ok(), Arc::clone(&permits), &run_request()).await;

        assert_eq!(permits.available_permits(), 1);
    }

    #[tokio::test]
    async fn a_malformed_frame_is_answered_and_the_connection_dropped() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let handler = tokio::spawn(serve_connection(
            server,
            StubRunner::ok(),
            Arc::new(Semaphore::new(1)),
            Duration::from_secs(5),
        ));

        tokio::io::AsyncWriteExt::write_all(&mut client, b"{\"operation\":\"nonsense\"}\n")
            .await
            .expect("the write should succeed");

        let mut reader = BufReader::new(client);
        let response: Response = read_frame(&mut reader)
            .await
            .expect("a malformed frame should still be answered");
        handler.await.expect("the connection should be served");

        assert!(matches!(
            response,
            Response::Failed {
                kind: FailureKind::InvalidRequest,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_peer_that_asks_nothing_is_not_answered() {
        let (client, server) = tokio::io::duplex(1024);
        drop(client);

        let handler = tokio::spawn(serve_connection(
            server,
            StubRunner::ok(),
            Arc::new(Semaphore::new(1)),
            Duration::from_secs(5),
        ));

        handler.await.expect("the connection should close cleanly");
    }

    #[tokio::test]
    async fn a_silent_peer_is_cut_off_by_the_read_deadline() {
        let (client, server) = tokio::io::duplex(1024);
        let handler = tokio::spawn(serve_connection(
            server,
            StubRunner::ok(),
            Arc::new(Semaphore::new(1)),
            Duration::from_millis(50),
        ));

        let mut reader = BufReader::new(client);
        let response: Response = read_frame(&mut reader)
            .await
            .expect("the deadline should produce an answer");
        handler.await.expect("the connection should be served");

        assert!(matches!(
            response,
            Response::Failed {
                kind: FailureKind::Timeout,
                ..
            }
        ));
    }

    #[test]
    fn sandbox_failures_map_onto_stable_wire_kinds() {
        let cases = [
            (
                SandboxError::InvalidSource(SourceError::Empty),
                FailureKind::InvalidRequest,
            ),
            (SandboxError::Timeout, FailureKind::Timeout),
            (SandboxError::Unavailable, FailureKind::Unavailable),
            (SandboxError::Internal, FailureKind::Internal),
        ];

        for (error, expected) in cases {
            let Response::Failed { kind, .. } = failure_for(&error) else {
                panic!("expected a failure response for {error:?}");
            };
            assert_eq!(kind, expected);
        }
    }

    #[test]
    fn a_rejected_submission_explains_itself_without_echoing_content() {
        let error = SandboxError::InvalidSource(SourceError::TooLarge {
            bytes: 99_999,
            maximum: 32_768,
        });

        let Response::Failed { detail, .. } = failure_for(&error) else {
            panic!("expected a failure response");
        };
        let detail = detail.expect("a validation failure should explain itself");

        assert!(detail.contains("99999"));
        assert!(detail.contains("32768"));
    }

    #[test]
    fn failures_that_could_name_a_host_detail_carry_none() {
        for error in [
            SandboxError::Unavailable,
            SandboxError::Timeout,
            SandboxError::Internal,
        ] {
            let Response::Failed { detail, .. } = failure_for(&error) else {
                panic!("expected a failure response");
            };
            assert!(detail.is_none(), "{error:?} must not carry a detail");
        }
    }
}
