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

Configuration is one TOML file, parsed and validated in `src/config.rs` and documented key by key in [config/config.toml](../config/config.toml), the committed local-development copy. Both binaries read it, and each is given its path with `--config`; without that argument they look for `config/config.toml` relative to the working directory. Which process a setting belongs to is a property of its key path: `[playground.api]` is the registry's, `[playground.broker]` is the daemon's, and `playground.socket` is the one value both read, so the two cannot disagree about where the socket is.

An unknown key is a startup error rather than a silent default, and every bound is checked before the process serves anything, so a typo fails loudly instead of quietly loosening a limit. Config is read before logging is installed, so those failures arrive on stderr with a line and column rather than in the JSON log.

Install one root-owned file and hand each unit a private copy through systemd's credential mechanism:

```bash
sudo install -d -m 0755 -o root -g root /etc/rux
sudo install -m 0400 -o root -g root /dev/null /etc/rux/config.toml
sudoedit /etc/rux/config.toml
```

At minimum it sets `database.url`, the four required `[storage]` values, `packages.cdn_base_url`, `[web]`, and the two required `[github]` OAuth values. The browser origin and the callback are configured independently on purpose. Secrets belong only in this file; it is never committed and never logged.

`LoadCredential` is what makes one file readable by two services running as different users without widening it: each unit receives a copy on tmpfs at `%d`, mode `0400`, owned by that unit's user and inside that unit's mount namespace, destroyed when the service stops. Neither service can see the other's copy, and the file on disk stays unreadable to everything but root — which a shared group would not achieve, because a group is a standing grant anyone can later be added to. On a host without systemd 247 or later, fall back to a `rux-config` group holding both service users with the file at `0640 root:rux-config`. Do not reach for POSIX ACLs instead: they carry the same exposure and are silently dropped by `cp`, `install`, and most restore paths, so the protection would depend on an attribute a recovery can erase. Never `0644`.

`RUST_LOG` is the one setting that stays on the environment — it belongs to the logging library rather than to us — so each unit sets it with `Environment=`.

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
LoadCredential=config:/etc/rux/config.toml
Environment=RUST_LOG=info
ExecStart=/srv/rux-server/bin/rux-server --config %d/config
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

Install Caddy and use this site block. It refuses the operational routes publicly — readiness exposes dependency names, and nothing outside the host has any reason to reach it. The two `{$RUX_…}` placeholders below are Caddy's own environment substitution, not the server's configuration; they are the only `RUX_` names left on this host:

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

Keep `abuse.trusted_proxy_cidrs` at loopback so client-IP resolution trusts only this proxy; see [abuse-controls.md](abuse-controls.md).

## The playground (optional)

Skip this whole section unless you want the playground. With `playground.api.enabled` false the routes are never mounted and both endpoints answer 404 from the fallback, so a host without Docker is a fully working registry.

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

The broker reads the same `/etc/rux/config.toml` the API does, under `[playground.broker]`: `image` is required, and `jobs_root`, `docker_binary`, `packages`, `max_concurrency`, `request_timeout_seconds`, and the four `[playground.broker.limits]` knobs all default. Every one is range-checked at broker startup and an unrecognised key is refused outright, so a typo fails fast rather than loosening the sandbox. Note that `playground.socket` is shared with the API and belongs in `[playground]`, not here. Then `/etc/systemd/system/rux-playground.service`:

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
LoadCredential=config:/etc/rux/config.toml
Environment=RUST_LOG=info
ExecStart=/srv/rux-server/bin/rux-playgroundd --config %d/config
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

One consequence of a single configuration file is worth stating rather than discovering: `rux-playgroundd` can read the registry's database password and GitHub client secret, because its credential copy is the whole document. Under the two-file arrangement this replaced, it could not. This is a deliberate trade and an acceptable one, because the broker is in the `docker` group and docker-socket access is root-equivalent on this host — a broker that wanted those values could always have read the API's environment file through a privileged container. What actually changes is the blast radius of a defect in the broker that is *not* a full compromise: a core file or a stray debug format now has the registry's credentials in scope. That is why every credential is a `Secret` whose `Debug` prints a placeholder, why the broker drops the parsed document as soon as it has taken its own settings, and why `UMask=0077` and the empty capability set above matter. The sandboxed container is unaffected either way: a compromised run still sees only its own `0700` job mount, never the broker's memory or filesystem.

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
