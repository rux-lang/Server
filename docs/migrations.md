# Database migration workflow

PostgreSQL migrations are reviewed, reversible SQLx migrations stored in the repository-level `migrations/` directory. Applying them is a deliberate operator or test action. The API must never change the database schema at startup.

## Tooling and connection

Use the SQLx CLI release that matches the workspace's SQLx dependency. The current lockfile resolves SQLx 0.8.6:

```powershell
cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features rustls,postgres
```

The SQLx CLI reads `DATABASE_URL` from the environment, while the API reads `database.url` from its configuration file. The two are deliberately separate: migrations are an operator step, not something the API does at startup, so the credential that can change the schema is not the one the service runs with. Set it for the CLI process from the same value your `config/config.toml` carries:

```powershell
$env:DATABASE_URL = "postgres://registry:registry@localhost:5432/registry"
```

Do not commit a database URL or put a production credential directly in a command. In production, read `DATABASE_URL` out of the root-owned `/etc/rux/config.toml` rather than typing it; that file is readable only by root, which is the intent.

## Create a migration

Start from the latest reviewed migrations, apply them locally, then create one reversible migration from the repository root:

```powershell
sqlx migrate run
sqlx migrate add -r add_package_search_columns
```

Use a short `snake_case` name for one logical schema change. SQLx creates a timestamped `.up.sql` and `.down.sql` pair. The up file advances the schema; the down file reverses that migration in dependency-safe order.

Both files must be complete before review. Include data movement with the schema change that requires it, and make every backfill deterministic and repeatable within a failed-and-retried deployment. If reversing a local migration necessarily discards data, make that consequence obvious in the down SQL and the pull request.

Never rename, reorder, delete, or edit a migration after it has been applied to a shared database. SQLx validates applied migration checksums against the repository. Correct an applied migration with a new forward migration instead of changing its history.

## Review a migration

Migration review covers both SQL files and their operational effect. Confirm that:

- the change is one coherent unit and follows the database contracts;
- constraints, indexes, foreign keys, and generated values have explicit names and the intended deletion behavior;
- backfills handle existing rows and preserve required data;
- statements have acceptable lock duration and resource use at expected scale;
- the schema remains compatible with both the outgoing and incoming API releases;
- the down migration restores the prior schema on a disposable database, with any unavoidable data loss called out;
- focused migration tests cover new invariants and failure cases.

Use expand-and-contract changes when one release cannot safely make the entire transition: add a backward-compatible shape first, deploy code that can use it, and remove the retired shape only in a later migration after the old code can no longer run. A migration that requires downtime must say so in its pull request and release instructions.

## Verify locally

Use a disposable PostgreSQL database whose configured user can create test databases. Inspect the state before and after applying migrations:

```powershell
sqlx migrate info
sqlx migrate run
sqlx migrate info
```

On that disposable database only, validate the down file and reapplication:

```powershell
sqlx migrate revert
sqlx migrate run
cargo test -p rux-infrastructure --test migrations
```

`sqlx migrate revert` reverts only the latest applied migration. Never use the local rollback exercise against a database containing data that must be kept. The infrastructure migration suite independently creates isolated databases and verifies rollback and clean reapplication.

## Apply a reviewed migration

Applying migrations is a separate release step performed before starting the new API release. From the exact reviewed release checkout, with `DATABASE_URL` provided securely:

```powershell
sqlx migrate info
sqlx migrate run
sqlx migrate info
```

The first status check identifies the pending set. `run` applies it and rejects checksum drift in previously applied migrations. The final status check records that the intended set is current. Stop and investigate any unexpected pending, missing, or mismatched migration; do not bypass the migration history.

Production API code must not run an embedded or filesystem migrator as part of startup or otherwise write SQLx migration state. If the schema is not suitable for a release, startup or readiness may fail, but the process must not repair or advance the schema automatically.

In production this is a deliberate operator step run over SSH with the same pinned SQLx CLI. Review the pending set with `sqlx migrate info` before applying anything, and apply it before starting the new binaries. See [deployment.md](deployment.md).

## Rollback and recovery policy

`sqlx migrate revert` is limited to local or otherwise unreleased disposable databases. Once a migration reaches a shared staging or production database, its files and recorded history are immutable.

Recover a production release in this order:

1. Roll back the application release only when the migrated schema remains compatible with the previous application.
2. Correct schema or data defects with a new reviewed forward migration.
3. Restore the database through the recovery procedure only when a forward correction cannot recover the required data.

Down migrations remain mandatory because they improve review quality and test local reversibility. They are not the normal production recovery mechanism. Before applying a destructive or data-moving production migration, confirm that the release has an applicable backup and recovery path.
