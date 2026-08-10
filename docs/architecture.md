# Architecture

## Goals and boundaries

The registry publishes, indexes, searches, and distributes public Rux packages. It provides a machine-facing API for the Rux CLI and a human-facing package catalog. Version 1 does not include private packages, billing, federation, mirroring, multi-region operation, or registry-side documentation builds.

The initial deployment must remain inexpensive and understandable on one DigitalOcean droplet. PostgreSQL is the source of truth for identities and metadata. DigitalOcean Spaces is the source of truth for immutable package bytes. The relational entities, lifecycle rules, and constraints are documented in the [database schema](database.md).

## Components

```text
                                      DigitalOcean droplet
                           ┌────────────────────────────────────┐
browser ── HTTPS ─────────► rux-lang.dev/packages (Web repository)
                                      │ JSON over HTTPS
                                      ▼
rux CLI ── HTTPS ─────────┐      ┌──────────────────────────────┐
                          ├─────►│ api.rux-lang.dev             │     unix      ┌─────────────────┐
Nuxt site ── HTTPS ───────┘      │ Caddy ──► Rust/Axum API      │───socket─────►│ rux-playgroundd │
                                 └──────────┬───────────┬───────┘               └────────┬────────┘
                                            │           │                   docker run   │
                                            ▼           ▼                                ▼
                                     PostgreSQL 18  DigitalOcean                 sandbox container
                                                    Spaces + CDN
```

- **Caddy** terminates TLS for `api.rux-lang.dev`, proxies the Rust process over loopback HTTP, and does not serve frontend files. Operational health routes are not publicly exposed.
- **Web app** is owned by the separate `rux-lang/Web` repository and serves the registry beneath `https://rux-lang.dev/packages`.
- **API app** is the root Tokio and Axum package. It owns HTTP contracts, authentication, authorization, registry use cases, dependency readiness, and access to persistence and object storage. Production runs it as an unprivileged systemd service.
- **Playground broker** (`rux-playgroundd`) runs submitted code in a throwaway container. It is a second binary under a second service user, and the only process on the host permitted to reach the Docker socket. That split is a trust boundary rather than a packaging choice: socket access is root-equivalent, and the registry and its database share this droplet, so the API talks to the broker over a unix socket and never to a container runtime. It is optional and disabled by default. See [playground](playground.md).
- **PostgreSQL 18** is the authoritative store for accounts, sessions, ownership, package metadata, tokens, audits, and download records. Only the API connects to it.
- **DigitalOcean Spaces** stores immutable package artifacts through its S3-compatible API. The API publishes and manages objects; package downloads are redirected to the public Spaces CDN instead of being proxied through the droplet.

The API exposes bounded structured `/v1/sitemap` data; the separate Web repository owns route mapping, static generation, and frontend hosting policy.

## Rust boundaries

- `domain` contains registry value types, invariants, and state transitions. It has no I/O or framework dependencies.
- `manifest` parses the versioned `Rux.toml` contract and converts it into validated values while depending only on `domain` and format-validation libraries.
- `artifact` validates the ZIP-based [`.ruxpkg` contract](artifact.md), embedded publication manifest, portable paths, bounded contents, and source metrics while depending on `manifest`.
- `application` contains use cases and declares ports for persistence, object storage, clocks, credentials, and identity.
- `infrastructure` implements those ports with SQLx, PostgreSQL, the S3 API, and external identity providers.
- `sandbox` owns the playground's container contract: the resource envelope, the `docker` argument vector, the per-job directory, the nonce framing, and the socket protocol the API and the broker speak. It depends only on `domain` and small utility crates.
- `server` is the composition root. It owns HTTP mapping, authentication middleware, CORS, rate limits, and the versioned [HTTP API contract](http-api.md), including RFC 9457 responses. Additional server-side surfaces are composed here rather than in separate binaries.

Dependencies point inward. Handlers never issue SQL or S3 operations directly, and infrastructure cannot import API types.

The root package produces two binaries: `rux-server`, and `rux-playgroundd` for the playground broker. The broker is a separate binary only because it must run as a different user with different privileges; it is not a separate service in the architectural sense, and both ship in the same release artifact so a deploy moves them together. `sandbox` spawns processes and writes files, so it is infrastructure in everything but name: `application` deliberately does not depend on it, and restates the playground's types as its own so the layer keeps depending on nothing but `domain`.

## Frontend and authentication

GitHub OAuth uses the authorization-code flow through the API. A short-lived host-only cookie binds the authorization state to the initiating browser; callback redirects always target the separately configured `RUX_WEB_CALLBACK_URL` at `/packages/-/auth/callback` and never accept a caller-selected destination. The API keeps the client secret and temporary provider token in memory, atomically upserts the user and session, stores only session and CSRF hashes in PostgreSQL, and issues host-only `Secure`, `HttpOnly`, `SameSite=Lax` session and CSRF cookies. The exact-origin session endpoint returns the CSRF value to the browser in a non-cacheable response. State-changing cookie-authenticated requests require that value in a custom header, and browser sessions rotate every seven days without extending their absolute 30-day expiry. Credentialed CORS admits only `RUX_ALLOWED_WEB_ORIGIN`.

The CLI uses separately issued `rux_pat_` bearer credentials backed by 32 bytes of operating-system randomness. A credential is returned only by its creation response; PostgreSQL receives its SHA-256 hash and a safe `rux_pat_` display prefix. Tokens may have an optional absolute expiry and carry only the `publish`, `yank`, and `namespace` scopes. Authorization locks the token and user, rejects expired, revoked, or anonymized principals, enforces the operation's required scope, and updates last use inside the caller's mutation transaction. Browser token management uses the session and CSRF contract and exposes token history without hashes or credentials.

The authenticated settings route reads its GitHub profile and session expiry from the existing session contract. Account deletion is an exact-origin, CSRF-protected browser operation with a case-sensitive login confirmation. Its transaction locks the user followed by membership namespaces, refuses to orphan a final-owner namespace, removes revocable access, scrubs identity data, and retains only an anonymous internal row for durable package and audit references. Re-registering through GitHub creates a new account rather than reactivating that row.

Namespace claims and management accept either the protected browser session or a bearer token with the `namespace` scope. Claims atomically create the namespace and first owner. Owners manage role changes and invitations to existing registry users; maintainers may leave but cannot administer membership. Namespace-row locking serializes all membership and invitation mutations and prevents concurrent changes from removing the last owner.

## Package flow

Publication streams a bounded multipart upload to temporary storage while computing SHA-256. The application validates `Rux.toml` and the [`.ruxpkg` artifact](artifact.md), including its exact embedded manifest, portable ZIP layout, bounded expansion, UTF-8 sources, referenced text, and source metrics, before checking namespace ownership and version uniqueness. Package bytes use collision-proof immutable object keys. If object upload succeeds but the database transaction fails, a delayed orphan sweep removes the unreferenced object.

The publication receiver accepts exactly one case-sensitive `manifest` part and one `package` part in either order. It retains at most the manifest's 65,536 bytes in memory and streams package chunks directly to an exclusively created temporary file while computing the artifact SHA-256. The package is limited to 5 MiB, the complete multipart request including framing is limited to 6 MiB, and active upload operations and retained results share a process-wide temporary-byte quota. The receiver defaults to 100 MiB below the operating system temporary directory, allows the path and quota to be injected when the publication route is composed, never uses caller filenames as paths, and releases both the file and its reservation on every drop or failure.
Inspection consumes a staged upload and reopens its temporary artifact on a Tokio blocking worker. The ZIP reader works directly against that seekable file: it retains only the bounded manifest, the current bounded entry when content validation requires it, and bounded referenced text returned as publication metadata. A successful inspection returns the typed manifest, artifact and source metrics, optional README and license text, and the original staging guard with its byte size and SHA-256. Invalid artifacts and operational failures drop that guard and release its temporary-byte reservation. If an asynchronous caller is cancelled after blocking inspection begins, the detached bounded inspection finishes before releasing the guard.
Publication authorization and metadata persistence use a prepared application transaction. It authorizes a bearer token with `publish` scope, locks the namespace and membership, applies the operator-managed namespace and global package blocklist, and reserves the normalized package plus exact version identity. Owners and maintainers may publish. The transaction remains open across the later bounded object upload; successful completion inserts the immutable version aggregate, token last-use update, and audit record in one commit, while an upload failure explicitly aborts it. `POST /v1/packages` composes this transaction with the staged upload and Spaces adapter. Objects use `packages/{normalized namespace}/{normalized package}/{exact version}/{SHA-256}.ruxpkg` keys, so a repeated key can contain only identical bytes. The adapter streams the staged file with an exact content length and provider-verified SHA-256, accepts an omitted checksum echo, and rejects a mismatched echo. Database failures after a successful upload intentionally leave a recognizable orphan for the delayed orphan sweep.

The API runs one bounded orphan sweep on startup and hourly thereafter. Each sweep lists at most 1,000 provider object versions below `packages/`, carries the provider's key and version markers forward in memory, and attempts at most 100 deletions. Only keys that exactly match the immutable package-key grammar, have complete version metadata, are at least 24 hours old, and have no matching `package_versions.storage_key` reference are eligible. A referenced key protects all of its provider versions. Delete markers and unrecognized keys are never removed. Deletions always include the exact provider version ID, so a concurrent publication of the same content creates a distinct version that the sweep cannot remove. Listing and database failures fail closed; individual deletion failures are logged and retried after the bounded scan wraps.

Downloads never proxy package bytes through the API. A canonical exact-version route commits one append-only event before returning an uncacheable temporary redirect to the configured Spaces CDN URL. HEAD resolves without counting, and known yanked versions remain downloadable because yanking affects selection, not stored bytes. A public package-level statistics read groups the same append-only events across all versions into thirty zero-filled UTC days ending at request time, whose final day is partial, and returns only aggregates. Resolver indexes read current PostgreSQL state without a process cache, use deterministic JSON with yanked-version flags, and provide strong ETags for revalidation. Search selects one stable-first representative version per package, combines literal PostgreSQL full-text and `pg_trgm` identity matching, and uses request-bound keyset cursors; no separate search service is required initially. Discovery reuses that representative rule for reverse dependencies and keywords, exposes cursor-paged version and sitemap source collections, and serves fixed recent and 30-day-download highlight groups. All-yanked packages remain discoverable but are not promoted in highlights. Sitemap data stays structured so frontend route design remains owned by the static catalog.

Yank state is set through an idempotent exact-version mutation authenticated by a bearer token with the `yank` scope. Owners and maintainers may perform it. Token authorization, membership validation, the conditional state update, and an audit event for a real transition share one transaction. Repeated requests for the current state commit as no-ops without replacing attribution or adding audit noise. Because resolver indexes are not process-cached, a committed transition is visible on the next read and changes the representation ETag.

## Registry identities

Namespace and package identity segments preserve their submitted spelling for display while using a normalized collision key for lookup, uniqueness, ordering, and hashing. A segment is 1–64 ASCII bytes, begins and ends with an alphanumeric character, and contains only alphanumeric characters separated by single `-` or `_` characters. Digits may appear first. Normalization lowercases ASCII letters and folds `_` to `-`, so spellings such as `Foo_Bar` and `foo-bar` identify the same segment. Unicode, whitespace, punctuation, leading or trailing separators, and adjacent separators are invalid. Reserved or blocked identities are publication policy rather than segment syntax.

## Versions and dependency ranges

Package versions are strict Semantic Versioning 2.0.0 values. The major, minor, and patch components are required `u64` decimal integers without leading zeros. Optional prerelease and build identifiers are non-empty, dot-separated ASCII alphanumeric or hyphen strings; numeric prerelease identifiers cannot contain leading zeros. Whitespace and a leading `v` are not accepted. Submitted spelling is preserved. Build metadata is part of immutable version identity, equality, hashing, and deterministic total ordering, so `1.2.3+linux` and `1.2.3+windows` are distinct versions. Semantic-version precedence ignores build metadata.

Rux registry dependencies use Cargo-style ranges. A range is either a standalone `*`, `x`, or `X`, or an intersection of at most 32 comma-separated comparators. Comparators support `=`, `>`, `>=`, `<`, `<=`, `~`, and `^`; an omitted operator means caret compatibility. Partial versions and `*`, `x`, or `X` component wildcards are accepted, including `1`, `1.2`, `1.*`, and `1.2.x`. ASCII spaces are allowed around operators and commas, but not within a partial version. Unions, hyphen ranges, and whitespace-separated comparator lists are not supported. Caret compatibility permits changes to components to the right of the leftmost nonzero component; tilde compatibility permits patch updates when a minor component is present.

Build metadata in a comparator is validated but ignored during evaluation. A prerelease version satisfies a range only when the range contains a comparator with the same complete major, minor, and patch tuple and an explicit prerelease identifier. Consequently, wildcard and ordinary release ranges do not select prereleases implicitly.

## Package manifests

The registry accepts the strict, PascalCase Rux manifest schema documented in [Rux Manifest v1](manifest.md). Every manifest declares `[Manifest] Version = 1` and a strict `MinRux` of at least `0.4.0`, then contains exactly one package or workspace. Package types are Executable, SharedLibrary, StaticLibrary, and SourceLibrary; the 0.4.0 publication profile accepts only SourceLibrary. Package identities, semantic versions, dependency ranges, SPDX licensing, catalog URLs, portable paths, metadata sizes, and collection counts are validated before the manifest reaches application use cases. Unknown fields, retired type spellings, and legacy unversioned manifests are rejected.

Build configuration has only Debug and Release modes. Both have stable defaults and optional mode-specific overrides for optimization, debug information, debug assertions, output, and typed compile-time definitions. Custom build profiles are not part of manifest v1. Callers select either local validation, which may omit a namespace and permits path dependencies and workspaces, or publication validation, which requires a namespace and rejects path dependencies and workspaces.

## Data and failure model

- Package versions and object keys are immutable; yanking changes resolver visibility, not stored bytes.
- Database transactions protect ownership, publication, token use, audit events, and metadata changes.
- Background aggregation and cleanup jobs are repeatable and bounded.
- Liveness reports only process health. Readiness requires PostgreSQL and the package bucket.
- API errors use stable problem identifiers and never expose internal errors, credentials, storage keys, or database identifiers.

## Operations

Production runs Ubuntu 26.04 LTS with Caddy, PostgreSQL 18, and an unprivileged systemd API service. Releases are immutable directories selected by a `current` symlink. The [release workflow](releases.md) applies reviewed migrations deliberately, restarts, verifies readiness, and restores the prior application symlink on failure without reverting production migrations.

An idempotent Ansible playbook configures an existing dedicated x86-64 droplet. It owns the SSH hardening snippet and nftables ruleset, admits administrator SSH only from declared CIDRs, exposes only Caddy publicly, and keeps PostgreSQL, the API, metrics, Prometheus, Alertmanager, and Grafana on loopback. Provisioning creates the release and shared-directory boundary but leaves the first artifact, migrations, and `current` symlink to the release workflow.

Logs are structured JSON on stdout and captured by journald. Request, dependency-probe, and cleanup spans carry trace and span identifiers shared with optional OTLP/gRPC traces. Prometheus scrapes separate loopback-only API and recovery metrics listeners; the public proxy never exposes them. Repository-owned Grafana dashboards and Prometheus alert rules cover traffic, failures, dependency health, cleanup, backups, and WAL archival, while host provisioning supplies private alert receivers. See [observability](observability.md) and the [alert runbook](observability-runbook.md). Encrypted pgBackRest backups and WAL archives go to a dedicated versioned Space in another region, package bucket versioning remains enabled, and both restore paths are rehearsed quarterly under the [production recovery contract](recovery.md).

The API applies the [abuse-control boundary](abuse-controls.md) behind Caddy. Only configured proxy networks may supply client-address forwarding data; per-client rate limits, request deadlines, publication admission, and fixed upload bounds are enforced in the Rust process. Server-generated request IDs correlate public responses with secret-safe request telemetry.
