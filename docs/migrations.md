# Database migration workflow

PostgreSQL migrations are reviewed, reversible SQLx migrations stored in the repository-level `migrations/` directory. Applying them is a deliberate operator or test action. The API must never change the database schema at startup.

## Tooling and connection

Use the SQLx CLI release that matches the workspace's SQLx dependency. The current lockfile resolves SQLx 0.8.6:

```powershell
cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features rustls,postgres
```

SQLx CLI reads `DATABASE_URL`, while the API reads `RUX_DATABASE_URL`. For the local Compose database, import the repository environment and copy the value for the CLI process:

```powershell
.\Import-LocalEnv.ps1
$env:DATABASE_URL = $env:RUX_DATABASE_URL
```

Do not commit a database URL or put a production credential directly in a command. Production automation must provide `DATABASE_URL` through its secret environment.

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

The production [release playbook](releases.md) bundles SQLx CLI 0.8.6 and the exact tagged migration directory. It requires an explicit migration-review confirmation, records migration state before and after `run`, and performs the operation before changing the active application symlink.

## Rollback and recovery policy

`sqlx migrate revert` is limited to local or otherwise unreleased disposable databases. Once a migration reaches a shared staging or production database, its files and recorded history are immutable.

Recover a production release in this order:

1. Roll back the application release only when the migrated schema remains compatible with the previous application.
2. Correct schema or data defects with a new reviewed forward migration.
3. Restore the database through the recovery procedure only when a forward correction cannot recover the required data.

Down migrations remain mandatory because they improve review quality and test local reversibility. They are not the normal production recovery mechanism. Before applying a destructive or data-moving production migration, confirm that the release has an applicable backup and recovery path.
