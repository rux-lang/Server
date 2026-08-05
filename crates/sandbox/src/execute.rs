use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rand::RngCore;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;

use crate::docker::{DockerRun, container_name, docker_argv};
use crate::framing::{FramedOutput, FramingError, parse_framed_output, truncate};
use crate::job::{JobId, Nonce};
use crate::limits::{SandboxLimits, SandboxLimitsError};
use crate::manifest::{PackageAllowlist, manifest_for};
use crate::request::{BuildOutcome, ProgramOutcome, SandboxMode, SandboxOutcome, SandboxRequest};
use crate::source::{SourceError, validate_source, validate_stdin};
use crate::{JOB_MANIFEST_PATH, JOB_SOURCE_PATH, JOB_STDIN_PATH};

/// Default root the per-job directories are created under.
pub const DEFAULT_JOBS_ROOT: &str = "/var/lib/rux-playground/jobs";
/// Default container runtime binary.
pub const DEFAULT_DOCKER_BINARY: &str = "docker";

/// Bytes read from the child in one call.
const READ_CHUNK_BYTES: usize = 8 * 1024;
/// Cap on the container runtime's own standard error, which is never returned
/// to a caller and exists only to explain a failed run in a server log.
const RUNTIME_STDERR_CAP_BYTES: usize = 8 * 1024;
/// Grace given to a killed container to actually exit.
const REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// How the sandbox is wired to a host.
#[derive(Clone, Debug)]
pub struct DockerSandboxConfig {
    /// Directory the per-job directories are created under.
    pub jobs_root: PathBuf,
    /// Tag of the sandbox image.
    pub image: String,
    /// Container runtime binary, looked up on `PATH` when relative.
    pub docker_binary: PathBuf,
    /// Resource envelope applied to every run.
    pub limits: SandboxLimits,
    /// Packages a run may import.
    pub allowlist: PackageAllowlist,
    /// Deadline for [`DockerSandbox::probe`].
    pub probe_timeout: Duration,
}

impl Default for DockerSandboxConfig {
    fn default() -> Self {
        Self {
            jobs_root: PathBuf::from(DEFAULT_JOBS_ROOT),
            image: String::new(),
            docker_binary: PathBuf::from(DEFAULT_DOCKER_BINARY),
            limits: SandboxLimits::default(),
            allowlist: PackageAllowlist::default(),
            probe_timeout: Duration::from_secs(2),
        }
    }
}

/// Runs playground jobs as throwaway containers.
#[derive(Clone, Debug)]
pub struct DockerSandbox {
    config: DockerSandboxConfig,
}

impl DockerSandbox {
    /// Creates a sandbox after checking its limits.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidLimits`] when the configured limits
    /// cannot hold together.
    pub fn new(config: DockerSandboxConfig) -> Result<Self, SandboxError> {
        config
            .limits
            .validate()
            .map_err(SandboxError::InvalidLimits)?;

        Ok(Self { config })
    }

    /// Returns the limits every run is subject to.
    #[must_use]
    pub const fn limits(&self) -> &SandboxLimits {
        &self.config.limits
    }

    /// Compiles and optionally runs one submission.
    ///
    /// The submission is written into a fresh `0700` job directory, which is
    /// bind-mounted read-only into a container that has no network, no
    /// capabilities, and a read-only root filesystem. The directory is removed
    /// on every exit path, including a panic or a dropped future.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::InvalidSource`] or [`SandboxError::InvalidStdin`]
    /// for a submission that fails validation, [`SandboxError::Timeout`] when
    /// the container overran its outer deadline, and otherwise an error
    /// describing which stage failed without naming host paths or images.
    pub async fn execute(&self, request: &SandboxRequest) -> Result<SandboxOutcome, SandboxError> {
        let limits = &self.config.limits;
        validate_source(&request.source, limits).map_err(SandboxError::InvalidSource)?;
        validate_stdin(&request.stdin, limits).map_err(SandboxError::InvalidStdin)?;

        let job_id = JobId::new(random_hex()).map_err(|_| SandboxError::Internal)?;
        let nonce = Nonce::new(random_hex()).map_err(|_| SandboxError::Internal)?;

        let directory = JobDirectory::create(
            &self.config.jobs_root,
            &job_id,
            &manifest_for(&request.source, &self.config.allowlist),
            &request.source,
            &request.stdin,
        )
        .await
        .map_err(SandboxError::JobDirectory)?;

        let job_path =
            directory
                .path()
                .to_str()
                .ok_or(SandboxError::JobDirectory(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "job directory path is not valid UTF-8",
                )))?;

        let argv = docker_argv(
            &DockerRun {
                job_id: &job_id,
                nonce: &nonce,
                job_directory: job_path,
                image: &self.config.image,
                user: directory.owner().as_deref(),
                mode: request.mode,
                profile: request.profile,
            },
            limits,
        );

        let run = self.run_container(&argv, &job_id).await?;
        let framed = parse_framed_output(&run.stdout, &nonce).map_err(SandboxError::Framing)?;

        Ok(assemble(&framed, request, limits, run.overflowed))
    }

    /// Reports the container runtime's server version.
    ///
    /// This is the availability signal the daemon publishes as a metric; it is
    /// deliberately not part of the API's readiness aggregate.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Runtime`] when the runtime cannot be started and
    /// [`SandboxError::Unavailable`] when it answers with a failure or not at all.
    pub async fn probe(&self) -> Result<String, SandboxError> {
        let mut command = self.command();
        command.args(["version", "--format", "{{.Server.Version}}"]);

        let output = timeout(self.config.probe_timeout, command.output())
            .await
            .map_err(|_| SandboxError::Unavailable)?
            .map_err(SandboxError::Runtime)?;

        if !output.status.success() {
            return Err(SandboxError::Unavailable);
        }

        let (version, _) = truncate(&output.stdout, 128);
        let version = version.trim().to_owned();
        if version.is_empty() {
            return Err(SandboxError::Unavailable);
        }

        Ok(version)
    }

    /// Spawns the container and collects its output within the outer deadline.
    async fn run_container(
        &self,
        argv: &[String],
        job_id: &JobId,
    ) -> Result<RunOutput, SandboxError> {
        let mut command = self.command();
        // `argv[0]` is the logical program name; the binary to spawn is
        // configured separately so an operator can pin an absolute path.
        command.args(&argv[1..]);
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);

        let mut child = command.spawn().map_err(SandboxError::Runtime)?;
        let stdout = child.stdout.take().ok_or(SandboxError::Internal)?;
        let stderr = child.stderr.take().ok_or(SandboxError::Internal)?;

        let deadline = Duration::from_secs(self.config.limits.total_deadline_seconds().into());
        let capacity = stdout_capacity(&self.config.limits);
        let collected = timeout(deadline, async {
            tokio::try_join!(
                read_bounded(stdout, capacity),
                read_bounded(stderr, RUNTIME_STDERR_CAP_BYTES),
                child.wait(),
            )
        })
        .await;

        match collected {
            Ok(Ok((stdout, _runtime_stderr, status))) => {
                if status.success() || !stdout.bytes.is_empty() {
                    // A non-zero exit with framed output is an ordinary failed
                    // build; the framing decides, not the runtime's exit code.
                    Ok(RunOutput {
                        stdout: stdout.bytes,
                        overflowed: stdout.overflowed,
                    })
                } else {
                    Err(SandboxError::Unavailable)
                }
            }
            Ok(Err(error)) => Err(SandboxError::Runtime(error)),
            Err(_elapsed) => {
                // `kill_on_drop` reaches the `docker` client, not the container
                // it asked the daemon to start, so the container needs an
                // explicit kill of its own.
                self.reap(job_id).await;
                let _ = timeout(REAP_TIMEOUT, child.wait()).await;
                Err(SandboxError::Timeout)
            }
        }
    }

    /// Kills a container that overran the outer deadline.
    async fn reap(&self, job_id: &JobId) {
        let mut command = self.command();
        command.args(["kill", "--signal=KILL", &container_name(job_id)]);
        command.stdin(Stdio::null());
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
        command.kill_on_drop(true);

        if let Ok(mut child) = command.spawn() {
            let _ = timeout(REAP_TIMEOUT, child.wait()).await;
        }
    }

    fn command(&self) -> Command {
        Command::new(&self.config.docker_binary)
    }
}

/// What one container run produced.
struct RunOutput {
    stdout: Vec<u8>,
    overflowed: bool,
}

/// Bytes read from one stream, and whether more were discarded.
struct BoundedOutput {
    bytes: Vec<u8>,
    overflowed: bool,
}

/// Total bytes retained from a run's standard output.
///
/// A run emits up to four capped content sections plus a small status trailer,
/// so the ceiling has to clear four times the per-stream cap; a tighter bound
/// would drop the trailing `status` section of a legitimately chatty run and
/// turn it into a framing error.
fn stdout_capacity(limits: &SandboxLimits) -> usize {
    limits
        .max_output_bytes
        .saturating_mul(4)
        .saturating_add(4 * 1024)
}

/// Reads a stream to its end, retaining at most `cap` bytes.
///
/// Reading continues past the cap so the child never blocks on a full pipe, but
/// nothing beyond the cap is kept. A program that prints without end is bounded
/// by the run's deadline rather than by the server's memory.
async fn read_bounded<R>(mut reader: R, cap: usize) -> io::Result<BoundedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    let mut overflowed = false;

    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }

        let remaining = cap.saturating_sub(bytes.len());
        if remaining == 0 {
            overflowed = true;
        } else if read > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            overflowed = true;
        } else {
            bytes.extend_from_slice(&chunk[..read]);
        }
    }

    Ok(BoundedOutput { bytes, overflowed })
}

/// Turns framed container output into the outcome returned to a caller.
fn assemble(
    framed: &FramedOutput<'_>,
    request: &SandboxRequest,
    limits: &SandboxLimits,
    overflowed: bool,
) -> SandboxOutcome {
    let cap = limits.max_output_bytes;
    let status = framed.status;

    let (diagnostics, diagnostics_truncated) = truncate(framed.build, cap);
    let build = BuildOutcome {
        success: status.build_exit_code == 0,
        diagnostics,
        diagnostics_truncated: diagnostics_truncated || overflowed,
        duration_ms: status.build_duration_ms,
    };

    let program = if request.mode.executes_program() && build.success {
        let (stdout, stdout_truncated) = truncate(framed.stdout, cap);
        let (stderr, stderr_truncated) = truncate(framed.stderr, cap);
        Some(ProgramOutcome {
            stdout,
            stdout_truncated: stdout_truncated || overflowed,
            stderr,
            stderr_truncated: stderr_truncated || overflowed,
            exit_code: status.run_exit_code,
            signal: status.run_signal,
            timed_out: status.timed_out,
            duration_ms: status.run_duration_ms.unwrap_or_default(),
        })
    } else {
        None
    };

    let formatted = if request.mode == SandboxMode::Fmt && build.success {
        Some(truncate(framed.formatted, limits.max_source_bytes).0)
    } else {
        None
    };

    SandboxOutcome {
        build,
        program,
        formatted,
    }
}

/// Returns 32 lowercase hexadecimal characters from the thread's CSPRNG.
fn random_hex() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);

    bytes
        .iter()
        .fold(String::with_capacity(32), |mut text, byte| {
            use std::fmt::Write as _;
            let _ = write!(text, "{byte:02x}");
            text
        })
}

/// A job directory that removes itself when dropped.
struct JobDirectory {
    path: PathBuf,
    owner: Option<String>,
}

impl JobDirectory {
    /// Creates the directory and writes the three files a run consumes.
    async fn create(
        root: &Path,
        job_id: &JobId,
        manifest: &str,
        source: &str,
        stdin: &str,
    ) -> io::Result<Self> {
        tokio::fs::create_dir_all(root).await?;

        let path = root.join(job_id.as_str());
        private_dir_builder().create(&path).await?;

        // The guard owns the directory from here on, so every later failure
        // still removes what was created.
        let directory = Self {
            owner: owner_of(&path),
            path,
        };

        let source_path = directory.path.join(JOB_SOURCE_PATH);
        if let Some(parent) = source_path.parent() {
            private_dir_builder().create(parent).await?;
        }

        tokio::fs::write(directory.path.join(JOB_MANIFEST_PATH), manifest).await?;
        tokio::fs::write(source_path, source).await?;
        tokio::fs::write(directory.path.join(JOB_STDIN_PATH), stdin).await?;

        Ok(directory)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the `uid:gid` the container must run as, on hosts that have one.
    fn owner(&self) -> Option<String> {
        self.owner.clone()
    }
}

impl Drop for JobDirectory {
    fn drop(&mut self) {
        // Deliberately the blocking API: `Drop` also runs when a future is
        // cancelled or a task panics, where spawning async work is not an
        // option. The tree is three small files.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
fn private_dir_builder() -> tokio::fs::DirBuilder {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut builder = tokio::fs::DirBuilder::new();
    builder.mode(0o700);
    builder
}

#[cfg(not(unix))]
fn private_dir_builder() -> tokio::fs::DirBuilder {
    tokio::fs::DirBuilder::new()
}

/// Returns the `uid:gid` owning `path`, on hosts that have such a notion.
///
/// A single `stat` on a directory just created locally, so it stays blocking
/// rather than paying for a thread hop.
#[cfg(unix)]
fn owner_of(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;

    // Reading back what we just created avoids a `libc` dependency and the
    // `unsafe` block a direct `getuid` call would need.
    let metadata = std::fs::metadata(path).ok()?;
    Some(format!("{}:{}", metadata.uid(), metadata.gid()))
}

#[cfg(not(unix))]
fn owner_of(_path: &Path) -> Option<String> {
    None
}

/// A run that could not be carried out, or whose result could not be read.
///
/// `Debug` is written by hand so an image tag, a job path, or a runtime error
/// cannot reach a log line through the usual `{:?}` on an error.
#[derive(Error)]
pub enum SandboxError {
    /// The submitted source failed validation.
    #[error("submitted source was rejected")]
    InvalidSource(#[source] SourceError),
    /// The submitted standard input failed validation.
    #[error("submitted standard input was rejected")]
    InvalidStdin(#[source] SourceError),
    /// The configured limits cannot hold together.
    #[error("sandbox limits are inconsistent")]
    InvalidLimits(#[source] SandboxLimitsError),
    /// The job directory could not be prepared.
    #[error("the job directory could not be prepared")]
    JobDirectory(#[source] io::Error),
    /// The container runtime could not be started.
    #[error("the container runtime could not be started")]
    Runtime(#[source] io::Error),
    /// The container runtime is not answering.
    #[error("the container runtime is unavailable")]
    Unavailable,
    /// The run overran its outer deadline and was killed.
    #[error("the run exceeded its deadline")]
    Timeout,
    /// The container's output could not be interpreted.
    #[error("the container produced unusable output")]
    Framing(#[source] FramingError),
    /// An invariant of the sandbox itself did not hold.
    #[error("the sandbox failed internally")]
    Internal,
}

impl fmt::Debug for SandboxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let variant = match self {
            Self::InvalidSource(_) => "InvalidSource",
            Self::InvalidStdin(_) => "InvalidStdin",
            Self::InvalidLimits(_) => "InvalidLimits",
            Self::JobDirectory(_) => "JobDirectory",
            Self::Runtime(_) => "Runtime",
            Self::Unavailable => "Unavailable",
            Self::Timeout => "Timeout",
            Self::Framing(_) => "Framing",
            Self::Internal => "Internal",
        };

        write!(formatter, "SandboxError::{variant}")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        BoundedOutput, DockerSandbox, DockerSandboxConfig, JobDirectory, SandboxError, assemble,
        random_hex, read_bounded, stdout_capacity,
    };
    use crate::framing::{FramedOutput, RunStatus};
    use crate::job::{JobId, Nonce};
    use crate::limits::SandboxLimits;
    use crate::request::{SandboxMode, SandboxProfile, SandboxRequest};
    use crate::{JOB_MANIFEST_PATH, JOB_SOURCE_PATH, JOB_STDIN_PATH};

    fn request(mode: SandboxMode, source: &str) -> SandboxRequest {
        SandboxRequest {
            mode,
            profile: SandboxProfile::Debug,
            source: source.to_owned(),
            stdin: String::new(),
        }
    }

    fn sandbox(root: &std::path::Path) -> DockerSandbox {
        DockerSandbox::new(DockerSandboxConfig {
            jobs_root: root.to_path_buf(),
            image: "rux-playground:test".to_owned(),
            // A binary that cannot exist, so nothing in these tests can reach a
            // real container runtime.
            docker_binary: root.join("no-such-runtime"),
            probe_timeout: Duration::from_millis(200),
            ..DockerSandboxConfig::default()
        })
        .expect("default limits should validate")
    }

    #[test]
    fn a_sandbox_refuses_to_start_with_inconsistent_limits() {
        let config = DockerSandboxConfig {
            limits: SandboxLimits {
                memory_bytes: 0,
                ..SandboxLimits::default()
            },
            ..DockerSandboxConfig::default()
        };

        assert!(matches!(
            DockerSandbox::new(config),
            Err(SandboxError::InvalidLimits(_))
        ));
    }

    #[test]
    fn random_identifiers_are_well_formed_and_do_not_repeat() {
        let first = random_hex();
        let second = random_hex();

        assert!(JobId::new(first.clone()).is_ok());
        assert!(Nonce::new(second.clone()).is_ok());
        assert_ne!(first, second);
    }

    #[test]
    fn the_stdout_ceiling_clears_every_section_a_legitimate_run_may_emit() {
        let limits = SandboxLimits::default();

        // Four capped sections plus the status trailer must all fit, or a
        // chatty-but-legal run would lose its status section.
        assert!(stdout_capacity(&limits) > limits.max_output_bytes * 4);
    }

    #[tokio::test]
    async fn a_job_directory_holds_the_three_files_a_run_consumes() {
        let root = tempfile::tempdir().expect("temporary root should be created");
        let job_id = JobId::new(random_hex()).unwrap();

        let directory = JobDirectory::create(
            root.path(),
            &job_id,
            "[Manifest]\n",
            "Fn Main() {}\n",
            "input\n",
        )
        .await
        .expect("job directory should be created");

        let path = directory.path().to_path_buf();
        assert_eq!(path.file_name().unwrap(), job_id.as_str());
        assert_eq!(
            std::fs::read_to_string(path.join(JOB_MANIFEST_PATH)).unwrap(),
            "[Manifest]\n"
        );
        assert_eq!(
            std::fs::read_to_string(path.join(JOB_SOURCE_PATH)).unwrap(),
            "Fn Main() {}\n"
        );
        assert_eq!(
            std::fs::read_to_string(path.join(JOB_STDIN_PATH)).unwrap(),
            "input\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_job_directory_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = tempfile::tempdir().expect("temporary root should be created");
        let job_id = JobId::new(random_hex()).unwrap();
        let directory = JobDirectory::create(root.path(), &job_id, "m", "s", "i")
            .await
            .expect("job directory should be created");

        let mode = std::fs::metadata(directory.path())
            .unwrap()
            .permissions()
            .mode();

        assert_eq!(mode & 0o777, 0o700);
        assert!(
            directory.owner().is_some(),
            "the container must be told which user to run as"
        );
    }

    #[tokio::test]
    async fn dropping_the_guard_removes_the_job_directory() {
        let root = tempfile::tempdir().expect("temporary root should be created");
        let job_id = JobId::new(random_hex()).unwrap();
        let directory = JobDirectory::create(root.path(), &job_id, "m", "s", "i")
            .await
            .expect("job directory should be created");
        let path = directory.path().to_path_buf();
        assert!(path.exists());

        drop(directory);

        assert!(!path.exists(), "the job directory must not outlive its run");
    }

    #[tokio::test]
    async fn a_panicking_run_still_removes_its_job_directory() {
        let root = tempfile::tempdir().expect("temporary root should be created");
        let job_id = JobId::new(random_hex()).unwrap();
        let path = root.path().join(job_id.as_str());

        let outcome = tokio::spawn({
            let root = root.path().to_path_buf();
            async move {
                let _directory = JobDirectory::create(&root, &job_id, "m", "s", "i")
                    .await
                    .expect("job directory should be created");
                panic!("a run panicked mid-flight");
            }
        })
        .await;

        assert!(outcome.is_err(), "the task should have panicked");
        assert!(!path.exists(), "unwinding must still remove the directory");
    }

    #[tokio::test]
    async fn a_cancelled_run_still_removes_its_job_directory() {
        let root = tempfile::tempdir().expect("temporary root should be created");
        let job_id = JobId::new(random_hex()).unwrap();
        let path = root.path().join(job_id.as_str());

        let never = async {
            let _directory = JobDirectory::create(root.path(), &job_id, "m", "s", "i")
                .await
                .expect("job directory should be created");
            std::future::pending::<()>().await;
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(50), never)
                .await
                .is_err()
        );

        assert!(
            !path.exists(),
            "a dropped future must still remove the directory"
        );
    }

    #[tokio::test]
    async fn a_bounded_reader_keeps_the_cap_and_reports_the_overflow() {
        let BoundedOutput { bytes, overflowed } = read_bounded(&b"abcdefghij"[..], 4)
            .await
            .expect("reading a slice should succeed");

        assert_eq!(bytes, b"abcd");
        assert!(overflowed);
    }

    #[tokio::test]
    async fn a_bounded_reader_drains_the_whole_stream_so_the_child_never_blocks() {
        // Far more than the cap, delivered in many chunks.
        let source = vec![b'x'; 1024 * 1024];
        let BoundedOutput { bytes, overflowed } = read_bounded(&source[..], 16)
            .await
            .expect("reading a slice should succeed");

        assert_eq!(bytes.len(), 16);
        assert!(overflowed);
    }

    #[tokio::test]
    async fn a_stream_within_the_cap_is_returned_whole() {
        let BoundedOutput { bytes, overflowed } = read_bounded(&b"short"[..], 64)
            .await
            .expect("reading a slice should succeed");

        assert_eq!(bytes, b"short");
        assert!(!overflowed);
    }

    #[test]
    fn a_failed_build_reports_diagnostics_and_no_program() {
        let framed = FramedOutput {
            build: b"error: expected `)`",
            status: RunStatus {
                build_exit_code: 1,
                build_duration_ms: 12,
                ..RunStatus::default()
            },
            ..FramedOutput::default()
        };

        let outcome = assemble(
            &framed,
            &request(SandboxMode::Run, "x"),
            &SandboxLimits::default(),
            false,
        );

        assert!(!outcome.build.success);
        assert_eq!(outcome.build.diagnostics, "error: expected `)`");
        assert_eq!(outcome.build.duration_ms, 12);
        assert!(outcome.program.is_none());
        assert!(outcome.formatted.is_none());
    }

    #[test]
    fn a_successful_run_reports_both_streams_and_the_exit_code() {
        let framed = FramedOutput {
            stdout: b"hello\n",
            stderr: b"warn\n",
            status: RunStatus {
                run_exit_code: Some(0),
                run_duration_ms: Some(7),
                ..RunStatus::default()
            },
            ..FramedOutput::default()
        };

        let program = assemble(
            &framed,
            &request(SandboxMode::Run, "x"),
            &SandboxLimits::default(),
            false,
        )
        .program
        .expect("a successful run should report its program");

        assert_eq!(program.stdout, "hello\n");
        assert_eq!(program.stderr, "warn\n");
        assert_eq!(program.exit_code, Some(0));
        assert_eq!(program.duration_ms, 7);
        assert!(!program.timed_out);
    }

    #[test]
    fn a_discarded_overflow_marks_every_stream_truncated() {
        let framed = FramedOutput {
            stdout: b"kept",
            status: RunStatus {
                run_exit_code: Some(0),
                run_duration_ms: Some(1),
                ..RunStatus::default()
            },
            ..FramedOutput::default()
        };

        let outcome = assemble(
            &framed,
            &request(SandboxMode::Run, "x"),
            &SandboxLimits::default(),
            true,
        );

        assert!(outcome.build.diagnostics_truncated);
        let program = outcome.program.expect("program should be reported");
        assert!(program.stdout_truncated);
        assert!(program.stderr_truncated);
    }

    #[test]
    fn build_mode_never_reports_a_program_even_when_it_succeeds() {
        let framed = FramedOutput {
            stdout: b"should be ignored",
            status: RunStatus::default(),
            ..FramedOutput::default()
        };

        let outcome = assemble(
            &framed,
            &request(SandboxMode::Build, "x"),
            &SandboxLimits::default(),
            false,
        );

        assert!(outcome.build.success);
        assert!(outcome.program.is_none());
    }

    #[test]
    fn fmt_mode_returns_the_formatted_source_and_no_program() {
        let framed = FramedOutput {
            formatted: b"Fn Main() {}\n",
            status: RunStatus::default(),
            ..FramedOutput::default()
        };

        let outcome = assemble(
            &framed,
            &request(SandboxMode::Fmt, "x"),
            &SandboxLimits::default(),
            false,
        );

        assert_eq!(outcome.formatted.as_deref(), Some("Fn Main() {}\n"));
        assert!(outcome.program.is_none());
    }

    #[tokio::test]
    async fn an_invalid_submission_is_rejected_before_any_container_is_started() {
        let root = tempfile::tempdir().expect("temporary root should be created");
        let sandbox = sandbox(root.path());

        assert!(matches!(
            sandbox.execute(&request(SandboxMode::Run, "")).await,
            Err(SandboxError::InvalidSource(_))
        ));
        assert!(matches!(
            sandbox
                .execute(&request(SandboxMode::Run, "ok\u{1e}bad"))
                .await,
            Err(SandboxError::InvalidSource(_))
        ));

        let mut oversized = request(SandboxMode::Run, "x");
        oversized.stdin = "s".repeat(SandboxLimits::default().max_stdin_bytes + 1);
        assert!(matches!(
            sandbox.execute(&oversized).await,
            Err(SandboxError::InvalidStdin(_))
        ));

        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            0,
            "a rejected submission must not create a job directory"
        );
    }

    #[tokio::test]
    async fn a_missing_container_runtime_is_reported_without_naming_it() {
        let root = tempfile::tempdir().expect("temporary root should be created");
        let sandbox = sandbox(root.path());

        let error = sandbox
            .execute(&request(SandboxMode::Run, "Fn Main() {}\n"))
            .await
            .expect_err("a missing runtime should fail the run");

        assert!(matches!(error, SandboxError::Runtime(_)));
        assert_eq!(format!("{error:?}"), "SandboxError::Runtime");
        assert_eq!(
            error.to_string(),
            "the container runtime could not be started"
        );
    }

    #[tokio::test]
    async fn a_failed_run_leaves_no_job_directory_behind() {
        let root = tempfile::tempdir().expect("temporary root should be created");
        let sandbox = sandbox(root.path());

        let _ = sandbox
            .execute(&request(SandboxMode::Run, "Fn Main() {}\n"))
            .await;

        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            0,
            "the job directory must be removed even when the run fails"
        );
    }

    #[tokio::test]
    async fn a_missing_container_runtime_makes_the_probe_report_unavailable() {
        let root = tempfile::tempdir().expect("temporary root should be created");

        assert!(matches!(
            sandbox(root.path()).probe().await,
            Err(SandboxError::Runtime(_))
        ));
    }

    #[test]
    fn error_debug_output_names_only_the_variant() {
        let errors = [
            SandboxError::Unavailable,
            SandboxError::Timeout,
            SandboxError::Internal,
        ];

        for error in errors {
            let rendered = format!("{error:?}");
            assert!(rendered.starts_with("SandboxError::"));
            assert!(!rendered.contains('{'), "no field may be rendered");
        }
    }
}
