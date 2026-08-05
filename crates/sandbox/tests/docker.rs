//! Tests that need a real container runtime.
//!
//! These are ignored by default, like `crates/infrastructure/tests/object_storage.rs`.
//! Run the runtime-only test with a Docker daemon available:
//!
//! ```text
//! cargo test -p rux-sandbox --test docker -- --ignored probe
//! ```
//!
//! The remaining tests additionally need the sandbox image from
//! `deploy/playground`, named by `RUX_PLAYGROUND_IMAGE`.

use std::env;
use std::time::Duration;

use rux_sandbox::{
    DockerSandbox, DockerSandboxConfig, SandboxError, SandboxLimits, SandboxMode, SandboxProfile,
    SandboxRequest,
};

#[tokio::test]
#[ignore = "requires a running container runtime"]
async fn probe_reports_the_running_container_runtime_version() {
    let root = tempfile::tempdir().expect("temporary root should be created");
    let sandbox = DockerSandbox::new(DockerSandboxConfig {
        jobs_root: root.path().to_path_buf(),
        image: env::var("RUX_PLAYGROUND_IMAGE")
            .unwrap_or_else(|_| "rux-playground:test".to_owned()),
        probe_timeout: Duration::from_secs(10),
        ..DockerSandboxConfig::default()
    })
    .expect("default limits should validate");

    let version = sandbox.probe().await.expect("the daemon should answer");

    assert!(
        version
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_digit()),
        "expected a server version, got {version:?}"
    );
}

#[tokio::test]
#[ignore = "requires the built rux-playground image"]
async fn hello_world_compiles_and_runs_in_the_sandbox_image() {
    let root = tempfile::tempdir().expect("temporary root should be created");
    let sandbox = sandbox_for(root.path(), SandboxLimits::default());

    let outcome = sandbox
        .execute(&request(
            SandboxMode::Run,
            "Fn Main() {\n\tPrint(\"hello\")\n}\n",
        ))
        .await
        .expect("a hello-world run should complete");

    assert!(
        outcome.build.success,
        "build failed: {}",
        outcome.build.diagnostics
    );
    let program = outcome.program.expect("a run should report its program");
    assert!(program.stdout.contains("hello"));
    assert_eq!(program.exit_code, Some(0));
    assert!(!program.timed_out);
}

#[tokio::test]
#[ignore = "requires the built rux-playground image"]
async fn a_compile_failure_is_a_completed_run_rather_than_an_error() {
    let root = tempfile::tempdir().expect("temporary root should be created");
    let sandbox = sandbox_for(root.path(), SandboxLimits::default());

    let outcome = sandbox
        .execute(&request(SandboxMode::Run, "Fn Main( {\n"))
        .await
        .expect("a failed build is still a completed run");

    assert!(!outcome.build.success);
    assert!(!outcome.build.diagnostics.is_empty());
    assert!(outcome.program.is_none());
}

#[tokio::test]
#[ignore = "requires the built rux-playground image"]
async fn a_wedged_container_is_killed_by_the_outer_deadline() {
    let root = tempfile::tempdir().expect("temporary root should be created");
    // One second for everything, so the outer deadline fires before the
    // in-container timeout ever could.
    let sandbox = sandbox_for(
        root.path(),
        SandboxLimits {
            compile_timeout_seconds: 1,
            run_timeout_seconds: 1,
            startup_grace_seconds: 1,
            ..SandboxLimits::default()
        },
    );

    let error = sandbox
        .execute(&request(
            SandboxMode::Run,
            "Fn Main() {\n\tWhile True {}\n}\n",
        ))
        .await
        .expect_err("an unbounded program should hit the deadline");

    assert!(matches!(error, SandboxError::Timeout));
    assert_eq!(format!("{error:?}"), "SandboxError::Timeout");
}

#[tokio::test]
#[ignore = "requires the built rux-playground image"]
async fn a_run_leaves_behind_neither_a_job_directory_nor_a_container() {
    let root = tempfile::tempdir().expect("temporary root should be created");
    let sandbox = sandbox_for(root.path(), SandboxLimits::default());

    let _ = sandbox
        .execute(&request(SandboxMode::Run, "Fn Main() {}\n"))
        .await;

    assert_eq!(
        std::fs::read_dir(root.path())
            .expect("the jobs root should be readable")
            .count(),
        0,
        "no job directory may survive its run"
    );

    let listed = std::process::Command::new("docker")
        .args(["ps", "--all", "--quiet", "--filter", "name=rux-play-"])
        .output()
        .expect("docker should be runnable");
    assert!(
        listed.stdout.is_empty(),
        "no playground container may survive its run"
    );
}

fn sandbox_for(root: &std::path::Path, limits: SandboxLimits) -> DockerSandbox {
    DockerSandbox::new(DockerSandboxConfig {
        jobs_root: root.to_path_buf(),
        image: required("RUX_PLAYGROUND_IMAGE"),
        limits,
        ..DockerSandboxConfig::default()
    })
    .expect("limits should validate")
}

fn request(mode: SandboxMode, source: &str) -> SandboxRequest {
    SandboxRequest {
        mode,
        profile: SandboxProfile::Debug,
        source: source.to_owned(),
        stdin: String::new(),
    }
}

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set for this test"))
}
