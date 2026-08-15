# Production deployment

The registry runs on one hand-configured Ubuntu droplet: PostgreSQL and the API on the host, package artifacts in DigitalOcean Spaces, and Caddy terminating TLS in front. The playground is opt-in and is the only reason Docker is installed at all. Nothing in this document is automated — every step is an operator action, deliberately, so that a change to production is something a person decided to make.

Read [architecture.md](architecture.md) for what the components are and [migrations.md](migrations.md) for why the API never migrates its own schema.

## Host baseline

Use Ubuntu 26.04 LTS on x86-64. Create an administrator account with your SSH key, disable password and root SSH login, and enable unattended security upgrades. Bound journald so logs cannot fill the disk.

The firewall accepts 22, 80, and 443 and drops everything else, including forwarded traffic:

```bash
sudo nft add table inet filter
sudo nft 'add chain inet filter forward { type filter hook forward priority 0; policy drop; }'
```

The forward policy matters for the playground: it is the outer guarantee that a sandbox container reaches no network even if a container flag is ever wrong.

## Service accounts

Two system users, no login shells:

```bash
sudo useradd --system --home-dir /srv/rux-server --shell /usr/sbin/nologin rux-server
sudo useradd --system --uid 1000 --home-dir /var/lib/rux-playground --shell /usr/sbin/nologin rux-playground
```

`rux-playground` must be uid 1000 to match the `play` user baked into the sandbox image — the broker mounts a `0700` job directory into the container and passes `--user` with the host uid that owns it. Only `rux-playground` joins the `docker` group; `rux-server` never does. The API reaches the broker through a shared group instead:

```bash
sudo usermod --append --groups docker rux-playground
sudo usermod --append --groups rux-playground rux-server
```

That split is the whole point of the two-binary design. Docker-socket access is root-equivalent, and the registry database is on this host.

## PostgreSQL

Install PostgreSQL 18 from the Ubuntu archive. Create a least-privileged role and the registry database:

```bash
sudo -u postgres createuser --pwprompt rux_server
sudo -u postgres createdb --owner rux_server registry
```

Restrict `pg_hba.conf` to `scram-sha-256` over loopback only, and keep the cluster listening on `127.0.0.1`. The `pg_trgm` extension is created by the initial migration, so no manual extension step is needed.

## Object storage

Create a DigitalOcean Space for package artifacts with versioning enabled. Object keys are immutable (`packages/{namespace}/{package}/{version}/{sha256}.ruxpkg`), and the orphan sweep deletes exact object versions, so versioning is required rather than optional. Issue credentials scoped to that Space alone — the backup Space in [recovery.md](recovery.md) gets its own.

## Configuration

Configuration is environment-only and `RUX_`-prefixed; every setting is parsed and validated in `src/config.rs` and listed in [.env.example](../.env.example). Production values live in root-owned files that the units read:

```bash
sudo install -d -m 0750 -o root -g rux-server /etc/rux-server
sudo install -m 0640 -o root -g rux-server /dev/null /etc/rux-server/api.env
```

At minimum `api.env` sets `RUX_BIND_ADDRESS=127.0.0.1:8080`, `RUX_DATABASE_URL`, the six `RUX_STORAGE_*` values, `RUX_PACKAGE_CDN_BASE_URL`, `RUX_ALLOWED_WEB_ORIGIN`, `RUX_WEB_CALLBACK_URL`, the three `RUX_GITHUB_*` OAuth values, and `RUST_LOG`. The browser origin and the callback are configured independently on purpose. Secrets belong only in these files; they are never committed and never logged.

## The API service

Install the binaries under `/srv/rux-server/bin/` owned by root, and write `/etc/systemd/system/rux-server.service`:

```ini
[Unit]
Description=Rux Server
After=network-online.target postgresql.service
Wants=network-online.target
Requires=postgresql.service
ConditionFileIsExecutable=/srv/rux-server/bin/rux-server

[Service]
Type=simple
User=rux-server
Group=rux-server
WorkingDirectory=/srv/rux-server
EnvironmentFile=/etc/rux-server/api.env
ExecStart=/srv/rux-server/bin/rux-server
Restart=on-failure
RestartSec=5s
TimeoutStartSec=30s
TimeoutStopSec=30s
UMask=0077
LimitNOFILE=65536

NoNewPrivileges=true
PrivateDevices=true
PrivateTmp=true
ProtectClock=true
ProtectControlGroups=true
ProtectHome=true
ProtectHostname=true
ProtectKernelLogs=true
ProtectKernelModules=true
ProtectKernelTunables=true
ProtectSystem=strict
# Connecting to a unix socket counts as a write, so the playground's runtime
# directory has to be writable for the registry to reach the broker. The leading
# dash keeps the unit startable when the playground is not installed at all,
# which is the default.
ReadWritePaths=/var/lib/rux-server/uploads -/run/rux-playground
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
RestrictNamespaces=true
RestrictRealtime=true
LockPersonality=true
MemoryDenyWriteExecute=true
CapabilityBoundingSet=
AmbientCapabilities=
SystemCallArchitectures=native

[Install]
WantedBy=multi-user.target
```

Create `/var/lib/rux-server/uploads` owned by `rux-server` before starting; publication streams bounded multipart uploads through it.

## Caddy

Install Caddy and use this site block. It refuses the operational routes publicly — readiness exposes dependency names, and nothing outside the host has any reason to reach it:

```caddyfile
{$RUX_API_ADDRESS:api.rux-lang.dev} {
	encode zstd gzip

	header {
		-Server
		Strict-Transport-Security "max-age=31536000"
		X-Content-Type-Options "nosniff"
	}

	@operational path /health /health/*
	respond @operational 404

	reverse_proxy {$RUX_API_UPSTREAM:127.0.0.1:8080}
}
```

Keep `RUX_TRUSTED_PROXY_CIDRS` at loopback so client-IP resolution trusts only this proxy; see [abuse-controls.md](abuse-controls.md).

## The playground (optional)

Skip this whole section unless you want the playground. With `RUX_PLAYGROUND_ENABLED` false the routes are never mounted and both endpoints answer 404 from the fallback, so a host without Docker is a fully working registry.

Install Docker CE and pin it. The daemon needs no container networking, because every run starts with `--network=none` — write `/etc/docker/daemon.json`:

```json
{
  "bridge": "none",
  "iptables": false,
  "ip6tables": false,
  "live-restore": true,
  "no-new-privileges": true,
  "log-driver": "json-file",
  "log-opts": { "max-size": "10m", "max-file": "3" },
  "default-ulimits": {
    "nofile": { "Name": "nofile", "Hard": 512, "Soft": 512 }
  }
}
```

With `bridge` none there is no `docker0` and nothing to forward; with `iptables` false Docker writes no firewall rules, so it cannot clobber the nftables policy above. That is a stronger guarantee than ordering the two services carefully, but order `docker.service` after `nftables.service` anyway. `userns-remap` is deliberately absent: a remapped user namespace would shift the `0700` job directory out from under the bind mount.

Build the sandbox image on the host with the pinned compiler version and checksum from [playground.md](playground.md):

```bash
bash playground/build-image.sh 0.3.0 82e654f9ced042dc029220836d1322b208790099627f32efd9d8d600834be5cc
```

Write `/etc/rux-playground/playground.env` (root-owned, mode `0640`, group `rux-playground`) with `RUX_PLAYGROUND_SOCKET`, `RUX_PLAYGROUND_IMAGE`, `RUX_PLAYGROUND_JOBS_ROOT`, `RUX_PLAYGROUND_DOCKER_BINARY=/usr/bin/docker`, the limit knobs, and `RUST_LOG`. Every knob is range-checked at broker startup, so a typo fails fast rather than loosening the sandbox. Then `/etc/systemd/system/rux-playground.service`:

```ini
[Unit]
Description=Rux Playground sandbox broker
After=network-online.target docker.service
Wants=network-online.target
Requires=docker.service
ConditionFileIsExecutable=/srv/rux-server/bin/rux-playgroundd

[Service]
Type=simple
User=rux-playground
Group=rux-playground
SupplementaryGroups=docker
WorkingDirectory=/var/lib/rux-playground
EnvironmentFile=/etc/rux-playground/playground.env
ExecStart=/srv/rux-server/bin/rux-playgroundd
Restart=on-failure
RestartSec=5s
TimeoutStartSec=30s
TimeoutStopSec=30s
UMask=0077
LimitNOFILE=65536

NoNewPrivileges=true
PrivateDevices=true
PrivateTmp=true
ProtectClock=true
ProtectControlGroups=true
ProtectHome=true
ProtectHostname=true
ProtectKernelLogs=true
ProtectKernelModules=true
ProtectKernelTunables=true
ProtectSystem=strict
ReadWritePaths=/var/lib/rux-playground /run/rux-playground /run/docker.sock
RestrictAddressFamilies=AF_UNIX
RestrictRealtime=true
LockPersonality=true
SystemCallArchitectures=native
CapabilityBoundingSet=
AmbientCapabilities=

[Install]
WantedBy=multi-user.target
```

This unit is deliberately weaker than the API's, and the difference is worth understanding rather than closing. Anything that can talk to the Docker socket can already ask the daemon to do root's work, so hardening the broker is not what contains a compromise — the container flag set the sandbox passes is (`--network=none`, read-only root, dropped capabilities, pid and memory limits), and that lives in `crates/sandbox` where it is unit-tested. Two flags that would fight the Docker CLI are omitted rather than pretended: `RestrictNamespaces` is absent because the CLI's exec path into the daemon is not audited against it, and `ProtectSystem=strict` is kept only because `/run/docker.sock` is listed read-write, since connecting to a unix socket counts as a write.

Add a `tmpfiles.d` entry so `/run/rux-playground` is recreated on boot, owned by `rux-playground:rux-playground` at mode `0750`. Start the broker before the API so the socket exists on the API's first request.

## Deploying a new version

Build the release artifacts and copy them up:

```bash
cargo build --workspace --release
```

The API does not migrate its own schema, so migrations are a separate, deliberate step. Review what is pending before applying anything:

```bash
sqlx migrate info
```

Then, on the host: stop the services, keep the outgoing binaries as `.previous` alongside the new ones, install the new binaries, run `sqlx migrate run`, and start the services back up. Verify before considering the deploy done:

```bash
curl --fail --silent http://127.0.0.1:8080/health/ready
```

Confirm both `postgresql` and `object_storage` report healthy, check the public API answers through Caddy, and read the first minute of `journalctl -u rux-server` for anything unexpected.

## Rolling back

Restore the `.previous` binaries and restart. **Migrations are never reverted in production.** A rollback is only safe if the previous binary is compatible with the schema now in the database — decide that before restarting, not after. If it is not, roll forward with a new migration instead; see [migrations.md](migrations.md).

## Operating

Logs are structured JSON on stdout, captured by journald. There is no metrics endpoint and no separate observability stack; `journalctl` is the interface.

```bash
journalctl -u rux-server -f
journalctl -u rux-server --since '1 hour ago' | jq 'select(.level == "ERROR")'
```

Each HTTP request logs its `request_id`, method, route template, status, and duration. Route templates rather than concrete paths are logged on purpose, so a package name or token never lands in a log line. The orphan cleanup sweep logs its counts once per run, and playground runs log mode, outcome, and duration — never submitted source.

Backups, restores, and rehearsals are in [recovery.md](recovery.md).
