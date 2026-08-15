use crate::job::{JobId, Nonce};
use crate::limits::SandboxLimits;
use crate::request::{SandboxMode, SandboxProfile};

/// Prefix of the container name for every playground run.
pub const CONTAINER_NAME_PREFIX: &str = "rux-play-";
/// Read-only mount point holding the generated job directory.
pub const JOB_MOUNT_PATH: &str = "/job";
/// Writable tmpfs the entry point copies the job into and builds in.
pub const WORK_MOUNT_PATH: &str = "/work";

/// Everything needed to name and mount one run.
///
/// The identifiers are server-generated newtypes and the image is operator
/// configuration, so no field can carry submitted text.
#[derive(Clone, Copy, Debug)]
pub struct DockerRun<'a> {
    /// Identifies the container and the job directory.
    pub job_id: &'a JobId,
    /// Separates trusted output sections from program output.
    pub nonce: &'a Nonce,
    /// Host path of the job directory, mounted read-only at [`JOB_MOUNT_PATH`].
    pub job_directory: &'a str,
    /// Tag of the sandbox image to run.
    pub image: &'a str,
    /// `uid:gid` the container process must run as, when the host has one.
    ///
    /// The job directory is created `0700`, so the container can only read its
    /// input if it runs as the owning user. Passing this explicitly also means
    /// confinement to a non-root user does not depend on the image's `USER`.
    pub user: Option<&'a str>,
    /// What the container should do.
    pub mode: SandboxMode,
    /// Which profile to build with.
    pub profile: SandboxProfile,
}

/// Builds the full `docker` argument vector for one run.
///
/// Every element is either a fixed string, a number derived from `limits`, or a
/// validated identifier. The submitted source and standard input reach the
/// container only as files inside the read-only [`JOB_MOUNT_PATH`] mount, never
/// as an argument and never as an environment variable.
///
/// The working tmpfs is mounted `nosuid,nodev` but explicitly `exec`: the
/// compiler writes the program there and the entry point executes it.
/// Confinement comes from the empty capability set, the dropped network, and
/// the read-only root filesystem instead.
#[must_use]
pub fn docker_argv(run: &DockerRun<'_>, limits: &SandboxLimits) -> Vec<String> {
    let mut argv: Vec<String> = vec!["docker".to_owned(), "run".to_owned(), "--rm".to_owned()];

    argv.push("--name".to_owned());
    argv.push(container_name(run.job_id));

    // Isolation.
    argv.push("--network=none".to_owned());
    argv.push("--read-only".to_owned());
    argv.push("--cap-drop=ALL".to_owned());
    argv.push("--security-opt=no-new-privileges".to_owned());
    if let Some(user) = run.user {
        argv.push(format!("--user={user}"));
    }

    // Resource ceilings. Memory and swap are pinned together so the container
    // cannot escape the memory limit by swapping.
    argv.push(format!("--memory={}", limits.memory_bytes));
    argv.push(format!("--memory-swap={}", limits.memory_bytes));
    argv.push(format!("--cpus={}", format_cpus(limits.cpu_millis)));
    argv.push(format!("--pids-limit={}", limits.pid_limit));

    // Filesystems.
    argv.push(format!(
        "--mount=type=bind,source={},target={JOB_MOUNT_PATH},readonly",
        run.job_directory
    ));
    // Two options here are load-bearing and easy to lose. `exec` must be stated
    // outright: the runtime adds `noexec` to a tmpfs by default, and the
    // compiled program is executed from this mount, so omitting it makes every
    // run fail with a build that succeeded and an artifact that will not run.
    // `mode=1777` matters because the mount otherwise arrives owned by root
    // with 0755, and the entry point's first act is to stage the job into it.
    argv.push(format!(
        "--tmpfs={WORK_MOUNT_PATH}:rw,exec,nosuid,nodev,mode=1777,size={}",
        limits.tmpfs_bytes
    ));
    argv.push(format!("--workdir={WORK_MOUNT_PATH}"));

    argv.push(run.image.to_owned());

    // The entry point's own arguments: two enums, a hex nonce, and three
    // integers from the configured limits. The in-container `timeout(1)` is the
    // primary bound on a run, so the limits have to travel here - baking them
    // into the image would leave the operator's configuration with no effect on
    // anything but the outer deadline.
    argv.push(run.mode.as_arg().to_owned());
    argv.push(run.profile.as_arg().to_owned());
    argv.push(run.nonce.as_str().to_owned());
    argv.push(limits.compile_timeout_seconds.to_string());
    argv.push(limits.run_timeout_seconds.to_string());
    argv.push(limits.max_output_bytes.to_string());

    argv
}

/// Builds the container name used to reap a wedged run.
#[must_use]
pub fn container_name(job_id: &JobId) -> String {
    format!("{CONTAINER_NAME_PREFIX}{job_id}")
}

/// Renders a CPU quota in the fixed-point form the Docker CLI accepts.
fn format_cpus(cpu_millis: u32) -> String {
    format!("{}.{:03}", cpu_millis / 1_000, cpu_millis % 1_000)
}

#[cfg(test)]
mod tests {
    use super::{DockerRun, container_name, docker_argv, format_cpus};
    use crate::job::{JobId, Nonce};
    use crate::limits::SandboxLimits;
    use crate::request::{SandboxMode, SandboxProfile};

    const JOB: &str = "0123456789abcdef0123456789abcdef";
    const NONCE: &str = "fedcba9876543210fedcba9876543210";

    const JOB_DIRECTORY: &str = "/var/lib/rux-playground/jobs/0123456789abcdef0123456789abcdef";
    const IMAGE: &str = "rux-playground:0.4.0";

    fn argv(mode: SandboxMode, profile: SandboxProfile) -> Vec<String> {
        argv_as(mode, profile, None)
    }

    fn argv_as(mode: SandboxMode, profile: SandboxProfile, user: Option<&str>) -> Vec<String> {
        let job_id = JobId::new(JOB).unwrap();
        let nonce = Nonce::new(NONCE).unwrap();
        let run = DockerRun {
            job_id: &job_id,
            nonce: &nonce,
            job_directory: JOB_DIRECTORY,
            image: IMAGE,
            user,
            mode,
            profile,
        };

        docker_argv(&run, &SandboxLimits::default())
    }

    #[test]
    fn every_hardening_flag_is_present() {
        let argv = argv(SandboxMode::Run, SandboxProfile::Debug);

        for flag in [
            "--rm",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--memory=134217728",
            "--memory-swap=134217728",
            "--cpus=0.500",
            "--pids-limit=32",
            "--workdir=/work",
        ] {
            assert!(argv.iter().any(|item| item == flag), "missing {flag}");
        }
    }

    #[test]
    fn memory_and_swap_are_pinned_together_so_swapping_cannot_widen_the_limit() {
        let argv = argv(SandboxMode::Run, SandboxProfile::Debug);
        let value = |prefix: &str| {
            argv.iter()
                .find_map(|item| item.strip_prefix(prefix).map(str::to_owned))
                .expect("flag should be present")
        };

        assert_eq!(value("--memory="), value("--memory-swap="));
    }

    #[test]
    fn the_job_directory_is_mounted_read_only() {
        let argv = argv(SandboxMode::Run, SandboxProfile::Debug);
        let mount = argv
            .iter()
            .find(|item| item.starts_with("--mount="))
            .expect("job mount should be present");

        assert!(mount.contains("type=bind"));
        assert!(mount.ends_with(",target=/job,readonly"));
    }

    #[test]
    fn the_working_tmpfs_is_bounded_and_allows_execution_of_the_built_program() {
        let argv = argv(SandboxMode::Run, SandboxProfile::Debug);
        let tmpfs = argv
            .iter()
            .find(|item| item.starts_with("--tmpfs="))
            .expect("work tmpfs should be present");

        assert!(tmpfs.contains("nosuid"));
        assert!(tmpfs.contains("nodev"));
        assert!(tmpfs.contains("size=33554432"));
        assert!(
            tmpfs.contains("mode=1777"),
            "the sandbox user must be able to stage the job into this mount"
        );
        // Asserting `noexec` is merely absent is not enough: the runtime adds
        // it by default, so `exec` has to be requested explicitly.
        assert!(
            tmpfs.contains(",exec,"),
            "the compiled program is executed from this mount"
        );
        assert!(!tmpfs.contains("noexec"));
    }

    #[test]
    fn the_container_is_named_from_the_job_id_so_it_can_be_reaped() {
        let argv = argv(SandboxMode::Run, SandboxProfile::Debug);
        let position = argv.iter().position(|item| item == "--name").unwrap();

        assert_eq!(argv[position + 1], format!("rux-play-{JOB}"));
        assert_eq!(
            container_name(&JobId::new(JOB).unwrap()),
            argv[position + 1]
        );
    }

    #[test]
    fn the_vector_holds_nothing_beyond_fixed_flags_and_server_generated_values() {
        // `docker_argv` cannot receive submitted source or standard input --
        // they are not parameters. This pins the complement: every element is
        // either a constant or derived from a value the server generated.
        let argv = argv(SandboxMode::Run, SandboxProfile::Debug);
        let permitted: Vec<String> = [
            "docker",
            "run",
            "--rm",
            "--name",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--memory=134217728",
            "--memory-swap=134217728",
            "--cpus=0.500",
            "--pids-limit=32",
            "--tmpfs=/work:rw,exec,nosuid,nodev,mode=1777,size=33554432",
            "--workdir=/work",
            "run",
            "debug",
            "5",
            "3",
            "16384",
        ]
        .iter()
        .map(|item| (*item).to_owned())
        .chain([
            format!("rux-play-{JOB}"),
            format!("--mount=type=bind,source={JOB_DIRECTORY},target=/job,readonly"),
            IMAGE.to_owned(),
            NONCE.to_owned(),
        ])
        .collect();

        for item in &argv {
            assert!(permitted.contains(item), "unexpected argument {item:?}");
        }
    }

    #[test]
    fn the_image_is_followed_by_the_entry_point_contract_in_order() {
        for (mode, profile, expected) in [
            (SandboxMode::Run, SandboxProfile::Debug, ["run", "debug"]),
            (
                SandboxMode::Build,
                SandboxProfile::Release,
                ["build", "release"],
            ),
            (SandboxMode::Fmt, SandboxProfile::Debug, ["fmt", "debug"]),
        ] {
            let argv = argv(mode, profile);
            let position = argv
                .iter()
                .position(|item| item == "rux-playground:0.4.0")
                .expect("image should be present");

            assert_eq!(argv[position + 1], expected[0]);
            assert_eq!(argv[position + 2], expected[1]);
            assert_eq!(argv[position + 3], NONCE);
            // The three limits the entry point enforces in-container. Their
            // arity is a contract with playground/run-job.
            assert_eq!(argv[position + 4], "5");
            assert_eq!(argv[position + 5], "3");
            assert_eq!(argv[position + 6], "16384");
            assert_eq!(
                argv.len(),
                position + 7,
                "the entry point takes six arguments"
            );
        }
    }

    #[test]
    fn a_supplied_user_is_passed_so_the_private_job_mount_stays_readable() {
        let argv = argv_as(SandboxMode::Run, SandboxProfile::Debug, Some("998:998"));

        assert!(argv.iter().any(|item| item == "--user=998:998"));
    }

    #[test]
    fn no_user_flag_is_emitted_when_the_host_has_no_owner_to_name() {
        let argv = argv(SandboxMode::Run, SandboxProfile::Debug);

        assert!(!argv.iter().any(|item| item.starts_with("--user")));
    }

    #[test]
    fn no_environment_variable_is_ever_passed() {
        let argv = argv(SandboxMode::Run, SandboxProfile::Debug);

        assert!(
            !argv
                .iter()
                .any(|item| item == "-e" || item.starts_with("--env")),
            "the container must receive no environment"
        );
    }

    #[test]
    fn cpu_quotas_render_as_fixed_point_decimals() {
        assert_eq!(format_cpus(500), "0.500");
        assert_eq!(format_cpus(1_000), "1.000");
        assert_eq!(format_cpus(2_250), "2.250");
        assert_eq!(format_cpus(100), "0.100");
    }
}
