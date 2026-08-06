# Observability

The API writes structured JSON to stdout for journald, exposes Prometheus metrics on a separate private listener, and can optionally export W3C-correlated traces over OTLP/gRPC. The public API and OpenAPI document do not include operational metrics.

## Runtime configuration

`RUX_METRICS_BIND_ADDRESS` defaults to `127.0.0.1:9464`. A non-loopback address is rejected unless `RUX_METRICS_ALLOW_NON_LOOPBACK=true`; that escape hatch exists for local Docker and CI only. Production must keep the listener on loopback. `RUX_DEPENDENCY_PROBE_INTERVAL_SECONDS` defaults to 15 and controls the continuous PostgreSQL and object-storage readiness observations.

Tracing always creates trace and span identifiers for log correlation. `OTEL_TRACES_EXPORTER` defaults to `none`. Set it to `otlp` to enable the batch OTLP/gRPC exporter, then configure the standard `OTEL_EXPORTER_OTLP_ENDPOINT` or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, headers, timeout, sampler, `OTEL_SERVICE_NAME`, and `OTEL_RESOURCE_ATTRIBUTES` variables. Export failures must not block API traffic. Headers and other exporter credentials must never appear in logs.

Request telemetry uses the server-generated request ID, method, matched route template, and response status. It does not record raw unmatched paths, query strings, headers, bodies, client addresses, user identities, or registry identities. The stable metric families are:

- `rux_http_server_requests_total`, `rux_http_server_request_duration_seconds`, and `rux_http_server_active_requests`.
- `rux_dependency_ready`, `rux_dependency_probes_total`, `rux_dependency_probe_duration_seconds`, and `rux_dependency_last_probe_unixtime_seconds`.
- `rux_orphan_cleanup_runs_total`, `rux_orphan_cleanup_duration_seconds`, `rux_orphan_cleanup_objects_total`, and `rux_orphan_cleanup_last_success_unixtime_seconds`.
- `rux_playground_runs_total`, `rux_playground_duration_seconds`, `rux_playground_active`, and `rux_playground_available`.
- `rux_postgres_backup_*`, `rux_postgres_backup_check_*`, `rux_postgres_wal_archive_*`, and `rux_recovery_rehearsal_*` are emitted by root-managed recovery jobs rather than the API.

Playground runs are labelled by `mode` (`run`, `build`, `fmt`) and `outcome` (`succeeded`, `build_failed`, `rejected`, `unavailable`, `timed_out`, `internal`). Both are fixed vocabularies: nothing derived from the submission or the caller is ever a label, so the series cannot be made to grow without bound and cannot carry submitted source. `rux_playground_active` is driven by a guard that decrements on every exit path, including a cancelled request, so it cannot drift upward under the load that makes it worth having. `rux_playground_available` reports whether the broker answered its most recent probe and is deliberately **not** part of the readiness aggregate — a stopped sandbox is a degraded playground, not a degraded registry, and must never pull the registry out of rotation. The whole family is absent when `RUX_PLAYGROUND_ENABLED` is false, because the watcher never starts.

The request histogram also enforces the launch [performance budgets](performance.md) through traffic-gated warning alerts for successful public reads, search, and publication. The minimum request counts prevent sparse traffic from producing unstable quantiles; database size and frontend budgets remain explicit release and CI checks rather than runtime metric families.

## Local Prometheus and Grafana

The `observability` Compose profile provisions Prometheus, its alert rules, and the read-only `Rux Server Overview` Grafana dashboard. Because the API runs on the host while Prometheus runs in Docker, start the API with an explicit development-only bridge bind:

```powershell
docker compose --profile observability up -d --wait
.\Import-LocalEnv.ps1
$env:RUX_METRICS_BIND_ADDRESS = "0.0.0.0:9464"
$env:RUX_METRICS_ALLOW_NON_LOOPBACK = "true"
cargo run -p rux-server
```

Prometheus is available at <http://localhost:9090> and Grafana at <http://localhost:3001>. Both published ports remain bound to host loopback. Stop the profile with `docker compose --profile observability down`; omit `--volumes` to retain local dashboards' data.

Validate repository-owned configuration with:

```bash
docker compose --profile observability config --quiet
docker run --rm --entrypoint /bin/promtool -v "$PWD/deploy/observability:/etc/prometheus:ro" prom/prometheus:v3.5.0 check config /etc/prometheus/prometheus.yml
docker run --rm --entrypoint /bin/promtool -v "$PWD/deploy/observability:/work:ro" -w /work prom/prometheus:v3.5.0 test rules alerts.test.yml
```

Prometheus evaluates the checked-in rules, but the checked-in configuration does not configure notification receivers. Host provisioning must route warning and critical alerts without committing receiver credentials. See the [observability runbook](observability-runbook.md).

## Production services

The production provisioner installs checksum-pinned Prometheus and Alertmanager binaries plus a pinned Grafana package. Their HTTP listeners bind only to loopback: Prometheus on `127.0.0.1:9090`, Alertmanager on `127.0.0.1:9093`, and Grafana on `127.0.0.1:3001`. Prometheus scrapes the API's loopback metrics listener and the recovery node exporter on `127.0.0.1:9100`, then sends alerts to Alertmanager. Warning and critical alerts use distinct secret webhook URLs supplied to Ansible outside the repository.

Reach Grafana only through an operator SSH tunnel:

```bash
ssh -L 3001:127.0.0.1:3001 rux-admin@registry-host
```

Then open `http://127.0.0.1:3001`. The provisioned administrator password is stored in a root-owned environment file and must be rotated through the same secret-input workflow.
