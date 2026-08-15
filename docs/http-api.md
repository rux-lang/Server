# HTTP API v1

The public registry API uses OpenAPI 3.1 and JSON over HTTPS. Its routes are versioned below `/v1`; the generated contract is available at `/openapi/v1.json`. Operational routes below `/health` are intentionally not part of the public API contract.

Every API response includes an opaque, server-generated `X-Request-ID`. Caller values are replaced rather than trusted. Browsers may read this header through CORS and operators can use it to correlate a response with structured logs.

## Success responses

Every successful JSON response has one top-level `data` member. A single resource is an object and a collection is an array:

```json
{
  "data": {
    "display_name": "Example"
  }
}
```

```json
{
  "data": [
    { "display_name": "Alpha" },
    { "display_name": "Beta" }
  ]
}
```

Public JSON member names use `snake_case`. Clients must ignore unknown members so optional envelope metadata can be added compatibly in a later contract.

## Problem responses

Errors use [RFC 9457](https://www.rfc-editor.org/rfc/rfc9457.html) problem details and the `application/problem+json` media type. Every response contains:

- `type`: the stable problem type URI.
- `title`: a short summary that is stable for the problem type.
- `status`: the 4xx or 5xx HTTP status, equal to the response status.
- `code`: the stable snake_case machine identifier.

Rux-specific problem types use `https://api.rux-lang.dev/problems/{code}`. The last URI segment and `code` must agree. That unversioned namespace is reserved because problem identities must remain stable across API versions.

The optional `detail` member explains the specific occurrence. The optional `instance` member is a URI reference identifying that occurrence. Neither member may expose implementation errors, credentials, storage keys, database identifiers, or other secrets.

```json
{
  "type": "https://api.rux-lang.dev/problems/not_found",
  "title": "Not Found",
  "status": 404,
  "code": "not_found"
}
```

Validation problems may include a non-empty `errors` array. Each entry has a snake_case `code`, a human-readable `detail`, and an optional RFC 6901 JSON Pointer locating the invalid value in the request:

```json
{
  "type": "https://api.rux-lang.dev/problems/invalid_request",
  "title": "The request is invalid",
  "status": 422,
  "code": "invalid_request",
  "detail": "One or more fields are invalid.",
  "errors": [
    {
      "code": "invalid_name",
      "detail": "must contain only lowercase letters",
      "pointer": "/name"
    }
  ]
}
```

Clients use `type` as the primary problem identity, may use `code` as its convenient machine-readable equivalent, must not parse `title` or `detail`, and must ignore unknown extension members.

## Abuse-control responses

Rate-limited operations return `429` with the stable `rate_limited` problem and an integer `Retry-After` header. Requests that exceed their execution deadline return the stable `504 request_timeout` problem. Publication retains its documented `413 upload_too_large` and `503 publication_unavailable` responses; admission saturation includes `Retry-After: 1`. The playground returns `503 playground_unavailable`, also with `Retry-After: 1`, when the sandbox is not answering or its admission limit is saturated. These responses contain no limiter key, client address, credential, or internal error detail.

## Browser authentication

`GET /v1/auth/github` starts the GitHub authorization-code flow. It sets the short-lived, host-only `__Host-rux_oauth_state` cookie and redirects to GitHub with the configured callback URL. It does not accept a caller-selected return location or request an explicit GitHub OAuth scope.

GitHub returns to `GET /v1/auth/github/callback`. The API requires the callback state to match the browser cookie, clears that cookie on every outcome, and redirects with `303 See Other` to the exact configured web origin at `/packages/-/auth/callback`. Successful login also sets the opaque, host-only `__Host-rux_session` cookie. Failures add exactly one stable `error` query value: `oauth_state_invalid`, `oauth_callback_invalid`, `oauth_access_denied`, `oauth_authorization_failed`, `oauth_provider_unavailable`, `account_conflict`, or `authentication_unavailable`. Provider descriptions and callback input are never reflected.

Successful login sets independent `__Host-rux_session` and `__Host-rux_csrf` credentials. `GET /v1/auth/session` requires the exact configured web `Origin` and the session cookie, and returns the active user's GitHub login, optional display name and avatar URL, absolute expiry, and CSRF token in a `data` envelope. The response uses `Cache-Control: no-store`. Missing, malformed, unknown, expired, revoked, and anonymized sessions return the stable `authentication_required` problem; persistence failures return `authentication_unavailable`.

The session endpoint also refreshes a missing or invalid legacy CSRF credential and rotates sessions at seven-day boundaries. Rotation atomically creates new independent session and CSRF credentials, revokes the old session, and preserves the original expiry. Activity never extends the absolute 30-day lifetime.

`POST /v1/auth/logout` requires the exact web `Origin`, both cookies, and an `X-CSRF-Token` header equal to the CSRF cookie and stored hash. A mismatch returns `csrf_invalid` without clearing or revoking the session. A valid request revokes the session, clears both cookies, and returns `204 No Content`. Missing, malformed, unknown, expired, and already-revoked session credentials remain idempotent success cases when the request comes from the allowed origin. A persistence failure leaves valid cookies in place so the browser can retry.

All three authentication cookies are `Secure`, `HttpOnly`, `SameSite=Lax`, use `Path=/`, omit `Domain`, and therefore satisfy the `__Host-` cookie contract. OAuth state lasts 10 minutes. Session and CSRF cookies share the session's remaining lifetime. Credentialed CORS permits only the exact configured web origin, the supported `GET`, `POST`, `PATCH`, and `DELETE` methods, and the explicit `Content-Type` and `X-CSRF-Token` request headers.

## Account lifecycle

`DELETE /v1/account` is a browser-session-only operation. It requires the exact configured web `Origin`, both authentication cookies, and a matching `X-CSRF-Token`. The closed JSON body contains the active GitHub login as an exact, case-sensitive confirmation: `{"github_login":"octocat"}`. A malformed body or mismatch returns `invalid_request`; API bearer tokens are not accepted.

Deletion locks the account and all of its membership namespaces before making changes. If the account is the final owner of any namespace, the complete operation returns `last_owner_required` and the user must add or promote another owner first. Otherwise one transaction revokes incoming invitations, removes memberships, revokes every session and API token, replaces token display names with a neutral deleted-account label, clears the GitHub identity and profile, and appends the `account_anonymized` audit event. Outgoing invitations remain actionable and show an anonymous inviter.

Successful deletion returns `204 No Content` with `Cache-Control: no-store` and clears both browser cookies. The anonymous internal user row and historical attribution references remain, so packages, versions, download records, and audit records are not rewritten or removed. Signing in later with the same GitHub identity creates a new account without restoring access. Persistence or audit failures return `account_lifecycle_unavailable` and roll back every change.

## API tokens

Browser token management requires the exact configured web `Origin` and an active session. `GET /v1/tokens` returns all of the user's tokens newest-first, including expired and revoked history, with a safe `token_prefix`, ordered scopes, lifecycle timestamps, and an `active`, `expired`, or `revoked` status. It never returns a credential or hash. The response is non-cacheable and may rotate the browser session in the same way as `GET /v1/auth/session`.

`POST /v1/tokens` additionally requires the CSRF cookie and matching `X-CSRF-Token` header. Its JSON body contains a display name, one to three unique scopes chosen from `publish`, `yank`, and `namespace`, and an optional future RFC 3339 `expires_at`. It returns `201 Created` with the token metadata and its `rux_pat_` bearer credential. That creation response is the only time the credential is available and uses `Cache-Control: no-store`.

`DELETE /v1/tokens/{token_prefix}` uses the same session and CSRF protections. It returns `204 No Content` whether the owner-scoped prefix was revoked, already revoked, absent, or owned by another user, so it does not disclose token ownership. Revoked and expired entries remain visible in token history.

Machine endpoints accept these credentials through `Authorization: Bearer`. Malformed, unknown, expired, revoked, and anonymized credentials share the `authentication_required` response. An active credential missing the operation's required scope returns `insufficient_scope`. Successful authorization updates the token's last-use timestamp in the same transaction as the protected registry mutation.

`GET /v1/me` is the one bearer endpoint that requires no scope of its own, and it exists so a client can verify a credential before relying on it: every other bearer route demands a specific scope, so a `publish` token would otherwise have nothing to call but a real publication. It takes no session or `Origin` and accepts no cookies. An active credential returns the owner's `github_login`, the token's safe `token_prefix`, its ordered scopes, and its optional `expires_at`, non-cacheable and carrying no credential, hash, or database identifier; anything else returns `authentication_required`, so the endpoint reveals nothing to a caller who does not already hold a working token. Like any other authorization it updates the token's last-use timestamp, and being unscoped it sits in the security abuse tier alongside the browser token routes. `rux login` uses it to reject a mistyped or revoked token before a package is built, and to report which scopes the stored credential actually carries.

Successful login and session lifecycle changes, token lifecycle changes, and namespace management mutations are audited in the same database transaction as the protected change. Audit failure is fail-closed. The audit contract retains only safe identifiers and action-specific allowlisted context; request bodies, OAuth values, cookies, CSRF values, bearer credentials, and credential hashes are never copied into audit records. Failed requests, idempotent no-ops, and routine last-seen or last-used timestamp updates are not audit events.

## Namespace management

Namespace management accepts either the browser session contract or an API token with the `namespace` scope. When an `Authorization` header is present it is authoritative and the API never falls back to cookies. Browser reads require the exact configured web `Origin`; browser mutations additionally require the CSRF cookie and matching `X-CSRF-Token`. Management reads use `Cache-Control: no-store`.

`GET /v1/namespaces` lists the caller's memberships in normalized namespace order. `POST /v1/namespaces` claims the submitted display-preserving identity and atomically makes the caller its first `owner`. Normalized spellings collide, so `Foo_Bar` and `foo-bar` cannot be claimed separately.

`GET /v1/namespaces/{namespace}/members` is available to members. Owners change an existing member's `owner` or `maintainer` role with `PATCH /v1/namespaces/{namespace}/members/{github_login}` and remove members with `DELETE` on the same route. A maintainer may remove only themselves. An owner may leave or demote themselves only while another owner remains; every namespace always retains at least one owner.

Owners list actionable invitations at `GET /v1/namespaces/{namespace}/invitations` and invite an existing, non-anonymized registry user by GitHub login with `POST` on the same route. The server fixes expiry at seven days. Invitations cannot target the inviter or an existing member, and only one unresolved invitation may exist for a user and namespace. Expired, revoked, or declined invitations may be replaced.

`GET /v1/invitations` lists the caller's unexpired invitations newest-first. `POST /v1/invitations/{namespace}/accept` atomically creates membership with the invited role and accepts the invitation. Owners revoke and invitees decline via `DELETE /v1/namespaces/{namespace}/invitations/{github_login}`. Delete and repeated acceptance operations are idempotent where the resulting membership or absence already matches the request. Invitation database identifiers are never exposed.

## Owner dashboard

`GET /v1/dashboard` is a browser-session-only, non-cacheable overview. It requires the exact configured web `Origin` and an active session, may rotate that session, and does not accept API tokens as dashboard authentication. The response reports all current owner and maintainer memberships and all unexpired incoming invitations. It also includes total namespace, package, and invitation counts; the ten distinct packages with the most recent publication times; the ten newest visible audit activities; and download summaries for all versions in the caller's current namespaces.

Package previews include all packages, even when their most recently published release is yanked. Each row exposes its most recently published exact version, yank state, total version count, and host-independent canonical package and version URLs. Download totals cover both the inclusive 30-day window ending at request time and all events through request time. The five non-zero 30-day leaders use download count followed by normalized namespace and package as a stable order. Download events from yanked versions remain included.

Activity is scoped by the caller's current role. Owners see namespace, membership, invitation, publication, yank, and unyank events in their namespaces. Maintainers see namespace creation and package lifecycle events, plus membership or invitation events where they are the actor or target. The response maps audit rows into a closed structured activity schema. It never exposes audit identifiers, subject keys, token identifiers, raw metadata, or internal database identifiers; anonymized actors and targets are represented without an identity profile.

Persistence failures return `dashboard_unavailable`. A missing or ended session returns `authentication_required`, and an invalid browser origin returns `csrf_invalid`.

## Package publication

`POST /v1/packages` requires a bearer token with the `publish` scope and a `multipart/form-data` body containing exactly one case-sensitive `manifest` part and one `package` part. The route applies the documented request, manifest, artifact, expansion, and temporary-storage limits before authorizing the manifest identity and reserving its immutable version.

Rux 0.4.0 publication accepts only `Type = "SourceLibrary"`. An otherwise valid Executable, SharedLibrary, or StaticLibrary upload returns `422 invalid_artifact` with a nested `not_publishable` error whose pointer is `/Package/Type`. The database and read APIs retain all four package-type values for future catalog support.

A successful request returns `201 Created` with the display namespace, package, exact version, and RFC 3339 publication timestamp. `Location` identifies the canonical exact-version resource using normalized namespace and package path segments. The artifact SHA-256, byte size, and internal immutable storage key are persisted but the storage key is never returned.

Malformed multipart requests, invalid artifact diagnostics, authentication and scope failures, namespace policy, immutable version conflicts, and operational storage or database failures use the shared RFC 9457 problem contract. If object storage succeeds but PostgreSQL cannot commit, the object remains for a bounded delayed orphan sweep rather than being deleted in the request path.

## Package metadata

Package metadata is public and unauthenticated. `GET /v1/packages/{namespace}/{package}` returns the version-independent package identity, package creation time, and canonical package URL. It deliberately does not select a representative release or return version history. Namespace and package lookups use normalized identities while the response preserves the stored display spelling.

`GET /v1/packages/{namespace}/{package}/{version}` returns one exact strict Semantic Version, including build metadata. It exposes the stored normalized manifest; package type, minimum Rux version, description, authors, keywords, catalog URLs, and dependencies; raw referenced README source; license data; the artifact SHA-256 and bounded artifact/source metrics; publication time; and current yank state. Dependencies are ordered by normalized alias. A dependency's optional `target_os` is an ordered array of exact manifest OS names; omission means the edge is unconditional. Authors and keywords retain manifest order.

`readme_file` and `license_file` are each either `null` or an object containing `path` and unrendered UTF-8 `source`. `license` is either `null` or the SPDX expression as a string, and is independent of `license_file` — a release may carry either, both, or neither:

```json
{
  "readme_file": {
    "path": "README.md",
    "source": "# Example\n"
  },
  "license": "MIT OR Apache-2.0",
  "license_file": {
    "path": "LICENSE.md",
    "source": "MIT License\n\nCopyright (c) 2026 ...\n"
  },
  "checksum": {
    "algorithm": "sha256",
    "digest": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  }
}
```

A file license instead uses `{"kind":"file","path":"LICENSE.md", "source":"..."}`. Source is returned verbatim; browser sanitization and rendering are not API responsibilities.

Both responses expose host-independent relative canonical URLs. Namespace and package path segments use normalized spelling; the version path segment retains the exact version. Exact-version responses contain both `package_url` and their own `canonical_url`. The publication response's `Location` header uses the same canonical version-path builder. Exact-version responses also contain `download_url`, the host-independent canonical download route built from normalized namespace and package segments plus the exact version. Clients never guess a URL from internal storage keys.

Invalid namespace, package, and version path values return `invalid_request` with a source pointer. Missing summaries return `package_not_found`; an absent exact package/version combination returns `package_version_not_found`. Persistence failures return `package_metadata_unavailable`. Responses never contain database identifiers, creator or mutation actor identifiers, storage keys, or credentials.

## Package yank state

`PATCH /v1/packages/{namespace}/{package}/{version}` sets the desired yank state for one exact immutable version. It requires an API bearer token with the `yank` scope and current `owner` or `maintainer` membership in the package's namespace. Browser sessions are not accepted. The closed JSON request contains exactly one Boolean member: `{"yanked": true}` or `{"yanked": false}`.

A successful request returns `200 OK` with the stored display namespace and package, exact version, and resulting `yanked` flag in the standard data envelope. Setting the state already in effect is an idempotent success: it does not rewrite the original yank timestamp or actor and does not append another audit record. Real transitions and their token actor are audited atomically with the state change.

Invalid path values or request bodies return `invalid_request`. Missing, malformed, unknown, expired, or revoked credentials return `authentication_required`; a token without `yank` returns `insufficient_scope`; a caller without current namespace membership receives `yank_forbidden`; and an absent namespace/package/version combination returns `package_version_not_found`. Persistence or audit failures return `yank_unavailable` and leave the previous state committed.

Yanking affects new resolver selection only. The exact metadata route exposes the current flag, and known yanked versions remain downloadable. Resolver reads come directly from committed PostgreSQL state, so the next revalidation after a transition returns the changed representation and a new strong ETag.

## Package downloads

Package downloads are public and unauthenticated. `GET /v1/packages/{namespace}/{package}/{version}/download` resolves one known exact version, appends and commits a durable download event, then returns `307 Temporary Redirect` with an absolute configured CDN `Location`. The response has an empty body and `Cache-Control: no-store`, ensuring later GET requests continue through registry accounting rather than reusing a cached redirect.

`GET /v1/packages/{namespace}/{package}/downloads` is the public, side-effect-free package statistics view. It aggregates all versions, including yanked releases, over the thirty UTC calendar days ending with the day of the request. The standard data envelope contains `window_days`, inclusive `start_date` and `end_date` values, `total_downloads` for that window, `total_all_time` through the same request instant, and exactly thirty ordered `daily` rows containing an ISO 8601 date and count. Dates without events are retained with zero counts. The final row is the current UTC day and is therefore partial: it counts only the events recorded so far and grows as the day proceeds, so consecutive reads of the same package can return a larger last row and larger totals. Both totals are current as of the request rather than as of a day boundary, matching the rolling window `GET /v1/highlights` uses for popularity. The endpoint accepts no query parameters and never returns individual events or actor information.

The API records the event before returning the redirect. A lookup, transaction, insert, or commit failure returns `download_unavailable` and does not return a CDN location. Each successful GET is one event; it measures a registry download request rather than completion of the subsequent CDN transfer. The route does not probe object storage in the request path.

`HEAD` on the same route resolves and redirects to the same target without recording an event. Known yanked versions remain downloadable by GET and HEAD; yanking affects new resolver selection rather than immutable stored bytes.

The CDN destination is constructed by appending the version's immutable storage key to `RUX_PACKAGE_CDN_BASE_URL`. That startup-validated base must be an absolute hierarchical URL ending in `/`, without credentials, a query, or a fragment, and must use HTTPS except for loopback development.

Invalid namespace, package, and version values return `invalid_request` with a source pointer. An unknown exact version returns `package_version_not_found`; a missing package statistics target returns `package_not_found`. Database failures on redirects return `download_unavailable`, while statistics reads use `discovery_unavailable`. No download response serializes database or actor identifiers.

## Resolver index

`GET /v1/index/{namespace}/{package}` is an unauthenticated machine-facing index for dependency resolution. Namespace and package path segments use the registry identity syntax and are matched by normalized identity; the response preserves the stored display spelling. Invalid segments return `invalid_request`, an unknown package returns `package_not_found`, and database failures return `resolver_index_unavailable` without exposing internal details.

The response contains versions in ascending registry semantic-version order. That total order follows SemVer precedence and uses build metadata as the deterministic tie-breaker. Each version contains its exact version, minimum Rux version, current `yanked` flag, and dependencies ordered by normalized alias. Dependencies contain their display alias, target namespace and package, original validated version range, and an optional `target_os` allow-list. A missing `target_os` means the dependency applies to every target. A present value contains one or more unique exact names from `FreeBSD`, `Linux`, `macOS`, and `Windows`; clients must reject malformed or empty values rather than resolving an untrusted index. Yanked versions remain in the index so an existing lock can identify them, while clients must avoid selecting them for a new resolution. Artifact checksums and broader version metadata are provided by the exact-version metadata contract rather than duplicated here.

```json
{
  "data": {
    "namespace": "Rux",
    "package": "Example",
    "versions": [
      {
        "version": "1.0.0",
        "min_rux": "0.4.0",
        "yanked": false,
        "dependencies": [
          {
            "alias": "Json",
            "target_namespace": "Rux",
            "target_package": "Json",
            "version_range": "^1",
            "target_os": ["Windows"]
          }
        ]
      }
    ]
  }
}
```

Successful responses carry `Cache-Control: public, no-cache` and a strong `ETag` derived from the exact deterministic JSON bytes. `If-None-Match` accepts the standard wildcard, weak or strong entity tags, lists, and repeated field lines. A weak match returns `304 Not Modified` with the current `ETag` and cache policy but no body. Malformed conditions are ignored. The API does not retain a process-local resolver cache; every request reads the current PostgreSQL state, so publication and yank changes immediately produce a new representation and validator.

## Package search

`GET /v1/search` is public and unauthenticated. It powers both catalog browsing and ranked literal search. The optional `q` parameter is trimmed, collapses whitespace, excludes NUL, and is limited to 256 UTF-8 bytes. An omitted or blank query browses packages in normalized namespace and package order. Search text is passed to PostgreSQL `plainto_tsquery` and escaped before partial identity matching, so punctuation and wildcard characters are data rather than query syntax.

Optional `namespace` and `keyword` parameters are exact normalized registry identity filters. `package_type` is exactly `executable`, `shared_library`, `static_library`, or `source_library`. Filters apply to the representative version. Query parameters are closed and scalar: unknown, repeated, malformed, or out-of-range values return `invalid_request`.

`sort` selects the result ordering and is exactly one of `relevance`, `name`, `downloads`, `recent_downloads`, `updated`, or `created`. It defaults to `relevance` when `q` is present and to `name` otherwise, because relevance scores are uniformly zero without a query. `downloads` counts every recorded download; `recent_downloads` counts only the last 30 days, the same window highlights calls popular. `updated` orders by the representative version's publication date, `created` by when the package itself first appeared. The optional `order` is `asc` or `desc`; it defaults to `asc` for `name` and `desc` for every other sort. Relevance only accepts `desc`, because ascending relevance would deliberately surface the weakest matches. Every ordering ends with normalized namespace and package as a total tie-breaker, so a row cannot land on two pages or on none.

Pagination is offset-based. `page` is 1-based, defaults to 1, and is bounded from 1 through 10,000 — a deep offset costs as much as every page before it, so the ceiling keeps a crafted request from scanning the whole catalog. `per_page` defaults to 20 and must be between 1 and 100 inclusive. `limit` is a deprecated alias for `per_page`; supplying both returns `conflicting_page_size`. A page past the end returns an empty `data` array rather than an error.

Each package is represented by its highest non-yanked stable version, then its highest non-yanked prerelease. If every version is yanked, the same stable-first ordering selects a yanked fallback and the result exposes `yanked: true`. Semantic Version ordering includes build metadata as the deterministic tie-breaker established by the domain contract.

Under the default ordering with `q`, results are ordered by exact qualified identity, exact package, exact keyword, exact namespace, and then partial or full-text relevance. Quantized trigram and full-text ranks break ties before normalized namespace and package. The response is a collection envelope with page metadata:

```json
{
  "data": [
    {
      "namespace": "Rux",
      "package": "Json",
      "version": "1.1.0",
      "package_type": "shared_library",
      "description": "Fast JSON parsing with streaming support.",
      "published_at": "2026-03-10T12:00:00Z",
      "yanked": false,
      "downloads_total": 48210,
      "downloads_30d": 3105,
      "package_url": "/v1/packages/rux/json",
      "version_url": "/v1/packages/rux/json/1.1.0"
    }
  ],
  "meta": {
    "total": 137,
    "page": 1,
    "per_page": 20
  }
}
```

`meta.total` is the size of the whole result set for the given criteria, counted in the same statement that reads the page, so a client can render a page count without a second request. It is `0` on a page past the end, where there are no rows to carry it. `meta.page` and `meta.per_page` echo the effective values after defaulting. Pagination is not a database snapshot: publication or yank changes between requests may move a result across a page boundary. Persistence failures return `search_unavailable`.

## Package discovery

Discovery reads are public and unauthenticated. Except for highlights and keywords, each collection uses an opaque versioned keyset cursor and returns `{"data": [...], "meta": {"next_cursor": null}}`. Unknown, repeated, malformed, or out-of-range query parameters return `invalid_request`. Ordinary discovery limits default to 20 and are bounded from 1 through 100. Sitemap limits default to 100 and are bounded from 1 through 1,000. A cursor is bound to its collection and, for package-scoped reads, its normalized package identity. Cursor kind `2` is retired — it belonged to the keyword index before it moved to page numbers — and is never reissued.

`GET /v1/packages/{namespace}/{package}/dependents` lists one row per package whose representative version declares the requested target. Representative selection matches search: highest active stable, then active prerelease, then a yanked stable-first fallback. Multiple aliases targeting the same package are grouped into ordered `requirements` containing the display alias, original version range, and optional `target_os` allow-list. Results are ordered by normalized dependent namespace and package. A missing target returns `package_not_found`.

`GET /v1/keywords` aggregates the representative versions' keywords. Each row contains display and normalized spelling plus the number of distinct packages. The newest representative publication supplies display spelling, with package identity as the deterministic tie-breaker.

The keyword index is the one discovery collection paginated by page number rather than by cursor, because it offers a choice of ordering that a keyset cursor cannot follow. `sort` is `packages` (the default — package count descending, then normalized keyword) or `name` (normalized keyword ascending); both end on the normalized keyword, which is unique per row, so the ordering is total. `page` is 1-based, defaults to 1, and is bounded from 1 through 10,000. `per_page` defaults to 20 and is bounded from 1 through 100, with `limit` accepted as a deprecated alias — supplying both returns `conflicting_page_size`. The envelope is `{"data": [...], "meta": {"total": 137, "page": 1, "per_page": 20}}`, where `total` is the number of distinct keywords and is `0` on a page past the end.

`GET /v1/packages/{namespace}/{package}/versions` returns every immutable release, including prereleases, build variants, and yanked versions. Rows are ordered by descending registry Semantic Version total order and expose the version, minimum Rux version, package type, publication time, yank state, and canonical metadata and download URLs. A missing package returns `package_not_found`.

`GET /v1/highlights` returns at most ten `recent` and ten `popular` package records. Both groups require a non-yanked representative and therefore never promote an all-yanked package. Recent results use representative publication time. Popular results sum registry download events across all package versions from the inclusive 30-day window ending at request time, omit zero-download packages, and expose that count as `downloads_30d`. Highlights are a fixed bounded response and do not paginate.

`GET /v1/sitemap` pages through structured `keyword`, `namespace`, and `package` records ordered by kind and normalized identity. Records preserve display spelling, expose normalized path segments, and carry the newest relevant publication timestamp as `last_modified`. They contain source data rather than frontend paths so the static catalog retains ownership of its route scheme. Only identities with published catalog content are included. Yank transitions do not alter sitemap timestamps.

Package lookup failures return `package_not_found`. Persistence failures from any discovery route return `discovery_unavailable`. Discovery responses expose no database identifiers, publisher attribution, storage keys, or download-event details.

## Playground

`POST /v1/playground/run` and `GET /v1/playground/limits` compile and run a submitted program in a throwaway container. Both are anonymous and present only when the playground is enabled; otherwise they answer `404`. The run endpoint requires an exact-match `Origin` and returns `403 origin_not_allowed` without one, because it is unauthenticated and executes code. The limits endpoint is a side-effect-free public document and is not origin-checked.

The run request body is closed and limited to 64 KiB. `source` is required; `mode` is `run`, `build`, or `fmt` and defaults to `run`; `profile` is `debug` or `release` and defaults to `debug`; `stdin` defaults to empty. The response reports `build`, an optional `program` for executed runs, and an optional `formatted` for `fmt`. A **failed compile is a `200`** carrying `build.success: false` with diagnostics — problem responses are reserved for submissions that never ran. Clients branch on `build.success`, not on the status code.

A submission that breaks a documented bound returns `422 invalid_request` with `invalid_playground_request` inside `errors[]`; the detail names a size, bound, or field and never echoes submitted source. A sandbox that is stopped, saturated, or internally broken returns `503 playground_unavailable` with `Retry-After: 1`, and a run exceeding the server deadline returns `504 request_timeout`. The full contract, resource envelope, and container isolation are documented in [playground](playground.md).

## OpenAPI composition

The OpenAPI document declares `/v1` as its relative server URL. Paths in the document are relative to that base and must not repeat the `/v1` prefix. It publishes reusable `DataEnvelope`, `Problem`, and `ValidationError` schemas plus an `application/problem+json` `ProblemResponse` component. Concrete endpoint schemas specialize the envelope's `data` member with their response DTO.
