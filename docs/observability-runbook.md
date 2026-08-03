# Observability alert runbook

Start every investigation by recording the alert start time, instance, dependency, and recent deployment. Check `curl --fail http://127.0.0.1:8080/health/live`, `curl --fail http://127.0.0.1:8080/health/ready`, the Prometheus target page, and recent service logs with `journalctl -u rux-server --since "30 minutes ago" -o cat`. Search a reported `trace_id` as an exact JSON value to follow one request. Do not paste credentials, cookies, authorization headers, or OTLP headers into tickets.

## RuxServerApiMetricsMissing

Impact: application telemetry is unavailable and the API may also be down. Confirm the process and both listeners with `systemctl status rux-server` and `ss -ltn`. Query liveness directly. If the API is healthy, verify Prometheus can reach `127.0.0.1:9464/metrics` and inspect its scrape error. Restart only after preserving logs and identifying bind, firewall, configuration, or resource failures. Resolve when five consecutive scrapes succeed.

## RuxServerDependencyUnavailable

Impact: readiness is failing and normal API operations involving the named dependency may return 503. Inspect `/health/ready` and the dependency probe duration panel. For `postgresql`, verify the service, connections, disk, and `SELECT 1` using operator credentials. For `object_storage`, verify provider status, DNS/TLS, bucket access, and credential expiry without printing secrets. Prefer restoring the dependency over restarting a healthy API. Resolve after readiness remains healthy for five minutes.

## RuxServerDependencyProbeStale

Impact: dependency health is unknown, usually because the Tokio worker is stalled or telemetry stopped updating. Confirm the process, scrape timestamp, CPU/memory pressure, and logs for task panics. Compare `/health/ready` with the stored gauge. Escalate if the worker remains stale after one controlled service restart. Resolve when both dependency timestamps update every configured interval.

## RuxServerElevatedServerErrors

Impact: clients are receiving a sustained elevated rate of server errors. Break down `rux_http_server_requests_total` by matched route and status, then correlate the time window with dependency state, deployments, and JSON logs. Use trace IDs to inspect representative failures. Roll back a recent release with the reviewed [application rollback playbook](releases.md#manual-application-rollback) when errors began immediately after it and schema compatibility is confirmed; otherwise restore the failing dependency or reduce traffic through the documented abuse controls. Resolve after the ten-minute ratio remains below threshold.

## RuxServerReadLatencyBudgetExceeded

Impact: successful resolver, metadata, discovery, or download requests exceed the 250 ms p95 launch budget. Use the latency dashboard to identify the matched route, then compare database CPU, connections, locks, disk latency, and request volume with the last deployment. Check slow-query logs and representative trace IDs without copying package identities into public incidents. Roll back a causal compatible release or reduce load while repairing the query or capacity bottleneck. Resolve after p95 remains within budget for 15 minutes.

## RuxServerSearchLatencyBudgetExceeded

Impact: catalog browsing or literal search exceeds the 500 ms p95 launch budget. Separate empty browse, exact identity, keyword/filter, and full-text requests using trace-safe local reproduction, then inspect PostgreSQL plans, statistics, index use, and resource pressure. Run the documented performance harness against a disposable database before changing indexes. Resolve after the threshold remains clear for 15 minutes at sufficient traffic.

## RuxServerPublicationLatencyBudgetExceeded

Impact: successful publications exceed the five-second p95 budget. Compare upload size, temporary-disk pressure, artifact inspection time, PostgreSQL locks, and object-storage latency. Do not weaken validation, atomicity, or checksum verification to reduce latency. Check provider status and recent releases, then reproduce with both the typical and maximum performance fixtures. Resolve after the one-hour p95 returns within budget.

## RuxServerCleanupFailed

Impact: newly orphaned object versions are not being evaluated for removal. Inspect the structured `orphan cleanup sweep failed` event and its bounded error kind. Verify PostgreSQL and object-storage availability and permissions. Do not manually delete objects while references are uncertain. Resolve after a successful bounded sweep and confirm the last-success timestamp advances.

## RuxServerCleanupStale

Impact: orphan cleanup has not completed successfully for three hours, allowing storage leakage. Check run totals, the last-success timestamp, dependency health, and worker logs. Confirm the configured interval is reasonable and the worker was not lost during restart. Follow the cleanup-failure procedure for dependency errors. Resolve after a successful sweep; investigate repeated staleness before increasing intervals.

## RuxServerCleanupDeletionFailures

Impact: eligible, unreferenced object versions were found but individual deletions failed. Inspect failure counts and logs, then verify exact-version delete permissions, provider availability, and request throttling. The sweep is retry-safe; do not bulk-delete or remove versioning. Resolve when a later sweep deletes or safely reclassifies the candidates and no new failures occur for one hour.

## RuxServerBackupMetricsMissing

Impact: PostgreSQL backup, WAL, and rehearsal status is unknown. Check `rux-node-exporter.service`, its loopback `127.0.0.1:9100` listener, the Prometheus target error, and files under `/var/lib/rux-recovery/metrics`. Do not edit metric values. Resolve after five consecutive successful scrapes.

## RuxServerPostgresBackupFailed

Impact: the newest full, differential, or repository-check operation failed. Inspect the matching `rux-pgbackrest-*` unit and pgBackRest logs without printing its configuration. Verify PostgreSQL, disk, DNS/TLS, provider status, repository credentials, and the encrypted repository. Rerun the failed unit only after identifying the cause. Resolve when it succeeds and its last-success timestamp advances.

## RuxServerPostgresBackupStale

Impact: the recoverable base is older than policy permits, increasing data-loss or restore-time risk. Check both backup timers, missed executions, the latest full/differential sets from `sudo -u postgres pgbackrest --stanza=registry info`, and repository capacity. The full-backup variant requires a successful full set within eight days. Resolve by completing the required backup; do not silence the age metric.

## RuxServerPostgresBackupCheckStale

Impact: backup metadata and end-to-end WAL transport have not been tested in 26 hours. Inspect `rux-pgbackrest-check.timer` and its service journal, then run the check as `postgres`. The command forces a WAL switch, so repeated retries should remain bounded. Resolve when the check metric advances.

## RuxServerWalArchiveLagging

Impact: the five-minute RPO is at risk or already breached. Inspect `pg_stat_archiver`, `.ready` files under `pg_wal/archive_status`, pgBackRest logs, local disk, network, and repository availability. PostgreSQL deliberately keeps unarchived WAL; never delete it manually. Restore repository access or credentials before disk exhaustion, and consider stopping writes if capacity becomes critical. The archive-failures alert uses the same procedure. Resolve when no WAL remains queued beyond five minutes and no new failures occur.

## RuxServerRecoveryRehearsalOverdue

Impact: operational recovery has not been proven during the required quarter. Check both `target="postgresql"` and `target="spaces"` timestamps and locate the restricted evidence records. Follow the [production recovery runbook](recovery.md) on disposable resources. Record a timestamp only after reviewing successful evidence for that target; repeat failed or over-RTO drills after remediation.
