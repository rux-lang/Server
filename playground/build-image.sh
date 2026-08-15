#!/usr/bin/env bash
# Builds the playground sandbox image for one pinned Rux release.
#
#   build-image.sh <version> <sha256> [packages]
#
# The checksum is mandatory: the image downloads a release tarball over the
# network at build time, and an unverified download is the one step of this
# whole design that a network attacker could otherwise turn into arbitrary code
# execution inside every subsequent run.

set -euo pipefail

usage() {
    cat >&2 <<'USAGE'
usage: build-image.sh <version> <sha256> [packages]

  version   Rux release to install, without the leading "v" (e.g. 0.3.0)
  sha256    SHA-256 of that release's rux-linux.tar.gz, lowercase hex
  packages  Optional space-separated standard packages to seed, e.g. "Io Math"

The image runs with --network=none, so a package that is not seeded here can
never resolve at request time; adding one means rebuilding the image.
USAGE
    exit 64
}

[[ $# -ge 2 && $# -le 3 ]] || usage

version="$1"
checksum="$2"
packages="${3:-}"

[[ -n "$version" ]] || usage
[[ "$version" != v* ]] || {
    echo "build-image.sh: give the version without a leading 'v' (got '$version')" >&2
    exit 64
}
[[ "$checksum" =~ ^[0-9a-f]{64}$ ]] || {
    echo 'build-image.sh: sha256 must be 64 lowercase hex characters' >&2
    exit 64
}

directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tag="rux-playground:${version}"

# --network=host because the build is the one step that needs the network, and
# the production host denies it to containers on purpose: the firewall's forward
# chain drops container egress, and the daemon is configured with the default
# bridge off entirely (see docs/deployment.md). Host networking is the only route
# left there, and it is harmless elsewhere.
echo "Building ${tag} from Rux ${version}"
docker build \
    --network=host \
    --tag "$tag" \
    --build-arg "RUX_VERSION=${version}" \
    --build-arg "RUX_SHA256=${checksum}" \
    --build-arg "RUX_PACKAGES=${packages}" \
    "$directory"

echo "Built ${tag}"
