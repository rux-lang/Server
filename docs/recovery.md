# Production recovery

The registry targets a five-minute recovery point objective (RPO) and a four-hour recovery time objective (RTO). The RPO is the maximum acknowledged PostgreSQL history that may be absent from the off-host repository. The RTO is measured from incident declaration until the restored API passes internal and public readiness checks. These are operational objectives, not provider guarantees.

## Backup boundary

pgBackRest archives WAL synchronously to an AES-256-encrypted, versioned DigitalOcean Space in a region other than the package Space and production droplet. PostgreSQL forces a WAL switch after 300 seconds of activity. A full backup runs Sundays at 02:00 UTC, differential backups run Monday through Saturday at 02:00 UTC, and an end-to-end repository check runs daily at 04:00 UTC. Timers persist missed runs and add up to fifteen minutes of randomized delay.

The repository retains five full backups, fourteen differential backups, and the WAL required by the five retained full backups. This normally provides at least four weeks of point-in-time history. The Space must have versioning enabled and a lifecycle that retains noncurrent versions and delete markers for 90 days. Retention is capacity management; versioning is protection against an accidental or compromised delete.

Create the backup Space before provisioning. Give its credentials access only to that Space, including the list, read, write, and delete operations pgBackRest needs for retention. Do not reuse the API's package credentials. Supply these secrets through the same untracked or Ansible Vault input used by production:

```yaml
rux_backup_access_key: replace
rux_backup_secret_key: replace
rux_backup_cipher_passphrase: replace-with-at-least-32-random-characters
```

The cipher passphrase is required to restore every backup. Keep an independent copy in the operator secret manager. Rotating repository credentials does not re-encrypt stored backups; update the secure Ansible input and converge the host. Changing the cipher passphrase starts a new repository and requires a fresh full backup, so retain the old passphrase until its repository expires.

## Monitoring and response

Prometheus scrapes recovery metrics from the loopback-only node exporter. The dashboard shows backup age, operation status, and the oldest pending WAL. Critical alerts cover failed or stale backups, archive failures, and WAL queued beyond the RPO. The daily `pgbackrest check` forces a WAL switch and verifies that the resulting segment reaches the repository. Inspect backup state as the `postgres` user:

```bash
sudo -u postgres pgbackrest --stanza=registry info
sudo -u postgres pgbackrest --stanza=registry check
systemctl list-timers 'rux-pgbackrest-*'
journalctl -u 'rux-pgbackrest-*' --since '24 hours ago'
```

Do not clear a backup alert by editing textfile metrics. Preserve the journal, confirm available disk space, provider health, credentials, TLS, and repository version history, then rerun only the failed operation. Escalate immediately if WAL accumulates under `pg_wal`; PostgreSQL retains unarchived segments to avoid silently losing the recovery chain.

## PostgreSQL disaster recovery

Stop write traffic and record the incident time before choosing a recovery target. Preserve the failed host and repository; do not initialize pgBackRest against a new empty repository or delete damaged data. Prefer a new Ubuntu 26.04 host. Bootstrap it with the exact file `/etc/rux-disposable-recovery-host`, place it alone in an Ansible `recovery` group, and give the rehearsal credentials read-only access to the backup Space.

Restore the latest state, or pass a UTC target in pgBackRest's documented time format:

```bash
ANSIBLE_CONFIG=deploy/ansible/ansible.cfg ansible-playbook \
  -i /secure/recovery-inventory.yml \
  deploy/ansible/rehearse-postgresql.yml \
  --extra-vars @/secure/recovery-secrets.yml \
  --extra-vars rux_recovery_confirm_disposable=true \
  --extra-vars rux_recovery_evidence_directory=/secure/recovery-rehearsals
```

The playbook refuses an unmarked host, removes only the standard PostgreSQL 18 data directory on that disposable host, restores with archiving disabled, and checks migrations, package references, and artifact checksums. For a point-in-time restore add `--extra-vars 'rux_recovery_target_time=YYYY-MM-DD HH:MM:SS+00'`. Review the fetched JSON evidence and query representative users, namespaces, packages, versions, audit records, and downloads before accepting the target.

For a real incident, provision the recovered host with the normal production playbook, re-enable writable backup credentials and WAL archival, install the schema-compatible application release, and perform the release health checks. Rotate credentials if compromise is suspected. Change DNS only after internal readiness and package checksum checks pass. Keep the prior host isolated until the incident owner accepts the recovery. The rehearsal playbook is never an in-place production restore tool.

## Quarterly rehearsal

Once per calendar quarter, restore PostgreSQL from an empty disposable host and record the elapsed time against the four-hour RTO. Use a target before the latest WAL at least annually to exercise point-in-time recovery.

For Spaces, create a separate versioned drill Space with a 90-day cleanup lifecycle. Use read-only credentials for the production package Space and read/write/delete credentials limited to the drill Space. Select a published object and its expected values without exposing internal identifiers publicly:

```sql
SELECT storage_key, encode(artifact_sha256, 'hex') AS sha256, artifact_size
FROM package_versions
ORDER BY published_at DESC
LIMIT 1;
```

Load the selected values and credentials from a mode `0600` environment file, then run:

```bash
set -a
. /secure/spaces-recovery.env
set +a
bash deploy/recovery/rehearse-spaces.sh
```

The script lists every required `RUX_SPACES_*` or evidence variable if one is missing. It downloads an explicit production version, checks its size and SHA-256, copies it into the drill Space, creates a delete marker there, restores the prior drill version, and verifies the current object. It never writes to or deletes from the production Space.

Retain both JSON evidence files in the restricted operations record with the date, operators, incident or change reference, elapsed time, and remediation for any failed attempt. Only after reviewing successful evidence, update the production cadence metrics:

```bash
sudo rux-record-recovery-rehearsal postgresql
sudo rux-record-recovery-rehearsal spaces
```

A rehearsal is complete only when both targets pass. Prometheus warns when either success timestamp is absent or older than 100 days. A failed or over-RTO rehearsal creates follow-up work and must be repeated after repair.
