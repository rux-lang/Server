use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Memory ceiling for one run, in bytes.
pub const DEFAULT_MEMORY_BYTES: u64 = 128 * 1024 * 1024;
/// CPU allowance for one run, in thousandths of a core.
pub const DEFAULT_CPU_MILLIS: u32 = 500;
/// Wall-clock ceiling for the in-container compile step.
pub const DEFAULT_COMPILE_TIMEOUT_SECONDS: u32 = 5;
/// Wall-clock ceiling for the in-container program step.
pub const DEFAULT_RUN_TIMEOUT_SECONDS: u32 = 3;
/// Slack added to the outer deadline to cover container startup and teardown.
pub const DEFAULT_STARTUP_GRACE_SECONDS: u32 = 5;
/// Process ceiling for one run.
pub const DEFAULT_PID_LIMIT: u32 = 32;
/// Size of the writable `/work` tmpfs, in bytes.
pub const DEFAULT_TMPFS_BYTES: u64 = 32 * 1024 * 1024;
/// Largest accepted program source, in bytes.
pub const DEFAULT_MAX_SOURCE_BYTES: usize = 32 * 1024;
/// Largest accepted standard input, in bytes.
pub const DEFAULT_MAX_STDIN_BYTES: usize = 16 * 1024;
/// Largest returned output per stream, in bytes.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 16 * 1024;

/// The resource envelope applied to a single playground run.
///
/// Every field is a hard bound handed to the container runtime or enforced
/// before the runtime is reached. [`SandboxLimits::validate`] rejects
/// combinations that could not be satisfied at run time.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct SandboxLimits {
    /// Container memory ceiling in bytes; swap is pinned to the same value.
    pub memory_bytes: u64,
    /// CPU quota in thousandths of a core, so `500` is half a core.
    pub cpu_millis: u32,
    /// Seconds allowed for the compile step inside the container.
    pub compile_timeout_seconds: u32,
    /// Seconds allowed for the program step inside the container.
    pub run_timeout_seconds: u32,
    /// Extra seconds granted to the outer deadline for container startup.
    pub startup_grace_seconds: u32,
    /// Maximum number of processes in the container.
    pub pid_limit: u32,
    /// Size of the writable working tmpfs in bytes.
    pub tmpfs_bytes: u64,
    /// Maximum accepted source length in bytes.
    pub max_source_bytes: usize,
    /// Maximum accepted standard input length in bytes.
    pub max_stdin_bytes: usize,
    /// Maximum returned length per output stream in bytes.
    pub max_output_bytes: usize,
}

impl Default for SandboxLimits {
    fn default() -> Self {
        Self {
            memory_bytes: DEFAULT_MEMORY_BYTES,
            cpu_millis: DEFAULT_CPU_MILLIS,
            compile_timeout_seconds: DEFAULT_COMPILE_TIMEOUT_SECONDS,
            run_timeout_seconds: DEFAULT_RUN_TIMEOUT_SECONDS,
            startup_grace_seconds: DEFAULT_STARTUP_GRACE_SECONDS,
            pid_limit: DEFAULT_PID_LIMIT,
            tmpfs_bytes: DEFAULT_TMPFS_BYTES,
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            max_stdin_bytes: DEFAULT_MAX_STDIN_BYTES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

impl SandboxLimits {
    /// Total seconds the caller should wait for the container as a whole.
    ///
    /// The in-container `timeout(1)` is the primary limit; this is the outer
    /// backstop for a container that never reaches its entry point.
    #[must_use]
    pub const fn total_deadline_seconds(&self) -> u32 {
        self.compile_timeout_seconds
            .saturating_add(self.run_timeout_seconds)
            .saturating_add(self.startup_grace_seconds)
    }

    /// Checks that every limit is inside its accepted range and mutually consistent.
    ///
    /// # Errors
    ///
    /// Returns a [`SandboxLimitsError`] naming the first field that is out of
    /// range, or the first pair of fields whose ordering cannot be satisfied.
    pub fn validate(&self) -> Result<(), SandboxLimitsError> {
        check(LimitField::MemoryBytes, self.memory_bytes, MEMORY_RANGE)?;
        check(LimitField::CpuMillis, self.cpu_millis.into(), CPU_RANGE)?;
        check(
            LimitField::CompileTimeoutSeconds,
            self.compile_timeout_seconds.into(),
            TIMEOUT_RANGE,
        )?;
        check(
            LimitField::RunTimeoutSeconds,
            self.run_timeout_seconds.into(),
            TIMEOUT_RANGE,
        )?;
        check(
            LimitField::StartupGraceSeconds,
            self.startup_grace_seconds.into(),
            GRACE_RANGE,
        )?;
        check(LimitField::PidLimit, self.pid_limit.into(), PID_RANGE)?;
        check(LimitField::TmpfsBytes, self.tmpfs_bytes, TMPFS_RANGE)?;
        check(
            LimitField::MaxSourceBytes,
            widen(self.max_source_bytes),
            PAYLOAD_RANGE,
        )?;
        check(
            LimitField::MaxStdinBytes,
            widen(self.max_stdin_bytes),
            PAYLOAD_RANGE,
        )?;
        check(
            LimitField::MaxOutputBytes,
            widen(self.max_output_bytes),
            PAYLOAD_RANGE,
        )?;

        // A tmpfs is charged against the container's memory cgroup, so a
        // filesystem larger than the memory ceiling can never be filled.
        order(
            LimitField::TmpfsBytes,
            self.tmpfs_bytes,
            LimitField::MemoryBytes,
            self.memory_bytes,
        )?;
        // The source is copied into the tmpfs before the compiler ever sees it.
        order(
            LimitField::MaxSourceBytes,
            widen(self.max_source_bytes),
            LimitField::TmpfsBytes,
            self.tmpfs_bytes,
        )?;
        order(
            LimitField::MaxStdinBytes,
            widen(self.max_stdin_bytes),
            LimitField::TmpfsBytes,
            self.tmpfs_bytes,
        )?;

        Ok(())
    }
}

const MEMORY_RANGE: (u64, u64) = (16 * 1024 * 1024, 4 * 1024 * 1024 * 1024);
const CPU_RANGE: (u64, u64) = (100, 16_000);
const TIMEOUT_RANGE: (u64, u64) = (1, 120);
const GRACE_RANGE: (u64, u64) = (1, 60);
const PID_RANGE: (u64, u64) = (8, 1_024);
const TMPFS_RANGE: (u64, u64) = (1024 * 1024, 1024 * 1024 * 1024);
const PAYLOAD_RANGE: (u64, u64) = (1, 4 * 1024 * 1024);

fn check(
    field: LimitField,
    value: u64,
    (minimum, maximum): (u64, u64),
) -> Result<(), SandboxLimitsError> {
    if value < minimum || value > maximum {
        return Err(SandboxLimitsError::OutOfRange {
            field,
            value,
            minimum,
            maximum,
        });
    }
    Ok(())
}

fn order(
    smaller: LimitField,
    smaller_value: u64,
    larger: LimitField,
    larger_value: u64,
) -> Result<(), SandboxLimitsError> {
    if smaller_value > larger_value {
        return Err(SandboxLimitsError::Ordering { smaller, larger });
    }
    Ok(())
}

/// Widens a host-sized byte count without risking a lossy cast.
fn widen(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Identifies the limit a [`SandboxLimitsError`] refers to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LimitField {
    MemoryBytes,
    CpuMillis,
    CompileTimeoutSeconds,
    RunTimeoutSeconds,
    StartupGraceSeconds,
    PidLimit,
    TmpfsBytes,
    MaxSourceBytes,
    MaxStdinBytes,
    MaxOutputBytes,
}

impl LimitField {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MemoryBytes => "memory_bytes",
            Self::CpuMillis => "cpu_millis",
            Self::CompileTimeoutSeconds => "compile_timeout_seconds",
            Self::RunTimeoutSeconds => "run_timeout_seconds",
            Self::StartupGraceSeconds => "startup_grace_seconds",
            Self::PidLimit => "pid_limit",
            Self::TmpfsBytes => "tmpfs_bytes",
            Self::MaxSourceBytes => "max_source_bytes",
            Self::MaxStdinBytes => "max_stdin_bytes",
            Self::MaxOutputBytes => "max_output_bytes",
        }
    }
}

impl fmt::Display for LimitField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A rejected [`SandboxLimits`] combination.
///
/// These describe operator configuration, never submitted content, so they are
/// safe to log in full.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SandboxLimitsError {
    /// A limit fell outside its accepted range.
    #[error("{field} is {value}; accepted range is {minimum}..={maximum}")]
    OutOfRange {
        field: LimitField,
        value: u64,
        minimum: u64,
        maximum: u64,
    },
    /// Two limits were individually valid but cannot hold together.
    #[error("{smaller} must not exceed {larger}")]
    Ordering {
        smaller: LimitField,
        larger: LimitField,
    },
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_MEMORY_BYTES, LimitField, SandboxLimits, SandboxLimitsError};

    /// Applies one out-of-range value to an otherwise-valid set of limits.
    type Mutate = fn(&mut SandboxLimits);

    #[test]
    fn default_limits_match_the_documented_envelope_and_validate() {
        let limits = SandboxLimits::default();

        assert_eq!(limits.memory_bytes, 134_217_728);
        assert_eq!(limits.cpu_millis, 500);
        assert_eq!(limits.compile_timeout_seconds, 5);
        assert_eq!(limits.run_timeout_seconds, 3);
        assert_eq!(limits.pid_limit, 32);
        assert_eq!(limits.tmpfs_bytes, 33_554_432);
        assert_eq!(limits.max_source_bytes, 32_768);
        assert_eq!(limits.max_output_bytes, 16_384);
        assert!(limits.validate().is_ok());
    }

    #[test]
    fn total_deadline_covers_both_steps_and_the_startup_grace() {
        let limits = SandboxLimits::default();

        assert_eq!(limits.total_deadline_seconds(), 13);
    }

    #[test]
    fn zero_valued_limits_are_rejected_field_by_field() {
        let cases: [(Mutate, LimitField); 10] = [
            (|l| l.memory_bytes = 0, LimitField::MemoryBytes),
            (|l| l.cpu_millis = 0, LimitField::CpuMillis),
            (
                |l| l.compile_timeout_seconds = 0,
                LimitField::CompileTimeoutSeconds,
            ),
            (|l| l.run_timeout_seconds = 0, LimitField::RunTimeoutSeconds),
            (
                |l| l.startup_grace_seconds = 0,
                LimitField::StartupGraceSeconds,
            ),
            (|l| l.pid_limit = 0, LimitField::PidLimit),
            (|l| l.tmpfs_bytes = 0, LimitField::TmpfsBytes),
            (|l| l.max_source_bytes = 0, LimitField::MaxSourceBytes),
            (|l| l.max_stdin_bytes = 0, LimitField::MaxStdinBytes),
            (|l| l.max_output_bytes = 0, LimitField::MaxOutputBytes),
        ];

        for (mutate, expected) in cases {
            let mut limits = SandboxLimits::default();
            mutate(&mut limits);

            match limits.validate().unwrap_err() {
                SandboxLimitsError::OutOfRange { field, .. } => assert_eq!(field, expected),
                other @ SandboxLimitsError::Ordering { .. } => {
                    panic!("expected an out-of-range error for {expected}, got {other:?}")
                }
            }
        }
    }

    #[test]
    fn limits_above_their_ceiling_are_rejected() {
        let limits = SandboxLimits {
            compile_timeout_seconds: 121,
            ..SandboxLimits::default()
        };

        assert_eq!(
            limits.validate().unwrap_err(),
            SandboxLimitsError::OutOfRange {
                field: LimitField::CompileTimeoutSeconds,
                value: 121,
                minimum: 1,
                maximum: 120,
            }
        );
    }

    #[test]
    fn a_tmpfs_larger_than_the_memory_ceiling_is_rejected() {
        let limits = SandboxLimits {
            tmpfs_bytes: DEFAULT_MEMORY_BYTES + 1,
            ..SandboxLimits::default()
        };

        assert_eq!(
            limits.validate().unwrap_err(),
            SandboxLimitsError::Ordering {
                smaller: LimitField::TmpfsBytes,
                larger: LimitField::MemoryBytes,
            }
        );
    }

    #[test]
    fn source_and_stdin_may_not_exceed_the_working_filesystem() {
        let cases: [(Mutate, LimitField); 2] = [
            (
                |l| l.max_source_bytes = 3 * 1024 * 1024,
                LimitField::MaxSourceBytes,
            ),
            (
                |l| l.max_stdin_bytes = 3 * 1024 * 1024,
                LimitField::MaxStdinBytes,
            ),
        ];

        for (mutate, smaller) in cases {
            let mut limits = SandboxLimits {
                tmpfs_bytes: 2 * 1024 * 1024,
                ..SandboxLimits::default()
            };
            mutate(&mut limits);

            assert_eq!(
                limits.validate().unwrap_err(),
                SandboxLimitsError::Ordering {
                    smaller,
                    larger: LimitField::TmpfsBytes,
                }
            );
        }
    }

    #[test]
    fn limits_round_trip_through_json_with_snake_case_keys() {
        let limits = SandboxLimits::default();
        let encoded = serde_json::to_string(&limits).expect("limits should serialize");

        assert!(encoded.contains("\"memory_bytes\":134217728"));
        assert_eq!(
            serde_json::from_str::<SandboxLimits>(&encoded).expect("limits should deserialize"),
            limits
        );
    }

    #[test]
    fn unknown_limit_fields_are_rejected_on_deserialization() {
        let encoded = r#"{"memory_bytes":1,"surprise":2}"#;

        assert!(serde_json::from_str::<SandboxLimits>(encoded).is_err());
    }
}
