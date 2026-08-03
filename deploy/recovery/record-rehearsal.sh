#!/usr/bin/env bash
set -euo pipefail

metrics_directory=${RUX_BACKUP_METRICS_DIRECTORY:-/var/lib/rux-recovery/metrics}
target=${1:-}
case "$target" in
  postgresql|spaces) ;;
  *)
    echo "usage: record-rehearsal.sh {postgresql|spaces}" >&2
    exit 2
    ;;
esac

mkdir -p "$metrics_directory"
umask 0027
metric_file="$metrics_directory/rux-recovery-rehearsal-$target.prom"
temporary_file=$(mktemp "$metrics_directory/.rux-recovery-rehearsal.XXXXXX")
trap 'rm -f "$temporary_file"' EXIT
{
  echo '# HELP rux_recovery_rehearsal_last_success_unixtime_seconds Unix time of the latest successful recovery rehearsal.'
  echo '# TYPE rux_recovery_rehearsal_last_success_unixtime_seconds gauge'
  echo "rux_recovery_rehearsal_last_success_unixtime_seconds{target=\"$target\"} $(date +%s)"
} > "$temporary_file"
chmod 0640 "$temporary_file"
mv -f "$temporary_file" "$metric_file"
trap - EXIT
