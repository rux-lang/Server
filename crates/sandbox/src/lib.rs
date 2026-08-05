#![doc = "Playground sandbox contract: limits, framing, and container arguments."]
//!
//! This crate is the pure core of the playground. It decides what a run is
//! allowed to be, what the container is invoked with, and how the container's
//! output is read back — all without touching the filesystem, a process, or a
//! container runtime, so every rule here is testable on a machine with no
//! Docker installed.
//!
//! Two invariants shape the design:
//!
//! - **Nothing user-authored reaches `argv` or the environment.** Submitted
//!   source and standard input travel only as files in a read-only mount. The
//!   container's arguments are two enums ([`SandboxMode`], [`SandboxProfile`])
//!   and a server-generated [`Nonce`]. See [`docker_argv`].
//! - **A program cannot forge its own output framing.** Sections are introduced
//!   by a sentinel bearing a per-run nonce the program is never told. See
//!   [`parse_framed_output`].

mod docker;
mod execute;
mod framing;
mod job;
mod limits;
mod manifest;
mod request;
mod source;

pub use docker::{
    CONTAINER_NAME_PREFIX, DockerRun, JOB_MOUNT_PATH, WORK_MOUNT_PATH, container_name, docker_argv,
};
pub use execute::{
    DEFAULT_DOCKER_BINARY, DEFAULT_JOBS_ROOT, DockerSandbox, DockerSandboxConfig, SandboxError,
};
pub use framing::{
    FramedOutput, FramingError, RunStatus, SECTION_SENTINEL, Section, parse_framed_output, truncate,
};
pub use job::{IDENTIFIER_LENGTH, IdentifierError, JobId, Nonce};
pub use limits::{
    DEFAULT_COMPILE_TIMEOUT_SECONDS, DEFAULT_CPU_MILLIS, DEFAULT_MAX_OUTPUT_BYTES,
    DEFAULT_MAX_SOURCE_BYTES, DEFAULT_MAX_STDIN_BYTES, DEFAULT_MEMORY_BYTES, DEFAULT_PID_LIMIT,
    DEFAULT_RUN_TIMEOUT_SECONDS, DEFAULT_STARTUP_GRACE_SECONDS, DEFAULT_TMPFS_BYTES, LimitField,
    SandboxLimits, SandboxLimitsError,
};
pub use manifest::{
    PLAYGROUND_MANIFEST_VERSION, PLAYGROUND_MIN_RUX_VERSION, PLAYGROUND_PACKAGE_NAME,
    PLAYGROUND_PACKAGE_VERSION, PackageAllowlist, manifest_for,
};
pub use request::{
    BuildOutcome, ProgramOutcome, SandboxMode, SandboxOutcome, SandboxProfile, SandboxRequest,
};
pub use source::{SourceError, validate_source, validate_stdin};

/// Relative path of the generated manifest inside a job directory.
pub const JOB_MANIFEST_PATH: &str = "Rux.toml";
/// Relative path of the submitted source inside a job directory.
pub const JOB_SOURCE_PATH: &str = "Src/Main.rux";
/// Relative path of the submitted standard input inside a job directory.
pub const JOB_STDIN_PATH: &str = "stdin";
