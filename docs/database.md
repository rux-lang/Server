# Database schema

PostgreSQL 18 is the source of truth for registry accounts, authorization, catalog metadata, audit history, and download events. The initial schema is installed by the reversible SQLx migration in `migrations/`. The [database migration workflow](migrations.md) defines creation, review, application, and rollback policy. Migrations are applied deliberately by operators and tests; the API does not apply them at startup.

## Entity relationships

```text
users ─┬─< sessions
       ├─< namespace_owners >─ namespaces ─< packages ─< package_versions
       ├─< namespace_invitations >─────────┘                 ├─< dependencies
       ├─< api_tokens ─< api_token_scopes                    ├─< authors
       └─< audit_records                                     ├─< keywords
                                                             └─< download_events

blocked_identities (operator-managed publication policy)
```

Entity tables — `users`, `sessions`, `namespaces`, `namespace_invitations`, `packages`, `package_versions`, and `api_tokens` — use `UUID` primary keys defaulting to the built-in `uuidv7()`. The append-only `audit_records` and `download_events` keep `BIGINT` identity keys, where row density matters and the key is never exposed. Version 7 is required rather than version 4: several indexes and keyset reads use `id` as a time-ordered tiebreaker, and a UUIDv7 sorts lexicographically by creation time, so it preserves both that ordering and B-tree insert locality. Internal primary keys never appear in the public API; a UUID key does, however, encode its own creation timestamp, so treat exposing one as also exposing when the row was created.

Foreign keys use restrictive deletion for durable catalog and audit history, cascading deletion for credentials and owned child rows, and nullification for attribution when a referenced row is physically removed. Normal account deletion retains an anonymized user row instead.

## Accounts and authorization

`users` retains GitHub's numeric identity and case-insensitive login while an account is active. An anonymized account clears both GitHub fields but retains its internal row so immutable publication history remains valid. Profile fields are bounded and optional because GitHub may omit them.

Account deletion is one explicit transaction. It locks the user and each membership namespace in normalized order, then rejects deletion if any membership is the namespace's final owner. A successful transaction revokes unresolved invitations targeting the user, removes all memberships, revokes every session and API token, replaces token display names with `Deleted account token`, clears all GitHub and profile fields, and sets `anonymized_at`. Outgoing invitations and durable attribution references remain; reads represent their anonymized actor as absent. The cleared GitHub identity may later create a new, independent user row.

`sessions` stores only fixed-length session and CSRF hashes. Browser session rotation creates a replacement row with the original absolute expiry and revokes the previous row in one transaction. Successful authentication updates last-seen at most once every five minutes. Expiration, last-seen, and revocation timestamps cannot precede creation. `api_tokens` likewise stores a safe display prefix and a fixed-length secret hash rather than the credential. Token scopes are normalized into `api_token_scopes` and limited to `publish`, `yank`, and `namespace`. Token history reads are owner-scoped and ordered newest-first. Revocation locks by the globally unique safe prefix plus the authenticated owner, while bearer authorization locks by secret hash and updates `last_used_at` only after the account, lifetime, revocation, and scope checks succeed.

Namespaces have `owner` and `maintainer` roles. Invitations target an existing registry user, carry the proposed role, and may be accepted or revoked but not both. A partial unique index permits only one pending invitation for a user and namespace.

Namespace claims insert the namespace and first owner in one transaction. Every later membership or invitation mutation locks authentication rows first, then the namespace row, followed by membership or invitation rows. Target-user lookups do not take row locks. That common order serializes role changes, removals, invitation acceptance, and revocation without cross-user deadlocks, allowing the application to enforce that at least one owner remains under concurrency. Expired unresolved invitations are revoked inside a locked re-invite transaction before the replacement row is inserted.

## Registry identities

Namespace names, package names, dependency aliases and targets, and keywords store the submitted display spelling. PostgreSQL generates the corresponding collision key by lowercasing ASCII letters and folding `_` to `-`. Check constraints enforce the same 1-64 byte ASCII grammar as the domain crate, and unique constraints use the generated form.

Exact namespace-qualified identity reads continue to use the uniqueness indexes. Global package and keyword identity indexes support catalog filters, while `pg_trgm` GIN indexes on normalized namespace, package, and keyword names support case-insensitive literal and partial matching. Dependency targets have a composite normalized-identity index for reverse-dependency reads.

`blocked_identities` is an operator-managed publication policy table. Rows block either a namespace segment or a package segment globally, using the same display-preserving syntax and generated normalized collision key as catalog identities. The migration intentionally installs no policy rows. Blocking is checked only when publishing; it does not prevent namespace claims or registry dependency declarations.

## Packages and versions

A package belongs to one namespace and is unique there by normalized name. A version preserves its strict Semantic Version spelling and stores parsed major, minor, and patch components as `NUMERIC(20,0)`. That type represents the full unsigned 64-bit range required by the domain contract. Exact version uniqueness includes prerelease and build metadata through the preserved version string, so build variants remain distinct immutable versions.

Each version stores query-critical manifest metadata, a normalized manifest JSON object, referenced README or license text, artifact SHA-256, immutable storage key, bounded artifact metrics, publisher attribution, and yank state. Authors and keywords use ordered child rows. Dependencies store normalized target identities instead of foreign keys because publication may reference a package that does not exist yet.

PostgreSQL stores a generated full-text vector for each version. It uses the `simple` configuration so technical terms are not stemmed or removed as English stop words, weights the description as `A`, and weights README text as `B`. A GIN index supports later ranked search queries. Package identities and keywords remain separate normalized search signals; representative-version selection and the final ranking formula belong to the search application use case. Stored prerelease and build-metadata sort keys reproduce the domain Semantic Version ordering in PostgreSQL, allowing the search query to select the highest active stable version, then an active prerelease, before falling back to yanked versions. A composite index bounds that per-package representative lookup.

Discovery queries reuse the representative-version ordering for dependents and keyword aggregation. Reverse dependencies page by dependent package rather than dependency row, so multiple aliases cannot split one package across pages. Version history has a package-prefixed descending registry-version index, and a time-first download-event index bounds the 30-day popularity window. Keyword counts and sitemap timestamps are derived from committed catalog rows; no materialized aggregation or process cache is maintained.

Version metadata and its author, keyword, and dependency rows are immutable after insertion. Only yank attribution and time may change. The database also keeps audit and download rows append-only. Each committed download row records the exact package version and registry request time before the API redirects to the CDN; it does not assert that the later CDN transfer completed.

Yank and unyank updates lock the exact version and are conditional on its current state. The first yank records its transaction timestamp and user; a repeated yank is unchanged rather than replacing that attribution. Unyank clears both fields, and repeated unyank is likewise unchanged. Authorization, namespace membership, the transition, and its audit record commit atomically.

Publication begins by locking the API token and user, then the namespace and the caller's membership. Both owners and maintainers may publish. After blocked identity checks, the transaction locks the package row when it exists and checks the exact version identity. A first publication retains the namespace lock until it creates the package. The prepared transaction remains open while the bounded artifact is uploaded, so competing publications wait and observe the committed winner or an abort before deciding version uniqueness. The database unique constraints remain the final race safeguard.

## Repository adapters

Application persistence is expressed as account, namespace, token, catalog, audit, and download capability ports. These ports use validated domain values, typed internal identifiers, fixed-length hashes and checksums, and stable error categories; SQLx types and database constraint names do not cross the application boundary.

Ordinary exact-key reads execute through the PostgreSQL pool. Every mutation requires an explicit unit-of-work transaction, including the narrow account transaction used to atomically upsert an OAuth user and create a session. Transaction-local locking reads are available for decisions that must remain valid until commit. Mutating repository methods never start or commit nested transactions. The caller composes ownership, publication, token, and audit changes and then explicitly commits or rolls back the complete operation. PostgreSQL's drop rollback is only a safety fallback.

Repository adapters cover all initial-schema tables. Immutable authors, keywords, dependencies, and token scopes are persisted as parts of their parent aggregates. Search, pagination, authorization policy, and reporting queries are implemented. Serialization and deadlock failures are classified as retryable, but no automatic retry policy is applied yet.

## Audit records

Successful security and namespace business mutations append one audit record in the same transaction as the state change. An audit insert failure therefore fails the operation and rolls back all of its writes. Failed requests, idempotent no-ops, session last-seen updates, token last-used updates, and the cleanup of an expired invitation while creating its replacement do not produce audit records.

The application exposes closed audit-event constructors rather than accepting arbitrary action names or metadata. Current actions are `session_created`, `session_rotated`, `session_revoked`, `account_anonymized`, `api_token_created`, `api_token_revoked`, `namespace_created`, `namespace_member_role_changed`, `namespace_member_removed`, `namespace_invitation_created`, `namespace_invitation_accepted`, `namespace_invitation_revoked`, `package_version_published`, `package_version_yanked`, and `package_version_unyanked`. Session subjects use the internal session UUID in its hyphenated lowercase text form, token subjects use the safe display prefix, namespace subjects use the normalized namespace identity, and package-version subjects use the internal package-version ID with display namespace, package, and exact version in allowlisted metadata. Namespace, publication, and yank actions authenticated by a bearer token retain both the owning user and token actor; browser actions retain only the user actor. Account anonymization uses the retained internal user UUID as its subject and has no metadata. Every internal identifier stored in a subject key or in metadata is written as a UUID string, never as a JSON number.

Metadata is action-specific and allowlisted. It is limited to replacement session UUIDs, ordered token scopes and expiry, display-preserved namespace and package names, exact package versions, target user UUIDs, roles, and invitation expiry. Audit values never accept OAuth codes or state, session or CSRF credentials, API token credentials or hashes, cookies, authorization headers, request bodies, client IP addresses, or user agents. The database supplies `occurred_at` at transaction time and keeps audit rows append-only.

Owner dashboard activity derives a normalized `namespace_key` from the closed audit shape: namespace subjects already use the normalized subject key, while package-version actions derive it from their allowlisted namespace metadata. The generated value is indexed with descending occurrence time and ID so current memberships can read a stable bounded activity feed without exposing the underlying audit representation. Recent package previews likewise use a package/publication-time index. Dashboard download queries reuse the existing time-first and package-version-first download indexes for 30-day and all-time aggregates.

## Local catalog fixtures

After applying migrations to the local Compose database, seed representative catalog metadata with:

```powershell
docker compose --profile tools run --rm catalog-seed
```

The opt-in `catalog-seed` service waits for PostgreSQL, runs `deploy/local/local-catalog.sql` with stop-on-error behavior, and removes its one-shot container afterward. The `tools` profile keeps it out of ordinary `docker compose up` runs.

The fixture contains 100 packages across 12 namespaces — `StdLib`, `CommunityTools`, `Acme`, `Northwind`, `Helio`, `Cobalt`, `Ironbark`, `Lumen`, `Meridian`, `Orbit`, `Sentinel`, and `Vantage` — with 2 to 10 releases each. It deliberately leaves `Rux` unused so that namespace stays free to claim by hand when exercising the dashboard. Its 633 versions span source, library, and program packages and cover prereleases, build-metadata variants, yanked releases, ordered authors and keywords, generated READMEs, license expressions, search text, a dependency graph, and 90 days of download history.

Display names are PascalCase with no separators, so `HttpClient` normalizes to `httpclient`. The `_` → `-` normalization path is still exercised, by the unit fixtures in `src/discovery.rs` rather than by this seed.

`deploy/local/local-catalog.sql` is **generated, not hand-edited**. Regenerate it after changing the curated list in `tools/catalog/packages.mjs`:

```powershell
node tools/catalog/generate-catalog.mjs
```

The generator is deterministic — its PRNG is seeded from each package's identity — so an unchanged input produces a byte-identical file. It prints the row counts asserted by `crates/infrastructure/tests/local_catalog_seed.rs`; update those together.

The seed is one transaction and takes a transaction-scoped advisory lock to serialize concurrent invocations. Every catalog insert uses stable identities with `ON CONFLICT DO NOTHING`; it never updates, deletes, or truncates data. `download_events` has no natural key to conflict on, so it is guarded by `NOT EXISTS` instead — without that, re-running would double every count and skew the popularity ranking. Running the seed again therefore preserves local changes and unrelated rows while adding only what is still absent.

Only PostgreSQL catalog metadata and download history are seeded. The fixture does not create users, ownership, credentials, audit records, or MinIO objects. Its artifact checksums and `local-seed/` storage keys are deterministic placeholders, so seeded package downloads are not expected to resolve to an object.

## Schema verification

The infrastructure migration and repository tests create isolated PostgreSQL databases. They verify the schema contract, adapter round trips, logical conflict mapping, full-width semantic-version components, aggregate ordering, and explicit commit and rollback behavior. Tests require a PostgreSQL URL whose user can create test databases:

```powershell
$env:DATABASE_URL = 'postgres://registry:registry@localhost:5432/registry'
cargo test -p rux-infrastructure --test migrations
```

The CI Rust job provides PostgreSQL 18 and runs these tests as part of the full workspace test command.
