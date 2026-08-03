#!/usr/bin/env bash
set -euo pipefail

repository_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
fixture_root=$(mktemp -d)
trap 'rm -rf -- "$fixture_root"' EXIT

install -d "$fixture_root/bin" "$fixture_root/output"
printf '#!/usr/bin/env sh\nexit 0\n' > "$fixture_root/bin/rux-server"
printf '#!/usr/bin/env sh\nexit 0\n' > "$fixture_root/bin/sqlx"
chmod 0755 "$fixture_root/bin/rux-server" "$fixture_root/bin/sqlx"

export RUX_RELEASE_TAG=v0.1.0
export RUX_RELEASE_COMMIT=0123456789abcdef0123456789abcdef01234567
export RUX_RELEASE_VERSION=0.1.0
export RUX_RELEASE_API_BINARY="$fixture_root/bin/rux-server"
export RUX_RELEASE_SQLX_BINARY="$fixture_root/bin/sqlx"
export RUX_RELEASE_OUTPUT_DIRECTORY="$fixture_root/output"
export RUX_RELEASE_SOURCE_DATE_EPOCH=1785715200

cd "$repository_root"
archive=$(bash deploy/release/build-release.sh)
bash deploy/release/verify-release.sh "$archive" "$archive.sha256"

if bash deploy/release/build-release.sh >/dev/null 2>&1; then
  echo "builder replaced an existing immutable artifact" >&2
  exit 1
fi

printf 'tampered\n' >> "$archive"
if bash deploy/release/verify-release.sh "$archive" "$archive.sha256" >/dev/null 2>&1; then
  echo "verifier accepted a checksum mismatch" >&2
  exit 1
fi

echo "Verified release packaging success and failure paths"
