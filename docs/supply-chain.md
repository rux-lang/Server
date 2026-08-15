# Supply-chain policy

## Dependencies and vulnerabilities

Pull requests use GitHub Dependency Review to reject newly introduced high or critical advisories. The committed Cargo lockfile is audited with the pinned `cargo-audit` version:

```bash
cargo install cargo-audit --version 0.22.2 --locked
cargo audit --file Cargo.lock --json > cargo-audit.json
node scripts/supply-chain/check-cargo-audit.mjs cargo-audit.json
```

RustSec findings marked unmaintained or unsound are reviewed explicitly; security vulnerabilities fail the gate. Dependabot opens weekly grouped compatible Cargo and GitHub Actions updates.

## Licenses and sources

`cargo-deny` checks the complete Rust graph, including development dependencies, and accepts crates only from crates.io:

```bash
cargo deny --all-features check licenses sources
```

The allowlist covers approved permissive, attribution, data, and MPL-2.0 licenses. Unknown, malformed, source-available, and strong-copyleft expressions fail unless an explicit reviewed exception is added.

## Secrets

Gitleaks scans the complete reachable history with redacted output on every push and pull request, and weekly on a schedule. `.gitleaks.toml` allowlists only the synthetic values that appear in documentation and tests.
