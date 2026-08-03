# Launch performance contract

This contract defines repeatable launch budgets for the initial single-host registry. The checked-in smoke job proves that the fixture, API, database, object-storage, and reporting paths remain executable. It is deliberately not a production capacity claim: the authoritative result comes from the exact release on a disposable reference host.

## Budgets

Every measured backend run follows a 60-second warm-up, uses an external load generator, accepts no dropped iterations, requires all response checks to pass, and permits an HTTP failure rate below 0.1%.

| Area                | Authoritative workload                                                                                                   | Budget                                                                                |
| ------------------- | ------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------- |
| Public reads        | 20 requests/second for five minutes across resolver, metadata, versions, dependents, highlights, keywords, and downloads | p95 at most 250 ms; p99 at most 500 ms                                                |
| Search              | Concurrent 5 requests/second for five minutes across browse, identity, text, keyword, namespace, and miss queries        | p95 at most 500 ms; p99 at most 1 second                                              |
| Typical publication | Twenty valid packages of approximately 1 MiB at four per minute                                                          | p95 at most 5 seconds; maximum below 10 seconds                                       |
| Maximum publication | Ten valid packages near 5 MiB at two per minute                                                                          | p95 at most 10 seconds; maximum below 15 seconds                                      |
| PostgreSQL          | Complete launch fixture after `VACUUM (ANALYZE)`                                                                         | At most 2 GiB from `pg_database_size`, including indexes and TOAST                    |
| Frontend            | Three median Lighthouse runs for each representative mobile and desktop route                                            | Performance 90, LCP 2.5 seconds, CLS 0.1, TBT 200 ms, and transferred payload 400 KiB |

The database budget excludes WAL and immutable package objects. The maximum publication budget is a boundary check; the five-second production alert tracks the representative publication objective because runtime metrics intentionally do not label request sizes.

## Deterministic fixture

The non-shipping `rux-performance` tool accepts only a database whose name ends in `_performance` and refuses to seed occupied catalog tables. The launch profile creates 1,000 namespaces, 10,000 packages, 100,000 versions, two authors, three keywords, and three dependencies per version, plus 100,000 publication audit records and 1,000,000 download events spread over two years. Descriptions and README text include controlled exact, full-text, filtered, and missing search cases. Five percent of versions are yanked and the highest version of each package is a prerelease, exercising representative selection.

The smoke profile uses 100 packages, 1,000 versions, and 10,000 downloads. Its 64 MiB database and relaxed latency limits detect broken harness behavior on a shared runner without claiming that runner represents production.

Create a fresh database, migrate it, seed it, generate publication inputs, and check its size from the repository root:

```bash
export DATABASE_URL=postgres://registry:registry@127.0.0.1:5432/registry_performance
cargo run -p rux-performance -- migrate launch
cargo run -p rux-performance -- seed launch > .performance/seed.json
cargo run -p rux-performance -- fixtures launch .performance/fixtures launch-001 \
  > .performance/fixtures-report.json
cargo run -p rux-performance -- size launch > .performance/database-size.json
```

The command's stdout report is credential-free. The private `.performance/fixtures/fixtures.json` manifest contains a credential valid only for the disposable performance database so k6 can publish. Keep `.performance` private even though Git ignores it; never substitute production credentials or copy production data into the fixture database.

## Authoritative prelaunch run

Use a disposable x86-64 Ubuntu 26.04 host with two shared vCPUs and 4 GiB RAM, provisioned with the exact release binary, Caddy settings, PostgreSQL 18 configuration, and a fresh `_performance` database. Generate traffic from a different host so k6 does not consume the target's CPU or memory. The API may raise only its disposable per-client read rate and burst limits to admit the single generator; retain production deadlines, ten-connection pool, upload limits, and publication concurrency.

Run k6 1.5.0 from the external generator:

```bash
docker run --rm --network host \
  -v "$PWD/scripts/performance:/scripts:ro" \
  -v "$PWD/.performance:/work" \
  grafana/k6:1.5.0 run --summary-export /work/api.json \
  -e RUX_PERFORMANCE_PROFILE=launch \
  -e RUX_PERFORMANCE_API_BASE_URL=https://api-performance.example \
  /scripts/api.js

docker run --rm --network host \
  -v "$PWD/scripts/performance:/scripts:ro" \
  -v "$PWD/.performance:/work" \
  grafana/k6:1.5.0 run --summary-export /work/publication-minio.json \
  -e RUX_PERFORMANCE_PROFILE=launch \
  -e RUX_PERFORMANCE_API_BASE_URL=https://api-performance.example \
  -e RUX_PERFORMANCE_FIXTURES=/work/fixtures \
  /scripts/publication.js
```

First run publication against MinIO colocated with the disposable target. Then restart the API with a dedicated, versioned, same-region performance Space, generate a new fixture set with a different run ID, and repeat to `publication-spaces.json`. Both runs must pass. Destroy the database, bucket, objects, and host after retaining the non-secret evidence.

Before public launch, replace the following pending record with the observed values; do not estimate or copy CI smoke numbers:

| Evidence                                                           | Required value            |
| ------------------------------------------------------------------ | ------------------------- |
| Release tag, commit, and archive SHA-256                           | Pending authoritative run |
| Target image, CPU/RAM, region, kernel, PostgreSQL, and k6 versions | Pending authoritative run |
| Read and search p95/p99, failures, and dropped iterations          | Pending authoritative run |
| MinIO and Spaces typical/maximum publication results               | Pending authoritative run |
| Database bytes and budget result                                   | Pending authoritative run |
| SHA-256 digests and restricted location of raw JSON summaries      | Pending authoritative run |

Repeat the authoritative run after changes to search SQL or indexes, publication inspection/storage, connection sizing, or the production host. Frontend Lighthouse budgets remain blocking on every pull request.
