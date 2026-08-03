#!/usr/bin/env bash
set -euo pipefail

metrics_directory=${RUX_BACKUP_METRICS_DIRECTORY:-/var/lib/rux-recovery/metrics}
postgres_data_directory=${RUX_POSTGRES_DATA_DIRECTORY:-/var/lib/postgresql/18/main}
psql_command=${RUX_PSQL_COMMAND:-/usr/bin/psql}

archive_values=$(
  "$psql_command" --dbname=postgres --no-align --tuples-only --field-separator='|' \
    --command="SELECT failed_count, COALESCE(EXTRACT(EPOCH FROM last_archived_time)::bigint, 0) FROM pg_stat_archiver"
)
IFS='|' read -r failed_count last_archived_at <<< "$archive_values"

oldest_ready_at=0
while IFS= read -r ready_at; do
  if [[ "$oldest_ready_at" -eq 0 || "$ready_at" -lt "$oldest_ready_at" ]]; then
    oldest_ready_at=$ready_at
  fi
done < <(find "$postgres_data_directory/pg_wal/archive_status" -maxdepth 1 -type f -name '*.ready' -printf '%T@\n' 2>/dev/null | cut -d. -f1)

oldest_pending_seconds=0
if [[ "$oldest_ready_at" -gt 0 ]]; then
  now=$(date +%s)
  oldest_pending_seconds=$((now - oldest_ready_at))
fi

mkdir -p "$metrics_directory"
umask 0027
temporary_file=$(mktemp "$metrics_directory/.rux-postgres-wal.XXXXXX")
trap 'rm -f "$temporary_file"' EXIT
{
  echo '# HELP rux_postgres_wal_archive_failures_total Number of failed PostgreSQL archive attempts.'
  echo '# TYPE rux_postgres_wal_archive_failures_total counter'
  echo "rux_postgres_wal_archive_failures_total $failed_count"
  echo '# HELP rux_postgres_wal_archive_last_success_unixtime_seconds Unix time of the latest archived WAL segment.'
  echo '# TYPE rux_postgres_wal_archive_last_success_unixtime_seconds gauge'
  echo "rux_postgres_wal_archive_last_success_unixtime_seconds $last_archived_at"
  echo '# HELP rux_postgres_wal_archive_oldest_pending_seconds Age of the oldest WAL segment waiting to be archived.'
  echo '# TYPE rux_postgres_wal_archive_oldest_pending_seconds gauge'
  echo "rux_postgres_wal_archive_oldest_pending_seconds $oldest_pending_seconds"
} > "$temporary_file"
chmod 0640 "$temporary_file"
mv -f "$temporary_file" "$metrics_directory/rux-postgres-wal.prom"
trap - EXIT
