#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: verify-release.sh ARCHIVE SHA256_FILE" >&2
  exit 1
fi

archive=$(realpath "$1")
checksum=$(realpath "$2")
if [[ "$(dirname "$archive")" != "$(dirname "$checksum")" ]]; then
  echo "archive and checksum must be in the same directory" >&2
  exit 1
fi

(
  cd "$(dirname "$archive")"
  sha256sum --check "$(basename "$checksum")"
)

staging_root=$(mktemp -d)
trap 'rm -rf -- "$staging_root"' EXIT
tar -xzf "$archive" -C "$staging_root"

for path in release.json SHA256SUMS bin/rux-server bin/rux-playgroundd bin/sqlx; do
  if [[ ! -f "$staging_root/$path" ]]; then
    echo "release archive is missing $path" >&2
    exit 1
  fi
done
if find "$staging_root" -type l -print -quit | grep -q .; then
  echo "release archive must not contain symbolic links" >&2
  exit 1
fi
if [[ ! -x "$staging_root/bin/rux-server" || ! -x "$staging_root/bin/rux-playgroundd" || ! -x "$staging_root/bin/sqlx" ]]; then
  echo "release executables have unsafe modes" >&2
  exit 1
fi
(
  cd "$staging_root"
  sha256sum --check SHA256SUMS
)

node -e '
const fs = require("node:fs")
const value = JSON.parse(fs.readFileSync(process.argv[1], "utf8"))
if (value.schema_version !== 1) throw new Error("unsupported release manifest schema")
if (!/^v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(value.tag)) throw new Error("invalid release tag")
if (value.tag !== `v${value.version}`) throw new Error("release tag/version mismatch")
if (!/^[0-9a-f]{40}$/.test(value.commit_sha)) throw new Error("invalid release commit")
if (value.target !== "x86_64-unknown-linux-gnu") throw new Error("invalid release target")
if (!Number.isSafeInteger(value.source_date_epoch) || value.source_date_epoch < 1) throw new Error("invalid source date")
' "$staging_root/release.json"

echo "Verified release archive $(basename "$archive")"
