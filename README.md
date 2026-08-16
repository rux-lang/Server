# Rux Server

[![CI](https://github.com/rux-lang/Server/actions/workflows/ci.yml/badge.svg)](https://github.com/rux-lang/Server/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/rux-lang/Server?style=flat)](LICENSE.md)

The Rust server for the Rux programming language. It hosts the package registry and the playground; language tooling and chat integrations are planned alongside them. The public API is hosted at <https://api.rux-lang.dev>; the package catalog is part of <https://rux-lang.dev/packages>.

## Structure

- `src/`: Axum composition root and HTTP contract, plus the `rux-playgroundd` binary.
- `crates/`: domain, manifest, artifact, application, infrastructure, and sandbox layers.
- `migrations/`: reviewed SQLx migrations.
- `playground/`: the sandbox container image and its containment tests.
- `docs/`: API, persistence, security, and operations contracts.

Dependencies point inward: `domain <- application <- infrastructure <- server`, with `domain <- manifest <- artifact` for package inspection and `domain <- sandbox` for the playground.

The playground runs submitted code in a throwaway container, and is the only part of this project that uses Docker at all. Because Docker-socket access is root-equivalent and the registry shares the host, it runs as a second binary under a second user, reached over a unix socket; the API itself never touches a container runtime. It is disabled by default, so a host without Docker is a fully working registry. See [docs/playground.md](docs/playground.md).

## Development

Requires a local PostgreSQL 18 and a local MinIO with a versioned `packages` bucket, both installed natively and reachable at the addresses in `.env.example`.

```powershell
Copy-Item .env.example .env
.\Import-LocalEnv.ps1
$env:DATABASE_URL = $env:RUX_DATABASE_URL
sqlx migrate run
cargo run -p rux-server
```

The local API listens on <http://localhost:8080>. Liveness, readiness, and OpenAPI are available at `/health/live`, `/health/ready`, and `/openapi/v1.json`. Logs are structured JSON on stdout; there is no metrics endpoint.

## Quality gates

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release
```

Production secrets belong in root-owned API environment files and must never be committed. The browser origin and callback are configured independently with `RUX_ALLOWED_WEB_ORIGIN` and `RUX_WEB_CALLBACK_URL`.

## Contributing

Pull requests should target `dev`, not `main`. CI rejects pull requests opened against `main`.

## License

Licensed under the [MIT License](LICENSE.md).
