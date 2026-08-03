#!/usr/bin/env bash
set -euo pipefail

required=(
  RUX_RELEASE_TAG
  RUX_RELEASE_COMMIT
  RUX_RELEASE_VERSION
  RUX_RELEASE_API_BINARY
  RUX_RELEASE_SQLX_BINARY
  RUX_RELEASE_OUTPUT_DIRECTORY
  RUX_RELEASE_SOURCE_DATE_EPOCH
)
for name in "${required[@]}"; do
  if [[ -z "${!name:-}" ]]; then
    echo "$name is required" >&2
    exit 1
  fi
done

if [[ ! "$RUX_RELEASE_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
  echo "RUX_RELEASE_COMMIT must be a full lowercase Git commit SHA" >&2
  exit 1
fi
if [[ ! "$RUX_RELEASE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]]; then
  echo "RUX_RELEASE_VERSION must be a semantic version" >&2
  exit 1
fi
if [[ "$RUX_RELEASE_TAG" != "v$RUX_RELEASE_VERSION" ]]; then
  echo "RUX_RELEASE_TAG must equal v followed by RUX_RELEASE_VERSION" >&2
  exit 1
fi
if [[ ! "$RUX_RELEASE_SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]]; then
  echo "RUX_RELEASE_SOURCE_DATE_EPOCH must be a Unix timestamp" >&2
  exit 1
fi
if [[ ! -x "$RUX_RELEASE_API_BINARY" || ! -x "$RUX_RELEASE_SQLX_BINARY" ]]; then
  echo "release API and SQLx inputs must be executable files" >&2
  exit 1
fi
output_directory=$(realpath -m "$RUX_RELEASE_OUTPUT_DIRECTORY")
mkdir -p "$output_directory"
archive_name="rux-server-${RUX_RELEASE_TAG}-${RUX_RELEASE_COMMIT}.tar.gz"
archive_path="$output_directory/$archive_name"
checksum_path="$archive_path.sha256"
if [[ -e "$archive_path" || -e "$checksum_path" ]]; then
  echo "refusing to replace an existing release artifact" >&2
  exit 1
fi

staging_root=$(mktemp -d)
trap 'rm -rf -- "$staging_root"' EXIT
release_root="$staging_root/release"
install -d -m 0755 "$release_root/bin" "$release_root/migrations"
install -m 0755 "$RUX_RELEASE_API_BINARY" "$release_root/bin/rux-server"
install -m 0755 "$RUX_RELEASE_SQLX_BINARY" "$release_root/bin/sqlx"
cp -a migrations/. "$release_root/migrations/"
find "$release_root/migrations" -type d -exec chmod 0755 {} +
find "$release_root/migrations" -type f -exec chmod 0644 {} +

cat > "$release_root/release.json" <<EOF
{
  "schema_version": 1,
  "tag": "$RUX_RELEASE_TAG",
  "version": "$RUX_RELEASE_VERSION",
  "commit_sha": "$RUX_RELEASE_COMMIT",
  "target": "x86_64-unknown-linux-gnu",
  "source_date_epoch": $RUX_RELEASE_SOURCE_DATE_EPOCH
}
EOF
chmod 0644 "$release_root/release.json"

(
  cd "$release_root"
  find bin migrations -type f -print0 \
    | sort -z \
    | xargs -0 sha256sum > SHA256SUMS
)
chmod 0644 "$release_root/SHA256SUMS"

tar \
  --sort=name \
  --mtime="@$RUX_RELEASE_SOURCE_DATE_EPOCH" \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  --format=posix \
  -czf "$archive_path" \
  -C "$release_root" .
(
  cd "$output_directory"
  sha256sum "$archive_name" > "${archive_name}.sha256"
)

printf '%s\n' "$archive_path"
