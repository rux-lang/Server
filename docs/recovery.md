# Production recovery

The registry targets a five-minute recovery point objective (RPO) and a four-hour recovery time objective (RTO). The RPO is the maximum acknowledged PostgreSQL history that may be absent from the off-host repository. The RTO is measured from incident declaration until the restored API passes internal and public readiness checks. These are operational objectives, not provider guarantees.

Everything here is run by hand over SSH. See [deployment.md](deployment.md) for how the host is built.

## Backup boundary

pgBackRest archives WAL to an AES-256-encrypted, versioned DigitalOcean Space in a region other than the package Space and the production droplet. Configure PostgreSQL to force a WAL switch after 300 seconds of activity, which is what bounds the RPO during quiet periods.

Run a full backup weekly and a differential daily, with an end-to-end repository check daily. Install them as systemd timers so a missed run is caught up rather than skipped:

```bash
sudo -u postgres pgbackrest --stanza=registry backup --type=full
sudo -u postgres pgbackrest --stanza=registry backup --type=diff
sudo -u postgres pgbackrest --stanza=registry check
```

Retain five full backups, fourteen differential backups, and the WAL required by the five retained fulls. That normally provides at least four weeks of point-in-time history. The Space must have versioning enabled and a lifecycle that keeps noncurrent versions and delete markers for 90 days. Retention is capacity management; versioning is protection against an accidental or compromised delete.

Create the backup Space before provisioning. Scope its credentials to that Space alone, including the list, read, write, and delete operations pgBackRest needs for retention, and do not reuse the API's package credentials. The cipher passphrase is required to restore every backup — keep an independent copy in the operator secret manager. Rotating repository credentials does not re-encrypt stored backups. Changing the cipher passphrase starts a new repository and requires a fresh full backup, so retain the old passphrase until its repository expires.

## Monitoring and response

Backup state is inspected directly rather than scraped:

```bash
sudo -u postgres pgbackrest --stanza=registry info
sudo -u postgres pgbackrest --stanza=registry check
systemctl list-timers 'pgbackrest-*'
journalctl -u 'pgbackrest-*' --since '24 hours ago'
```

`pgbackrest info` reports the age of the newest backup, which is the number to check first. The daily `check` forces a WAL switch and verifies that the resulting segment reaches the repository, so it fails loudly when archiving has silently stopped.

Watch the archive queue as well — PostgreSQL retains unarchived segments rather than silently losing the recovery chain, so a stuck archiver eventually fills the disk:

```bash
sudo -u postgres psql -c 'SELECT * FROM pg_stat_archiver;'
ls /var/lib/postgresql/18/main/pg_wal/archive_status/*.ready | wc -l
```

Escalate immediately if that count grows. Preserve the journal, confirm available disk space, provider health, credentials, TLS, and repository version history, then rerun only the failed operation.

## PostgreSQL disaster recovery

Stop write traffic and record the incident time before choosing a recovery target. Preserve the failed host and repository: do not initialize pgBackRest against a new empty repository, and do not delete damaged data.

Restore onto a fresh Ubuntu 26.04 host with PostgreSQL 18 installed and stopped, using credentials with read access to the backup Space. Restore the latest state, or pass a UTC target in pgBackRest's documented time format:

```bash
sudo -u postgres pgbackrest --stanza=registry --delta restore
```

```bash
sudo -u postgres pgbackrest --stanza=registry --delta --type=time --target='YYYY-MM-DD HH:MM:SS+00' restore
```

Restore with archiving disabled, start the cluster, and check the data before accepting the target:

```sql
SELECT count(*) FROM _sqlx_migrations WHERE success = false;
SELECT count(*) FROM package_versions pv
  LEFT JOIN packages p ON p.id = pv.package_id WHERE p.id IS NULL;
SELECT max(published_at) FROM package_versions;
```

Zero failed migrations and zero orphaned versions are the pass condition; the newest publication timestamp tells you how much history the target actually contains. Query representative users, namespaces, packages, versions, audit records, and downloads too.

For a real incident, build the recovered host by following [deployment.md](deployment.md), re-enable writable backup credentials and WAL archival, install a schema-compatible build, and run the deploy verification. Rotate credentials if compromise is suspected. Change DNS only after internal readiness and package checksum checks pass. Keep the prior host isolated until the incident owner accepts the recovery.

## Quarterly rehearsal

Once per calendar quarter, restore PostgreSQL onto an empty disposable host and record the elapsed time against the four-hour RTO. Use a target before the latest WAL at least annually to exercise point-in-time recovery.

Rehearse object storage separately. Create a versioned drill Space with a 90-day cleanup lifecycle, using read-only credentials for the production package Space and read/write/delete credentials limited to the drill Space. Select a published object and its expected values:

```sql
SELECT storage_key, encode(artifact_sha256, 'hex') AS sha256, artifact_size
FROM package_versions
ORDER BY published_at DESC
LIMIT 1;
```

Download that exact object version from production, verify its size and SHA-256 against those values, copy it into the drill Space, create a delete marker there, restore the prior drill version, and verify the current object. Never write to or delete from the production Space.

Record both rehearsals in the restricted operations record with the date, operators, incident or change reference, elapsed time, and remediation for any failed attempt. A rehearsal is complete only when both targets pass; a failed or over-RTO rehearsal creates follow-up work and must be repeated after repair.
