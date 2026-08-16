# Abuse controls

The Rust API is the authoritative abuse-control boundary. Caddy terminates TLS and forwards connections, but application middleware generates request IDs, derives a trusted client key, enforces quotas and deadlines, and emits only RFC 9457 failures. The controls are intentionally in-process for the initial single-instance production topology and reset when the API restarts.

## Client address and request identity

`abuse.trusted_proxy_cidrs` is a comma-separated list of IPv4 or IPv6 networks. It defaults to `127.0.0.0/8,::1/128` for same-host Caddy. An empty value disables proxy trust. When the TCP peer is trusted, the API parses all `X-Forwarded-For` values from right to left, skips trusted proxy hops, and uses the first untrusted address. A malformed trusted-proxy header is rejected; a forwarding header from an untrusted peer is ignored. IPv6 client keys are normalized to their `/64` prefix.

Every request receives a random UUIDv4 `X-Request-ID`. Any supplied value is discarded before tracing. The generated value is returned on success and error responses and recorded with the trace and span identifiers.

## Rate and time limits

The keyed token buckets use the following startup defaults:

| Tier        | Routes                                            | Per minute | Burst |
| ----------- | ------------------------------------------------- | ---------: | ----: |
| Read        | Catalog, dashboard, downloads, discovery, OpenAPI |        120 |    60 |
| Security    | Authentication, account, and token routes         |         30 |    10 |
| Mutation    | Namespace membership/invitations and yank routes  |         60 |    20 |
| Publication | `POST /v1/packages`                               |          6 |     2 |
| Playground  | `POST /v1/playground/run`                         |         10 |     4 |

The corresponding `abuse.rate_limit.<tier>.per_minute` and `.burst` settings must be positive, the rate cannot exceed 10,000 per minute, and the burst cannot exceed the rate. CORS preflight and health probes do not consume these budgets. Inactive keys are pruned every minute. A rejected request returns `429 rate_limited` and `Retry-After` without logging the limiter key.

`abuse.request_timeout_seconds` defaults to 30 and is bounded from 1 through 120. `abuse.publication_timeout_seconds` defaults to 120, must be at least the ordinary deadline, and is bounded at 600. Deadline cancellation drops staged-file and quota guards. The response is `504 request_timeout`.

## Upload admission and storage

The immutable publication contract remains a 6 MiB multipart request, a 5 MiB artifact, and a 65,536-byte manifest. `uploads.temporary_capacity_bytes` defaults to 100 MiB and must be between 5 MiB and 10 GiB. Omitting `uploads.temporary_directory` selects the operating-system temporary directory below `rux-server-uploads`, while naming it empty is a mistake rather than a second way of asking for that; production may set an explicit service directory. `uploads.max_concurrency` defaults to 8 and is bounded from 1 through 64. Saturation fails immediately as `503 publication_unavailable` with `Retry-After: 1`.

## Playground admission

The [playground](playground.md) mirrors publication rather than the ordinary read path, because a run compiles and executes code instead of answering from the database. `playground.api.max_concurrency` defaults to 2 and is bounded from 1 through 16; saturation fails immediately as `503 playground_unavailable` with `Retry-After: 1` rather than queueing behind an admission wait. `playground.api.timeout_seconds` defaults to 30, must be at least `abuse.request_timeout_seconds`, and is bounded at 120; that relationship is checked at startup, in the one place both values exist. The endpoint is anonymous, so it additionally requires an exact-match `Origin` and answers `403 origin_not_allowed` otherwise — without it, any page could drive the sandbox from a visitor's browser. `playground.api.enabled` is false by default, and a disabled playground is not routed at all, so both endpoints answer `404`.

Per-run resource ceilings — memory, CPU, processes, filesystem, and the compile and execute timeouts — are enforced by the container rather than by this middleware, and are documented in [playground](playground.md).

## Safe diagnostics

Request logging is allowlisted to request ID, method, matched route, status, duration, trace ID, and span ID. Raw paths, query strings, bodies, headers, client addresses, user/package identities, storage keys, and internal errors are not request fields. Authorization, cookie, CSRF, and response `Set-Cookie` headers are marked sensitive before tracing. Public problems use fixed codes and messages and never interpolate dependency errors or configuration values.

Changing the proxy chain requires updating `abuse.trusted_proxy_cidrs` in the same release. Horizontal API scaling requires moving enforcement to a shared or edge limiter; independent in-process buckets are not a distributed quota.
