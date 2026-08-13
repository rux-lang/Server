#!/usr/bin/env bash
set -euo pipefail

script_directory=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
test_directory=$(mktemp -d)
trap 'rm -rf -- "$test_directory"' EXIT
mkdir -p "$test_directory/bin" "$test_directory/metrics" "$test_directory/postgres/pg_wal/archive_status"

cat > "$test_directory/bin/pgbackrest" <<'EOF'
#!/usr/bin/env bash
if [[ "${RUX_TEST_PGBACKREST_FAIL:-false}" == "true" ]]; then
  exit 1
fi
EOF
chmod +x "$test_directory/bin/pgbackrest"

RUX_BACKUP_METRICS_DIRECTORY="$test_directory/metrics" \
RUX_PGBACKREST_COMMAND="$test_directory/bin/pgbackrest" \
  bash "$script_directory/run-pgbackrest.sh" backup full
grep -F 'rux_postgres_backup_last_run_success{type="full"} 1' \
  "$test_directory/metrics/rux-postgres-backup-full.prom"
successful_timestamp=$(awk '/last_success_unixtime_seconds/ && $1 !~ /^#/ {print $2}' \
  "$test_directory/metrics/rux-postgres-backup-full.prom")

if RUX_TEST_PGBACKREST_FAIL=true \
  RUX_BACKUP_METRICS_DIRECTORY="$test_directory/metrics" \
  RUX_PGBACKREST_COMMAND="$test_directory/bin/pgbackrest" \
  bash "$script_directory/run-pgbackrest.sh" backup full; then
  echo "a failed pgBackRest command unexpectedly succeeded" >&2
  exit 1
fi
grep -F 'rux_postgres_backup_last_run_success{type="full"} 0' \
  "$test_directory/metrics/rux-postgres-backup-full.prom"
grep -F "rux_postgres_backup_last_success_unixtime_seconds{type=\"full\"} $successful_timestamp" \
  "$test_directory/metrics/rux-postgres-backup-full.prom"

cat > "$test_directory/bin/psql" <<'EOF'
#!/usr/bin/env bash
echo '2|123'
EOF
chmod +x "$test_directory/bin/psql"
touch --date='2 minutes ago' "$test_directory/postgres/pg_wal/archive_status/000000010000000000000001.ready"
RUX_BACKUP_METRICS_DIRECTORY="$test_directory/metrics" \
RUX_POSTGRES_DATA_DIRECTORY="$test_directory/postgres" \
RUX_PSQL_COMMAND="$test_directory/bin/psql" \
  bash "$script_directory/collect-postgres-backup-metrics.sh"
grep -F 'rux_postgres_wal_archive_failures_total 2' "$test_directory/metrics/rux-postgres-wal.prom"
grep -E 'rux_postgres_wal_archive_oldest_pending_seconds [1-9][0-9]*' "$test_directory/metrics/rux-postgres-wal.prom"

RUX_BACKUP_METRICS_DIRECTORY="$test_directory/metrics" \
  bash "$script_directory/record-rehearsal.sh" postgresql
grep -F 'target="postgresql"' "$test_directory/metrics/rux-recovery-rehearsal-postgresql.prom"
if RUX_BACKUP_METRICS_DIRECTORY="$test_directory/metrics" \
  bash "$script_directory/record-rehearsal.sh" invalid >/dev/null 2>&1; then
  echo "an invalid rehearsal target unexpectedly succeeded" >&2
  exit 1
fi

printf 'recovery-artifact' > "$test_directory/artifact.ruxpkg"
artifact_sha256=$(sha256sum "$test_directory/artifact.ruxpkg" | awk '{print $1}')
artifact_size=$(stat --format='%s' "$test_directory/artifact.ruxpkg")
cat > "$test_directory/bin/aws" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == "--endpoint-url" ]]; then
  shift 2
fi
[[ "$1" == "s3api" ]]
operation=$2
case "$operation" in
  list-object-versions)
    printf '{"Versions":[{"Key":"packages/test/1.0.0/artifact.ruxpkg","IsLatest":true,"VersionId":"source-v1"}]}'
    ;;
  get-object)
    cp "$RUX_TEST_ARTIFACT" "${@: -1}"
    printf '{}'
    ;;
  put-object)
    printf '{"VersionId":"drill-v1"}'
    ;;
  delete-object)
    printf '{"DeleteMarker":true,"VersionId":"delete-v1"}'
    ;;
  *)
    echo "unexpected fake aws operation: $operation" >&2
    exit 1
    ;;
esac
EOF
chmod +x "$test_directory/bin/aws"

PATH="$test_directory/bin:$PATH" \
RUX_TEST_ARTIFACT="$test_directory/artifact.ruxpkg" \
RUX_SPACES_SOURCE_ENDPOINT=https://source.example.test \
RUX_SPACES_SOURCE_BUCKET=production-packages \
RUX_SPACES_SOURCE_REGION=fra1 \
RUX_SPACES_SOURCE_ACCESS_KEY=source-access \
RUX_SPACES_SOURCE_SECRET_KEY=source-secret \
RUX_SPACES_DRILL_ENDPOINT=https://drill.example.test \
RUX_SPACES_DRILL_BUCKET=recovery-drill \
RUX_SPACES_DRILL_REGION=nyc3 \
RUX_SPACES_DRILL_ACCESS_KEY=drill-access \
RUX_SPACES_DRILL_SECRET_KEY=drill-secret \
RUX_SPACES_STORAGE_KEY=packages/test/1.0.0/artifact.ruxpkg \
RUX_SPACES_EXPECTED_SHA256="$artifact_sha256" \
RUX_SPACES_EXPECTED_SIZE="$artifact_size" \
RUX_RECOVERY_EVIDENCE_DIRECTORY="$test_directory/evidence" \
  bash "$script_directory/rehearse-spaces.sh" > "$test_directory/evidence-path"
evidence_file=$(<"$test_directory/evidence-path")
jq -e '.target == "spaces" and .outcome == "success" and .source_version_id == "source-v1"' \
  "$evidence_file" >/dev/null

echo "Recovery script tests passed"
