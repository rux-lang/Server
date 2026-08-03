#!/usr/bin/env bash
set -euo pipefail

required_variables=(
  RUX_SPACES_SOURCE_ENDPOINT
  RUX_SPACES_SOURCE_BUCKET
  RUX_SPACES_SOURCE_REGION
  RUX_SPACES_SOURCE_ACCESS_KEY
  RUX_SPACES_SOURCE_SECRET_KEY
  RUX_SPACES_DRILL_ENDPOINT
  RUX_SPACES_DRILL_BUCKET
  RUX_SPACES_DRILL_REGION
  RUX_SPACES_DRILL_ACCESS_KEY
  RUX_SPACES_DRILL_SECRET_KEY
  RUX_SPACES_STORAGE_KEY
  RUX_SPACES_EXPECTED_SHA256
  RUX_SPACES_EXPECTED_SIZE
  RUX_RECOVERY_EVIDENCE_DIRECTORY
)
for variable_name in "${required_variables[@]}"; do
  if [[ -z "${!variable_name:-}" ]]; then
    echo "missing required environment variable: $variable_name" >&2
    exit 2
  fi
done
for command_name in aws jq sha256sum stat; do
  command -v "$command_name" >/dev/null || {
    echo "required command is unavailable: $command_name" >&2
    exit 2
  }
done
if [[ ! "$RUX_SPACES_EXPECTED_SHA256" =~ ^[0-9a-f]{64}$ || ! "$RUX_SPACES_EXPECTED_SIZE" =~ ^[0-9]+$ ]]; then
  echo "expected checksum or size is invalid" >&2
  exit 2
fi
if [[ "$RUX_SPACES_SOURCE_BUCKET" == "$RUX_SPACES_DRILL_BUCKET" ]]; then
  echo "the drill bucket must be different from the production package bucket" >&2
  exit 2
fi

source_aws() {
  AWS_ACCESS_KEY_ID="$RUX_SPACES_SOURCE_ACCESS_KEY" \
  AWS_SECRET_ACCESS_KEY="$RUX_SPACES_SOURCE_SECRET_KEY" \
  AWS_DEFAULT_REGION="$RUX_SPACES_SOURCE_REGION" \
    aws --endpoint-url "$RUX_SPACES_SOURCE_ENDPOINT" "$@"
}
drill_aws() {
  AWS_ACCESS_KEY_ID="$RUX_SPACES_DRILL_ACCESS_KEY" \
  AWS_SECRET_ACCESS_KEY="$RUX_SPACES_DRILL_SECRET_KEY" \
  AWS_DEFAULT_REGION="$RUX_SPACES_DRILL_REGION" \
    aws --endpoint-url "$RUX_SPACES_DRILL_ENDPOINT" "$@"
}
verify_artifact() {
  local artifact=$1
  local actual_sha256 actual_size
  actual_sha256=$(sha256sum "$artifact" | awk '{print $1}')
  actual_size=$(stat --format='%s' "$artifact")
  [[ "$actual_sha256" == "$RUX_SPACES_EXPECTED_SHA256" ]] || {
    echo "artifact checksum does not match PostgreSQL metadata" >&2
    return 1
  }
  [[ "$actual_size" == "$RUX_SPACES_EXPECTED_SIZE" ]] || {
    echo "artifact size does not match PostgreSQL metadata" >&2
    return 1
  }
}

started_at=$(date +%s)
temporary_directory=$(mktemp -d)
trap 'rm -rf -- "$temporary_directory"' EXIT
source_versions=$(source_aws s3api list-object-versions \
  --bucket "$RUX_SPACES_SOURCE_BUCKET" --prefix "$RUX_SPACES_STORAGE_KEY" --output json)
source_version_id=$(jq -r --arg key "$RUX_SPACES_STORAGE_KEY" \
  '[.Versions[]? | select(.Key == $key)][0].VersionId // empty' <<< "$source_versions")
if [[ -z "$source_version_id" ]]; then
  echo "no current version exists for the selected production object" >&2
  exit 1
fi

source_aws s3api get-object --bucket "$RUX_SPACES_SOURCE_BUCKET" \
  --key "$RUX_SPACES_STORAGE_KEY" --version-id "$source_version_id" \
  "$temporary_directory/source.ruxpkg" >/dev/null
verify_artifact "$temporary_directory/source.ruxpkg"

rehearsal_id=$(date -u +%Y%m%dT%H%M%SZ)
drill_key="rehearsals/$rehearsal_id/$RUX_SPACES_STORAGE_KEY"
put_result=$(drill_aws s3api put-object --bucket "$RUX_SPACES_DRILL_BUCKET" \
  --key "$drill_key" --body "$temporary_directory/source.ruxpkg" --output json)
drill_version_id=$(jq -r '.VersionId // empty' <<< "$put_result")
if [[ -z "$drill_version_id" ]]; then
  drill_versions=$(drill_aws s3api list-object-versions \
    --bucket "$RUX_SPACES_DRILL_BUCKET" --prefix "$drill_key" --output json)
  drill_version_id=$(jq -r --arg key "$drill_key" \
    '[.Versions[]? | select(.Key == $key and .IsLatest == true)][0].VersionId // empty' <<< "$drill_versions")
fi
[[ -n "$drill_version_id" ]] || {
  echo "the drill bucket did not return a version identifier; verify that versioning is enabled" >&2
  exit 1
}

drill_aws s3api delete-object --bucket "$RUX_SPACES_DRILL_BUCKET" --key "$drill_key" >/dev/null
drill_aws s3api get-object --bucket "$RUX_SPACES_DRILL_BUCKET" \
  --key "$drill_key" --version-id "$drill_version_id" \
  "$temporary_directory/deleted-version.ruxpkg" >/dev/null
verify_artifact "$temporary_directory/deleted-version.ruxpkg"
drill_aws s3api put-object --bucket "$RUX_SPACES_DRILL_BUCKET" \
  --key "$drill_key" --body "$temporary_directory/deleted-version.ruxpkg" >/dev/null
drill_aws s3api get-object --bucket "$RUX_SPACES_DRILL_BUCKET" \
  --key "$drill_key" "$temporary_directory/restored-current.ruxpkg" >/dev/null
verify_artifact "$temporary_directory/restored-current.ruxpkg"

finished_at=$(date +%s)
mkdir -p "$RUX_RECOVERY_EVIDENCE_DIRECTORY"
evidence_file="$RUX_RECOVERY_EVIDENCE_DIRECTORY/spaces-$rehearsal_id.json"
jq -n \
  --arg rehearsal_id "$rehearsal_id" \
  --arg storage_key "$RUX_SPACES_STORAGE_KEY" \
  --arg source_version_id "$source_version_id" \
  --arg drill_key "$drill_key" \
  --arg expected_sha256 "$RUX_SPACES_EXPECTED_SHA256" \
  --argjson expected_size "$RUX_SPACES_EXPECTED_SIZE" \
  --argjson started_at "$started_at" \
  --argjson finished_at "$finished_at" \
  '{schema_version: 1, target: "spaces", outcome: "success", rehearsal_id: $rehearsal_id, storage_key: $storage_key, source_version_id: $source_version_id, drill_key: $drill_key, expected_sha256: $expected_sha256, expected_size: $expected_size, started_at: $started_at, finished_at: $finished_at, duration_seconds: ($finished_at - $started_at)}' \
  > "$evidence_file"
chmod 0600 "$evidence_file"
echo "$evidence_file"
