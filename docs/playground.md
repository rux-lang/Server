# Playground

The playground compiles and runs a submitted Rux program inside a throwaway container and returns its diagnostics and output. It is anonymous, stores nothing, and is the only part of this server that executes code it did not build. Everything below is the contract: the endpoints, the isolation each run is subject to, and the two-process topology that keeps a compromised run away from the package registry sharing the host.

The playground is optional. When `RUX_PLAYGROUND_ENABLED` is false the router is never merged, so both endpoints answer `404` from the fallback rather than advertising an endpoint that always fails, and no playground metrics are emitted at all. It is disabled by default, and `playground_enabled` in [production provisioning](production-provisioning.md) is the operator's opt-in.

## Topology and trust boundary

Access to the Docker socket is equivalent to root on the host. The registry and its database live on the same droplet, so `rux-server` never touches a container runtime; a second process does, under a second user:

```
rux-server  ──NDJSON over /run/rux-playground/run.sock──▶  rux-playgroundd  ──docker run --rm──▶  container
(hardened, no docker access)                               (user rux-playground, group docker)
```

`rux-server` reaches the broker through a shared `rux-playground` group and holds no `docker` membership of its own. Its unit keeps `RestrictNamespaces=true`, an empty `CapabilityBoundingSet`, and `ProtectSystem=strict`; the only concession is that `/run/rux-playground` is listed read-write, because connecting to a unix socket is a write. The broker's unit is deliberately weaker — it must be able to drive the Docker CLI — and that asymmetry is the point: hardening the broker is defence in depth, while what actually contains a run is the container flag set below.

Nothing user-authored ever reaches `argv` or an environment variable. The source and standard input arrive only as files inside a read-only bind mount. The container's arguments are a mode enum, a profile enum, a random per-run nonce, and three integers from the configured limits. `JobId` and `Nonce` are newtypes restricted to 32 lowercase hex characters, which makes that a type guarantee rather than a convention.

## Endpoints

Both endpoints live under `/v1/playground`. Request and response bodies are JSON, and every success wraps its payload in the standard `data` envelope described in the [HTTP API contract](http-api.md).

`POST /v1/playground/run` submits one program. The request body is closed — unknown fields are rejected — and the whole body is limited to 64 KiB:

```json
{
  "mode": "run",
  "profile": "debug",
  "source": "Fn Main() {}\n",
  "stdin": ""
}
```

`source` is required. `mode` is `run`, `build`, or `fmt` and defaults to `run`; `profile` is `debug` or `release` and defaults to `debug`; `stdin` defaults to empty. `mode: "build"` compiles without executing, and `mode: "fmt"` reformats and returns the source instead of building it.

The request must carry an exact-match `Origin` header. The endpoint is anonymous and executes code, so without that check any page could drive the sandbox from a visitor's browser. `GET /v1/playground/limits` is deliberately **not** origin-checked: it is a side-effect-free public document with no credentials and no user data, and an exact-origin requirement would make it unreadable to anything but the web client.

A successful response reports the build, and — for `run` — the program:

```json
{
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
      "signal": null,
      "timed_out": false,
      "duration_ms": 7
    }
  }
}
```

`program` is absent for `build` and `fmt`, and absent for `run` when the build failed. `formatted` carries the reformatted source and is present only for `fmt`. `exit_code` is null when the program was killed by a signal, and `signal` is null otherwise; a program stopped by its own timeout reports `timed_out: true`. A truncation flag means output reached the per-stream cap and was cut on a UTF-8 boundary, so the value is always valid UTF-8.

**A failed compile is a `200`, not a problem.** It reports `build.success: false` with the compiler's diagnostics. Problem responses are reserved for transport-level faults — the submission never ran, or the service could not run it. Clients must branch on `build.success` rather than on the status code.

`GET /v1/playground/limits` returns the envelope every run is subject to, so a client can validate and display bounds without hardcoding them:

```json
{
  "data": {
    "max_source_bytes": 32768,
    "max_stdin_bytes": 16384,
    "max_output_bytes": 16384,
    "compile_timeout_seconds": 5,
    "run_timeout_seconds": 3,
    "memory_bytes": 134217728,
    "cpu_millis": 500
  }
}
```

The broker is authoritative for these values: they are what it is actually enforcing, not what the API was configured to believe. The API keeps its own copy of the bounds only to reject an oversized submission before it occupies a socket connection or a sandbox permit.

## Problem responses

| Status | `code` | Cause |
| --- | --- | --- |
| `403` | `origin_not_allowed` | Missing or non-matching `Origin` on `POST /run`. |
| `422` | `invalid_request` | The submission broke a documented bound. |
| `429` | `rate_limited` | The playground abuse tier was exceeded; carries `Retry-After`. |
| `503` | `playground_unavailable` | The sandbox is not answering; carries `Retry-After: 1`. |
| `504` | `request_timeout` | The run exceeded the server deadline. |

The `422` uses the same shape as every other validation failure in this API: problem `code` is `invalid_request`, and the specific `invalid_playground_request` code appears inside `errors[]` with a detail describing which bound was broken. That detail names a size, a bound, or a field, and never echoes the submitted source or standard input.

An internal broker fault is reported as the same `503 playground_unavailable` as a stopped sandbox. The caller's situation is identical, and a problem must never reveal that something internal went wrong. A body larger than the 64 KiB limit surfaces as the same `422` rather than a `413`, which is what keeps the framework's own error body from ever escaping; the body limit sits above the source and standard-input bounds combined, so anything larger would fail an application bound anyway.

## Limits

| Limit | Default | Configurable |
| --- | --- | --- |
| Source | 32 KiB | fixed |
| Standard input | 16 KiB | fixed |
| Output per stream | 16 KiB | fixed |
| Compile timeout | 5 s | `RUX_PLAYGROUND_COMPILE_TIMEOUT_SECONDS` (1–120) |
| Run timeout | 3 s | `RUX_PLAYGROUND_RUN_TIMEOUT_SECONDS` (1–120) |
| Startup grace | 5 s | fixed |
| Memory (swap disabled) | 128 MiB | `RUX_PLAYGROUND_MEMORY_BYTES` (16 MiB–4 GiB) |
| CPU | 0.5 | `RUX_PLAYGROUND_CPU_MILLIS` (100–16000) |
| Processes | 32 | fixed |
| Writable tmpfs | 32 MiB | fixed |
| Concurrent runs | 2 | `RUX_PLAYGROUND_MAX_CONCURRENCY` (1–16) |

Source is bounded in bytes rather than characters, and a submission is rejected if it is empty, oversized, contains a NUL, or contains a control character outside tab, newline, and carriage return. The compile timeout is the number most likely to need adjusting after real use, which is why it is a bounded configuration value rather than a constant.

The in-container `timeout(1)` is the primary bound on a run. The broker also wraps the whole child in `compile_timeout + run_timeout + startup_grace` and, on expiry, kills the container by name — that outer deadline is a backstop for a container that never reaches its entry point, not the mechanism that stops a long-running program.

## Container isolation

Every run is a fresh `docker run --rm` with a fixed flag set. Each flag is load-bearing:

| Flag | Why |
| --- | --- |
| `--network=none` | No egress, no lateral movement, no package resolution at request time. Only loopback exists. |
| `--read-only` | The image's filesystem cannot be modified; the only writable location is the working tmpfs. |
| `--cap-drop=ALL` | No capabilities at all. |
| `--security-opt=no-new-privileges` | A setuid binary inside the image cannot raise privilege. |
| `--user=<uid>:<gid>` | Runs as the host user owning the job directory, so confinement does not depend on the image's `USER`. |
| `--memory` / `--memory-swap` | Pinned to the same value, so the container cannot escape the memory limit by swapping. |
| `--cpus` | Bounds CPU so one run cannot starve the registry sharing the host. |
| `--pids-limit` | Stops a fork bomb. |
| `--mount type=bind,…,readonly` | The job directory at `/job` — the only path submitted content travels. Read-only, so a run cannot rewrite its own inputs. |
| `--tmpfs /work:rw,exec,nosuid,nodev,mode=1777,size=…` | The working copy, bounded and discarded with the container. |

Two `--tmpfs` options are easy to lose and were caught only by running the image. `exec` must be stated outright: the runtime adds `noexec` by default, which leaves every run compiling successfully and then failing to execute its own artifact. `mode=1777` matters because the mount otherwise arrives root-owned `0755` and the entry point cannot stage the job into it. Confinement comes from the empty capability set, the dropped network, and the read-only root — not from the tmpfs being non-executable.

The per-job directory on the host is created `0700` under `/var/lib/rux-playground/jobs` and removed by a guard that runs on every exit path, including panic and cancellation. Because it is `0700`, a container running as any other uid could not read its own input, which is why `--user` names the owning uid explicitly.

## Output framing

One stdout stream carries several logically separate sections, so the entry point separates them with a sentinel line: an ASCII record separator (`0x1e`), the run's nonce, a colon, and the section name. Sections are `build`, `stdout`, `stderr`, `formatted`, and `status`.

```
\x1e<nonce>:stdout
hello
\x1e<nonce>:status
build_exit=0
build_ms=120
run_exit=0
run_ms=7
```

The nonce is fresh per run and never leaves the host, so a program that prints a guessed sentinel cannot forge a section: a sentinel with the wrong nonce is ordinary output. Unknown section names are ignored, and a missing `status` section is a framing error rather than a silently empty result. The `status` trailer carries `build_exit`, `build_ms`, and — when a program ran — `run_exit`, `run_ms`, `run_signal`, and `timed_out`; a signalled program reports no exit code. Each section is capped in the container by `head -c` and again by a bounded reader in the broker, which keeps draining past the cap so a chatty program can neither exhaust server memory nor wedge itself on a full pipe.

## Operating the sandbox

**Adding a standard package requires an image rebuild.** The sandbox runs with `--network=none`, so the compiler cannot resolve a dependency at request time; packages are seeded into the image's cache at build time, when the network is still reachable. `playground_packages` names what is seeded and `playground_allowlist` names the `Root:Namespace` imports the broker will honour, and provisioning refuses an allowlist naming a package the image was never seeded with. Adding one means rebuilding the image, rotating `playground_rux_version`, and redeploying.

The image is built by `deploy/playground/build-image.sh <version> <sha256> [packages]`, which refuses to proceed without a checksum: the build downloads a release tarball over the network, and an unverified download is the one step of this design a network attacker could turn into arbitrary code execution inside every subsequent run. It builds with `--network=host`, because the production host denies containers egress on purpose — the firewall's `forward` chain drops it and the Docker daemon is configured with no default bridge at all.

`deploy/playground/test-image.sh` exercises the image directly, with no server involved: a syntax error yields diagnostics and a non-zero build status, an infinite loop is killed by the in-container timeout, a fork bomb hits the pid limit, an oversized allocation is OOM-killed, a socket call fails because only loopback exists, and a forged sentinel cannot inject a section.

Metrics, alerts, and the failure runbook are documented in [observability](observability.md) and the [alert runbook](observability-runbook.md). The playground's rate-limit tier and its defaults are in [abuse controls](abuse-controls.md). Availability is deliberately absent from `/health/ready`: a stopped sandbox is a degraded playground, not a degraded registry, and must never pull the registry out of rotation.

## Configuration

The API reads `RUX_PLAYGROUND_ENABLED`, `RUX_PLAYGROUND_SOCKET`, `RUX_PLAYGROUND_TIMEOUT_SECONDS` (at least `RUX_REQUEST_TIMEOUT_SECONDS`, at most 120), `RUX_PLAYGROUND_MAX_CONCURRENCY`, `RUX_RATE_LIMIT_PLAYGROUND_PER_MINUTE`, and `RUX_RATE_LIMIT_PLAYGROUND_BURST`.

The broker reads `RUX_PLAYGROUND_IMAGE` (required), `RUX_PLAYGROUND_SOCKET`, `RUX_PLAYGROUND_JOBS_ROOT`, `RUX_PLAYGROUND_DOCKER_BINARY`, `RUX_PLAYGROUND_PACKAGES`, `RUX_PLAYGROUND_MEMORY_BYTES`, `RUX_PLAYGROUND_CPU_MILLIS`, `RUX_PLAYGROUND_COMPILE_TIMEOUT_SECONDS`, `RUX_PLAYGROUND_RUN_TIMEOUT_SECONDS`, `RUX_PLAYGROUND_MAX_CONCURRENCY`, and `RUX_PLAYGROUND_REQUEST_TIMEOUT_SECONDS`. Every bound is validated at startup, so a nonsensical combination fails to start rather than failing on the first run. The two processes read their own environment files; `RUX_PLAYGROUND_MAX_CONCURRENCY` bounds API admission on one side and broker execution on the other.

The playground stores nothing. There is no table, no retention policy, and no migration. Permalinks were explicitly descoped, and adding them later would be the first change here to need one.
