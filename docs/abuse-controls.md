# Abuse controls

The Rust API is the authoritative abuse-control boundary. Caddy terminates TLS and forwards connections, but application middleware generates request IDs, derives a trusted client key, enforces quotas and deadlines, and emits only RFC 9457 failures. The controls are intentionally in-process for the initial single-instance production topology and reset when the API restarts.

## Client address and request identity

`RUX_TRUSTED_PROXY_CIDRS` is a comma-separated list of IPv4 or IPv6 networks. It defaults to `127.0.0.0/8,::1/128` for same-host Caddy. An empty value disables proxy trust. When the TCP peer is trusted, the API parses all `X-Forwarded-For` values from right to left, skips trusted proxy hops, and uses the first untrusted address. A malformed trusted-proxy header is rejected; a forwarding header from an untrusted peer is ignored. IPv6 client keys are normalized to their `/64` prefix.

Every request receives a random UUIDv4 `X-Request-ID`. Any supplied value is discarded before tracing. The generated value is returned on success and error responses and recorded with the trace and span identifiers.

## Rate and time limits

The keyed token buckets use the following startup defaults:

| Tier        | Routes                                            | Per minute | Burst |
| ----------- | ------------------------------------------------- | ---------: | ----: |
| Read        | Catalog, dashboard, downloads, discovery, OpenAPI |        120 |    60 |
| Security    | Authentication, account, and token routes         |         30 |    10 |
| Mutation    | Namespace membership/invitations and yank routes  |         60 |    20 |
| Publication | `POST /v1/packages`                               |          6 |     2 |

The corresponding `RUX_RATE_LIMIT_<TIER>_PER_MINUTE` and `_BURST` variables must be positive, the rate cannot exceed 10,000 per minute, and the burst cannot exceed the rate. CORS preflight and health probes do not consume these budgets. Inactive keys are pruned every minute. A rejected request returns `429 rate_limited` and `Retry-After` without logging the limiter key.

`RUX_REQUEST_TIMEOUT_SECONDS` defaults to 30 and is bounded from 1 through 120. `RUX_PUBLICATION_TIMEOUT_SECONDS` defaults to 120, must be at least the ordinary deadline, and is bounded at 600. Deadline cancellation drops staged-file and quota guards. The response is `504 request_timeout`.

## Upload admission and storage

The immutable publication contract remains a 6 MiB multipart request, a 5 MiB artifact, and a 65,536-byte manifest. `RUX_UPLOAD_TEMPORARY_CAPACITY_BYTES` defaults to 100 MiB and must be between 5 MiB and 10 GiB. An empty `RUX_UPLOAD_TEMPORARY_DIRECTORY` selects the operating-system temporary directory below `rux-server-uploads`; production may set an explicit service directory. `RUX_UPLOAD_MAX_CONCURRENCY` defaults to 8 and is bounded from 1 through 64. Saturation fails immediately as `503 publication_unavailable` with `Retry-After: 1`.

## Safe diagnostics

Request logging is allowlisted to request ID, method, matched route, status, duration, trace ID, and span ID. Raw paths, query strings, bodies, headers, client addresses, user/package identities, storage keys, and internal errors are not request fields. Authorization, cookie, CSRF, and response `Set-Cookie` headers are marked sensitive before tracing. Public problems use fixed codes and messages and never interpolate dependency errors or configuration values.

Changing the proxy chain requires updating `RUX_TRUSTED_PROXY_CIDRS` in the same release. Horizontal API scaling requires moving enforcement to a shared or edge limiter; independent in-process buckets are not a distributed quota.
