#!/usr/bin/env bash
set -uo pipefail

metrics_directory=${RUX_BACKUP_METRICS_DIRECTORY:-/var/lib/rux-recovery/metrics}
pgbackrest_command=${RUX_PGBACKREST_COMMAND:-/usr/bin/pgbackrest}
operation=${1:-}
backup_type=${2:-}

case "$operation" in
  backup)
    if [[ "$backup_type" != "full" && "$backup_type" != "diff" ]]; then
      echo "backup type must be full or diff" >&2
      exit 2
    fi
    metric_kind="backup"
    metric_label="type=\"$backup_type\""
    metric_file="$metrics_directory/rux-postgres-backup-$backup_type.prom"
    command=("$pgbackrest_command" --stanza=registry --type="$backup_type" backup)
    ;;
  check)
    metric_kind="backup_check"
    metric_label="check=\"repository\""
    metric_file="$metrics_directory/rux-postgres-backup-check.prom"
    command=("$pgbackrest_command" --stanza=registry check)
    ;;
  *)
    echo "usage: run-pgbackrest.sh backup {full|diff} | check" >&2
    exit 2
    ;;
esac

mkdir -p "$metrics_directory"
umask 0027
started_at=$(date +%s)
last_success=0
if [[ -f "$metric_file" ]]; then
  last_success=$(awk -v name="rux_postgres_${metric_kind}_last_success_unixtime_seconds{" 'index($1, name) == 1 { print $2 }' "$metric_file")
  last_success=${last_success:-0}
fi

if "${command[@]}"; then
  result=1
  exit_code=0
  last_success=$(date +%s)
else
  exit_code=$?
  result=0
fi
finished_at=$(date +%s)
duration=$((finished_at - started_at))
temporary_file=$(mktemp "$metrics_directory/.rux-pgbackrest.XXXXXX")
trap 'rm -f "$temporary_file"' EXIT
{
  echo "# HELP rux_postgres_${metric_kind}_last_run_success Whether the latest operation completed successfully."
  echo "# TYPE rux_postgres_${metric_kind}_last_run_success gauge"
  echo "rux_postgres_${metric_kind}_last_run_success{$metric_label} $result"
  echo "# HELP rux_postgres_${metric_kind}_last_success_unixtime_seconds Unix time of the latest successful operation."
  echo "# TYPE rux_postgres_${metric_kind}_last_success_unixtime_seconds gauge"
  echo "rux_postgres_${metric_kind}_last_success_unixtime_seconds{$metric_label} $last_success"
  echo "# HELP rux_postgres_${metric_kind}_last_duration_seconds Duration of the latest operation."
  echo "# TYPE rux_postgres_${metric_kind}_last_duration_seconds gauge"
  echo "rux_postgres_${metric_kind}_last_duration_seconds{$metric_label} $duration"
} > "$temporary_file"
chmod 0640 "$temporary_file"
mv -f "$temporary_file" "$metric_file"
trap - EXIT

exit "$exit_code"
