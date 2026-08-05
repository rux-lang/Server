use serde::{Deserialize, Serialize};

/// What the container should do with the submitted source.
///
/// Together with [`SandboxProfile`] this is the only caller-influenced value
/// that ever reaches the container's argument vector, and it is an enum
/// precisely so that the mapping to text is fixed at compile time.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxMode {
    /// Compile, then execute the resulting program.
    #[default]
    Run,
    /// Compile only, and report diagnostics.
    Build,
    /// Reformat the source and return it.
    Fmt,
}

impl SandboxMode {
    /// Returns the fixed argument passed to the container entry point.
    #[must_use]
    pub const fn as_arg(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Build => "build",
            Self::Fmt => "fmt",
        }
    }

    /// Reports whether this mode executes the compiled program.
    #[must_use]
    pub const fn executes_program(self) -> bool {
        matches!(self, Self::Run)
    }
}

/// Which compiler profile the container should build with.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    /// Unoptimized build with debug assertions.
    #[default]
    Debug,
    /// Optimized build.
    Release,
}

impl SandboxProfile {
    /// Returns the fixed argument passed to the container entry point.
    #[must_use]
    pub const fn as_arg(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    /// Returns the build output directory the compiler writes for this profile.
    #[must_use]
    pub const fn output_directory(self) -> &'static str {
        match self {
            Self::Debug => "Debug",
            Self::Release => "Release",
        }
    }
}

/// One playground run as submitted by a caller.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct SandboxRequest {
    /// What to do with the source.
    pub mode: SandboxMode,
    /// Which compiler profile to build with.
    pub profile: SandboxProfile,
    /// The submitted program source.
    pub source: String,
    /// Text piped to the program's standard input.
    #[serde(default)]
    pub stdin: String,
}

/// The result of one playground run.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct SandboxOutcome {
    /// How the compile step went.
    pub build: BuildOutcome,
    /// How the program step went; absent unless the mode executes a program
    /// and the build succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<ProgramOutcome>,
    /// Reformatted source; only present for [`SandboxMode::Fmt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formatted: Option<String>,
}

/// The compile step of a run.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct BuildOutcome {
    /// Whether the compiler exited successfully.
    pub success: bool,
    /// Compiler diagnostics, already truncated to the output limit.
    pub diagnostics: String,
    /// Whether `diagnostics` was cut short by the output limit.
    pub diagnostics_truncated: bool,
    /// Wall-clock duration of the compile step.
    pub duration_ms: u64,
}

/// The program step of a run.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub struct ProgramOutcome {
    /// Program standard output, already truncated to the output limit.
    pub stdout: String,
    /// Whether `stdout` was cut short by the output limit.
    pub stdout_truncated: bool,
    /// Program standard error, already truncated to the output limit.
    pub stderr: String,
    /// Whether `stderr` was cut short by the output limit.
    pub stderr_truncated: bool,
    /// Process exit code, absent when the program was killed by a signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Signal that killed the program, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// Whether the program hit its own run timeout.
    pub timed_out: bool,
    /// Wall-clock duration of the program step.
    pub duration_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::{SandboxMode, SandboxOutcome, SandboxProfile, SandboxRequest};

    #[test]
    fn mode_and_profile_arguments_are_fixed_lowercase_words() {
        assert_eq!(SandboxMode::Run.as_arg(), "run");
        assert_eq!(SandboxMode::Build.as_arg(), "build");
        assert_eq!(SandboxMode::Fmt.as_arg(), "fmt");
        assert_eq!(SandboxProfile::Debug.as_arg(), "debug");
        assert_eq!(SandboxProfile::Release.as_arg(), "release");
    }

    #[test]
    fn only_run_mode_executes_the_compiled_program() {
        assert!(SandboxMode::Run.executes_program());
        assert!(!SandboxMode::Build.executes_program());
        assert!(!SandboxMode::Fmt.executes_program());
    }

    #[test]
    fn profiles_name_the_directory_the_compiler_writes() {
        assert_eq!(SandboxProfile::Debug.output_directory(), "Debug");
        assert_eq!(SandboxProfile::Release.output_directory(), "Release");
    }

    #[test]
    fn modes_and_profiles_use_snake_case_on_the_wire() {
        for (mode, encoded) in [
            (SandboxMode::Run, "\"run\""),
            (SandboxMode::Build, "\"build\""),
            (SandboxMode::Fmt, "\"fmt\""),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), encoded);
            assert_eq!(serde_json::from_str::<SandboxMode>(encoded).unwrap(), mode);
        }

        assert_eq!(
            serde_json::from_str::<SandboxProfile>("\"release\"").unwrap(),
            SandboxProfile::Release
        );
    }

    #[test]
    fn a_request_defaults_to_a_debug_run_with_empty_stdin() {
        let request: SandboxRequest =
            serde_json::from_str(r#"{"mode":"run","profile":"debug","source":"x"}"#).unwrap();

        assert_eq!(request.mode, SandboxMode::Run);
        assert_eq!(request.profile, SandboxProfile::Debug);
        assert_eq!(request.stdin, "");
    }

    #[test]
    fn unknown_request_fields_are_rejected() {
        let encoded = r#"{"mode":"run","profile":"debug","source":"x","extra":1}"#;

        assert!(serde_json::from_str::<SandboxRequest>(encoded).is_err());
    }

    #[test]
    fn absent_outcome_sections_are_omitted_rather_than_encoded_as_null() {
        let encoded = serde_json::to_string(&SandboxOutcome::default()).unwrap();

        assert!(!encoded.contains("program"));
        assert!(!encoded.contains("formatted"));
        assert!(encoded.contains("\"diagnostics_truncated\":false"));
    }

    #[test]
    fn an_outcome_round_trips_through_json() {
        let outcome = SandboxOutcome {
            formatted: Some("Fn Main() {}\n".to_owned()),
            ..SandboxOutcome::default()
        };
        let encoded = serde_json::to_string(&outcome).unwrap();

        assert_eq!(
            serde_json::from_str::<SandboxOutcome>(&encoded).unwrap(),
            outcome
        );
    }
}
