# Rux Server

[![CI](https://github.com/rux-lang/Server/actions/workflows/ci.yml/badge.svg)](https://github.com/rux-lang/Server/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/rux-lang/Server?style=flat)](LICENSE.md)

The Rust server for the Rux programming language. It hosts the package registry and the playground; language tooling and chat integrations are planned alongside them. The public API is hosted at <https://api.rux-lang.dev>; the package catalog is part of <https://rux-lang.dev/packages>.

## Structure

- `Run.ps1`: every development command in one script; run it with no arguments for full help.
- `src/`: Axum composition root and HTTP contract, plus the `rux-playgroundd` binary.
- `config/`: the commented local-development configuration both binaries read.
- `crates/`: domain, manifest, artifact, application, infrastructure, and sandbox layers.
- `migrations/`: reviewed SQLx migrations.
- `playground/`: the sandbox container image and its containment tests.
- `docs/`: API, persistence, security, and operations contracts.

Dependencies point inward: `domain <- application <- infrastructure <- server`, with `domain <- manifest <- artifact` for package inspection and `domain <- sandbox` for the playground.

The playground runs submitted code in a throwaway container, and is the only part of this project that uses Docker at all. Because Docker-socket access is root-equivalent and the registry shares the host, it runs as a second binary under a second user, reached over a unix socket; the API itself never touches a container runtime. It is disabled by default, so a host without Docker is a fully working registry. See [docs/playground.md](docs/playground.md).

## Development

Requires a local PostgreSQL 18 and a local MinIO with a versioned `packages` bucket, both installed natively and reachable at the addresses in [config/config.toml](config/config.toml).

[Run.ps1](Run.ps1) collects every development command. Run it with no arguments for full help; `doctor` reports which of the two services is missing and how to start it.

```powershell
./Run.ps1 doctor          # toolchain, PostgreSQL, MinIO, and configuration
./Run.ps1 migrate         # apply pending migrations
./Run.ps1 dev             # start the API
```

It echoes each invocation before making it, so the same thing by hand is:

```powershell
$env:DATABASE_URL = "postgres://registry:registry@localhost:5432/registry"
sqlx migrate run
cargo run -p rux-server
```

Configuration is one TOML file. Both binaries read it, taking its path from `--config` and defaulting to the committed [config/config.toml](config/config.toml), which is why a fresh checkout runs with no arguments. That file is for local development only — every value in it names a local service or is a placeholder, and production reads its own root-owned `/etc/rux/config.toml` instead. `DATABASE_URL` above is separate because it belongs to the SQLx CLI, not to the server.

Keep your own credentials out of the repository: `./Run.ps1 config init` copies the committed file to `%APPDATA%\rux\config.toml`, and every command prefers that copy once it exists. It is a whole configuration rather than an overlay, because the server does no layering and refuses unknown keys.

The local API listens on <http://localhost:8080>. Liveness, readiness, and OpenAPI are available at `/health/live`, `/health/ready`, and `/openapi/v1.json`. Logs are structured JSON on stdout; there is no metrics endpoint.

## Quality gates

All four, in this order, are what CI runs; `./Run.ps1 check` runs them in one step and stops at the first failure.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --workspace --release
```

Production secrets belong in the root-owned `/etc/rux/config.toml` and must never be committed. The browser origin and callback are configured independently with `web.allowed_origin` and `web.callback_url`.

## Contributing

Pull requests should target `dev`, not `main`. CI rejects pull requests opened against `main`.

## License

Licensed under the [MIT License](LICENSE.md).
