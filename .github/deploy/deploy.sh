#!/usr/bin/env bash
# Installs one released build of the Rux server on the production droplet.
#
#   deploy.sh <version> <tarball> [--dry-run]
#
# The release workflow pipes this to `bash -s` over SSH, so the host has nothing
# pre-installed that could drift from the workflow that calls it. It is also
# shipped inside the tarball, so the same sequence can be run by hand from an
# unpacked release when GitHub is unavailable.
#
# Configuration is never deployed. /etc/rux/config.toml is root-owned, lives
# only on this host, and is the source of the migration credential read below -
# so the production database URL never leaves the droplet.
#
# -E as well as -e: without it the ERR trap does not fire inside functions or
# subshells, and the rollback would silently never run.

set -eEuo pipefail

usage() {
    cat >&2 <<'USAGE'
usage: deploy.sh <version> <tarball> [--dry-run]

  version    The release being installed, without the leading "v" (e.g. 0.1.1)
  tarball    Path to the release tarball already copied onto this host
  --dry-run  Verify the checksum, the staged binaries, the configuration read,
             and the pending migrations, then stop without touching the service

Applies pending migrations and swaps the binaries. Migrations are never
reverted; see docs/migrations.md.
USAGE
    exit 64
}

[[ $# -ge 2 && $# -le 3 ]] || usage

version="$1"
tarball="$2"
dry_run="no"

if [[ $# -eq 3 ]]; then
    [[ "$3" == "--dry-run" ]] || usage
    dry_run="yes"
fi

[[ -n "$version" ]] || usage
[[ -f "$tarball" ]] || {
    echo "deploy.sh: no such tarball: $tarball" >&2
    exit 66
}

readonly install_root="/srv/rux-server/bin"
readonly config_file="/etc/rux/config.toml"
readonly backup_root="/var/backups/rux"
readonly health="http://127.0.0.1:8080"
readonly binaries=(rux-server rux-playgroundd)

# Tracks how far the run got, so a pre-flight failure never stops and restarts a
# healthy service for nothing.
phase="preflight"
dump=""

say() { printf '==> %s\n' "$*"; }

journal() {
    echo "--- journalctl -u rux-server (last 60 lines) ---" >&2
    sudo journalctl -u rux-server -n 60 --no-pager --output=cat >&2 || true
}

rollback() {
    set +e
    echo >&2

    if [[ "$phase" == "preflight" ]]; then
        echo "deploy.sh: FAILED during pre-flight - production was not touched" >&2
        return
    fi

    echo "deploy.sh: FAILED after ${phase} - restoring the previous binaries" >&2

    sudo systemctl stop rux-server
    for binary in "${binaries[@]}"; do
        if [[ -e "${install_root}/${binary}.previous" ]]; then
            sudo cp -a "${install_root}/${binary}.previous" "${install_root}/${binary}.restore"
            sudo mv -f "${install_root}/${binary}.restore" "${install_root}/${binary}"
        fi
    done
    sudo systemctl start rux-server

    sleep 3
    if curl --fail --silent --max-time 5 "${health}/health/ready" > /dev/null; then
        echo "deploy.sh: the previous release is serving again" >&2
    else
        echo "deploy.sh: THE PREVIOUS RELEASE DID NOT COME BACK - THIS HOST NEEDS AN OPERATOR NOW" >&2
    fi

    journal

    if [[ "$phase" == "migrated" || "$phase" == "swapped" || "$phase" == "started" ]]; then
        cat >&2 <<WARNING

  ****************************************************************
  MIGRATIONS WERE APPLIED AND ARE NOT REVERTED.
  The restored binary is running against ${version}'s schema. That is
  only safe if the previous release tolerates it - which is what the
  expand-and-contract rule in docs/migrations.md exists to guarantee.
  Decide now, by hand. If it does not, roll forward with a new
  migration rather than reverting.
  Pre-migration dump: ${dump:-none taken}
  ****************************************************************
WARNING
    fi
}

trap rollback ERR

# One deploy at a time, independently of the workflow's concurrency group, so a
# manual run cannot race the pipeline.
exec 9> /var/lock/rux-deploy.lock
flock --nonblock 9 || {
    echo "deploy.sh: another deploy holds /var/lock/rux-deploy.lock" >&2
    exit 75
}

# ---------------------------------------------------------------------------
# Pre-flight - nothing here changes production
# ---------------------------------------------------------------------------

say "Verifying the checksum"
# The sidecar records a bare filename, so check it from its own directory.
(cd "$(dirname "$tarball")" && sha256sum --check --strict "$(basename "$tarball").sha256")

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
tar -xzf "$tarball" -C "$stage" --strip-components=1

# The outer sidecar covers the tarball; this covers what came out of it, which
# is what the by-hand path in docs/deployment.md verifies too.
(cd "$stage" && sha256sum --check --strict --quiet SHA256SUMS)

for binary in "${binaries[@]}"; do
    [[ -x "$stage/bin/$binary" ]] || {
        echo "deploy.sh: $binary missing from the tarball" >&2
        exit 65
    }
done
[[ -x "$stage/bin/sqlx" ]] || {
    echo "deploy.sh: the pinned SQLx CLI is missing from the tarball" >&2
    exit 65
}

shipped="$(cat "$stage/VERSION")"
[[ "$shipped" == "$version" ]] || {
    echo "deploy.sh: tarball declares ${shipped} but ${version} was requested" >&2
    exit 65
}

# Neither binary takes --version, so prove the staged one merely *loads* on this
# host's glibc: a wrong glibc dies in the loader, while a missing configuration
# file dies in our own parser with a message that names the config.
say "Checking the staged binary runs here"
loads="$("$stage/bin/rux-server" --config /nonexistent/rux.toml 2>&1 || true)"
grep -qi 'config' <<<"$loads" || {
    echo "deploy.sh: the staged binary does not run on this host:" >&2
    printf '%s\n' "$loads" >&2
    exit 65
}

say "Reading the migration credential from $config_file"
# A real TOML parse rather than a regex, because every value in that file is
# either a credential or next to one. The connection settings become PG*
# environment variables instead of pg_dump arguments, so the password never
# appears in this host's process list.
while IFS='=' read -r key value; do
    [[ -n "$key" ]] || continue
    export "$key=$value"
done < <(sudo python3 - "$config_file" <<'PYTHON'
import sys
import tomllib
import urllib.parse

with open(sys.argv[1], "rb") as handle:
    url = tomllib.load(handle)["database"]["url"]

parts = urllib.parse.urlparse(url)
unquote = urllib.parse.unquote

print("DATABASE_URL=%s" % url)
print("PGHOST=%s" % (parts.hostname or "localhost"))
print("PGPORT=%s" % (parts.port or 5432))
print("PGUSER=%s" % unquote(parts.username or ""))
print("PGPASSWORD=%s" % unquote(parts.password or ""))
print("PGDATABASE=%s" % (parts.path.lstrip("/") or "registry"))
PYTHON
)

# A failing process substitution leaves the loop with nothing to read rather
# than failing the script, so check the result instead of trusting set -e.
[[ -n "${DATABASE_URL:-}" ]] || {
    echo "deploy.sh: could not read database.url from $config_file" >&2
    exit 78
}

say "Pending migrations"
"$stage/bin/sqlx" migrate info --source "$stage/migrations"

if [[ "$dry_run" == "yes" ]]; then
    say "Dry run: stopping here. Nothing was stopped, migrated, or swapped."
    trap - ERR
    exit 0
fi

# ---------------------------------------------------------------------------
# From here on production changes
# ---------------------------------------------------------------------------

# Snapshot what is installed *now*, before anything moves. Taking it during the
# swap instead would make .previous mean "whatever the last deploy displaced",
# so a failed migration would roll back to a stale binary rather than the one
# that was serving a moment ago.
phase="snapshot"
say "Snapshotting the installed binaries"
for binary in "${binaries[@]}"; do
    if [[ -e "${install_root}/${binary}" ]]; then
        sudo cp -a "${install_root}/${binary}" "${install_root}/${binary}.previous"
    fi
done

phase="stopped"
say "Stopping the service"
sudo systemctl stop rux-server

phase="backed-up"
say "Backing up the database"
# Owned by the deploying account, because pg_dump runs as that account over
# loopback with the registry role rather than as the postgres system user.
sudo install -d -m 0700 -o "$(id -un)" -g "$(id -gn)" "$backup_root"
dump="${backup_root}/registry-$(date --utc +%Y%m%dT%H%M%SZ)-pre-${version}.dump"
pg_dump --format=custom --no-owner --no-privileges --file="$dump"
[[ -s "$dump" ]]
sha256sum "$dump" > "${dump}.sha256"
say "Wrote ${dump} ($(du -h "$dump" | cut -f1))"

# Migrations are applied before the binaries are installed rather than after.
# Both orders satisfy "migrate before starting the new binaries", but this one
# means a failed migration leaves the outgoing binary in place, so recovery is
# a plain restart. See docs/deployment.md.
phase="migrated"
say "Applying migrations"
"$stage/bin/sqlx" migrate run --source "$stage/migrations"
"$stage/bin/sqlx" migrate info --source "$stage/migrations"

phase="swapped"
say "Installing the binaries"
for binary in "${binaries[@]}"; do
    # Staged then renamed: a rename within one directory is atomic, so a dropped
    # connection can never leave a half-written executable that the unit's
    # ConditionFileIsExecutable would happily accept.
    sudo install -o root -g root -m 0755 "$stage/bin/$binary" "${install_root}/${binary}.incoming"
    sudo mv -f "${install_root}/${binary}.incoming" "${install_root}/${binary}"
done

phase="started"
say "Starting the service"
sudo systemctl start rux-server

# ---------------------------------------------------------------------------
# Verify
# ---------------------------------------------------------------------------

# Caddy answers 404 for /health and /health/* publicly, so this is only
# reachable from the host itself. TimeoutStartSec is 30s, so poll rather than
# assume the first request wins.
say "Waiting for liveness"
live=""
for _ in $(seq 1 45); do
    if live="$(curl --fail --silent --max-time 5 "${health}/health/live")"; then
        break
    fi
    live=""
    sleep 2
done

[[ -n "$live" ]] || {
    echo "deploy.sh: the API never answered /health/live" >&2
    false
}

# The point of the version field: prove the process answering is the one just
# installed, rather than a restart that failed and left the previous binary
# serving.
grep -q "\"version\":\"${version}\"" <<<"$live" || {
    echo "deploy.sh: /health/live does not report ${version}: ${live}" >&2
    false
}
say "Liveness reports ${version}"

say "Waiting for readiness"
ready=""
for _ in $(seq 1 45); do
    if ready="$(curl --fail --silent --max-time 5 "${health}/health/ready")"; then
        break
    fi
    ready=""
    sleep 3
done

[[ -n "$ready" ]] || {
    echo "deploy.sh: readiness never returned 200" >&2
    false
}

for probe in '"status":"healthy"' '"postgresql"' '"object_storage"'; do
    grep -q "$probe" <<<"$ready" || {
        echo "deploy.sh: readiness is missing ${probe}: ${ready}" >&2
        false
    }
done
say "PostgreSQL and object storage are both healthy"

# The playground is disabled in production, so its unit is normally absent and
# the new broker binary just sits there unused. Keying off the unit rather than
# a constant means turning the playground on later needs no change here.
if sudo systemctl is-enabled --quiet rux-playground 2> /dev/null; then
    say "The playground is enabled on this host; restarting the broker"
    sudo systemctl restart rux-playground
    sleep 3
    sudo systemctl is-active --quiet rux-playground || {
        echo "deploy.sh: rux-playground did not come back" >&2
        false
    }
fi

phase="done"
trap - ERR

journal

# Keep the last ten dumps. Releases themselves are retained by GitHub, so only
# what this script writes needs pruning.
find "$backup_root" -maxdepth 1 -name 'registry-*.dump*' -printf '%T@ %p\n' \
    | sort -rn | tail -n +21 | cut -d' ' -f2- | xargs -r rm -f

say "Deployed v${version}"
