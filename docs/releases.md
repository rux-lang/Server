# Production releases

API releases are immutable, checksum-bound archives produced from a `v`-prefixed SemVer tag contained in `main`. The tag must match the root `rux-server` package version.

Each archive contains the API and pinned SQLx executables under `bin/`, reviewed SQL migrations, `SHA256SUMS`, and schema-1 `release.json`. Frontend files are released independently from the `rux-lang/Web` repository and are never included in an API archive.

The release workflow runs the full Rust suite, builds `x86_64-unknown-linux-gnu` artifacts, produces an SPDX SBOM, and creates provenance attestations before publishing immutable draft assets.

## Deploy

Provision the host with `deploy/ansible/site.yml`, verify the downloaded archive and checksum with `deploy/release/verify-release.sh`, review pending migrations, and run `deploy/ansible/release.yml` with `rux_migrations_approved=true`. The playbook applies migrations deliberately, promotes the commit-addressed release symlink, restarts the service, and verifies readiness and the public OpenAPI route.

## Rollback

Production migrations are never reverted. Rollback selects an already installed, schema-compatible application release with `deploy/ansible/rollback.yml` and `rux_confirm_schema_compatible=true`. If verification fails, the playbook restores the previously active release.
