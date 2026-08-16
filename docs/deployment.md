# Production deployment

The registry runs on one hand-configured Ubuntu droplet: PostgreSQL and the API on the host, package artifacts in DigitalOcean Spaces, and Caddy terminating TLS in front. The playground is opt-in and is the only reason Docker is installed at all.

Building this host is not automated — every step below is an operator action, deliberately, so that the shape of production is something a person decided. Releasing onto it is automated: pushing a `vX.Y.Z` tag deploys that version, applies pending migrations, and verifies the result, with no approval gate. See [Deploying a new version](#deploying-a-new-version).

Read [architecture.md](architecture.md) for what the components are and [migrations.md](migrations.md) for why the API never migrates its own schema.

## Host baseline

Use Ubuntu 26.04 LTS on x86-64. Create an administrator account with your SSH key, disable password and root SSH login, and enable unattended security upgrades. Bound journald so logs cannot fill the disk.

On a cloud image the default `ubuntu` account already is that administrator: it carries the SSH key and passwordless sudo. It also holds uid 1000, which [Service accounts](#service-accounts) would rather give to `rux-playground` — read that section before deciding whether to move it, because the answer is "preferably, not necessarily".

The firewall accepts 22, 80, and 443 and drops everything else, including forwarded traffic. The input chain is created accepting and only closed at the end, because a chain that becomes `policy drop` before the established-connection rule exists kills the SSH session you are typing into:

```bash
sudo nft add table inet filter
sudo nft 'add chain inet filter input { type filter hook input priority 0; policy accept; }'
sudo nft 'add rule inet filter input ct state established,related accept'
sudo nft 'add rule inet filter input iif lo accept'
sudo nft 'add rule inet filter input ct state invalid drop'
sudo nft 'add rule inet filter input tcp dport { 22, 80, 443 } accept'
sudo nft 'add rule inet filter input meta l4proto { icmp, ipv6-icmp } accept'
sudo nft 'add chain inet filter forward { type filter hook forward priority 0; policy drop; }'
sudo nft 'chain inet filter input { policy drop; }'
```

ICMP stays open in both families deliberately: dropping ICMPv6 breaks path-MTU discovery and neighbour discovery, which fails as intermittent stalls rather than as a refused connection. Nothing above survives a reboot until it is written out, and `nft -f` on a file with no `flush ruleset` appends to whatever is already loaded, so the flush belongs in the file rather than in the operator's memory:

```bash
{ echo 'flush ruleset'; sudo nft list ruleset; } | sudo tee /etc/nftables.conf > /dev/null
sudo systemctl enable --now nftables
```

The forward policy matters for the playground: it is the outer guarantee that a sandbox container reaches no network even if a container flag is ever wrong.

## Service accounts

Two system users, no login shells. The second one and both group memberships below are the playground's; a registry-only host needs `rux-server` alone:

```bash
sudo useradd --system --home-dir /srv/rux-server --shell /usr/sbin/nologin rux-server
sudo useradd --system --uid 1000 --home-dir /var/lib/rux-playground --shell /usr/sbin/nologin rux-playground
```

Uid 1000 matches the `play` user baked into the sandbox image by `playground/Dockerfile`, and it is a preference rather than a requirement — worth stating exactly, because the `useradd` above fails outright when a cloud image's default `ubuntu` account already holds it. What the broker passes as `--user` is whatever uid owns the `0700` job directory, so the mount is self-consistent at any number. What matching buys is a real passwd entry inside the container: at 1000 the run owns `$HOME` and could write there, while at any other uid it is an anonymous uid that reads the pinned compiler's package cache through the image's `a+rX` and builds in the `1777` tmpfs at `/work`. That unmatched case is the one CI exercises on every push, because the smoke job runs the broker as the runner account.

So take 1000 if it is free, and if it is not, either move the account holding it (`usermod --uid 1100 ubuntu`, from a root session, then `find / -xdev -uid 1000`) or simply pass `--uid 1001` here. `useradd` warns that 1001 is above `SYS_UID_MAX`; the warning is cosmetic and the account is still created with system semantics. Revisit the choice only if a future pinned compiler needs to write under `$HOME` — CI fails first if it does.

Only `rux-playground` joins the `docker` group; `rux-server` never does. The API reaches the broker through a shared group instead. Both lines need the `docker` group to exist, so they wait until Docker is installed in [The playground](#the-playground-optional):

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

At minimum it sets `database.url`, the four required `[storage]` values, `packages.cdn_base_url`, `[web]`, the two required `[github]` OAuth values, and `uploads.temporary_directory = "/var/lib/rux-server/uploads"`. The browser origin and the callback are configured independently on purpose. Secrets belong only in this file; it is never committed and never logged.

The upload directory is the one entry on that list with a working default, and the default is wrong here rather than merely unset: omitted, it resolves to the operating-system temporary directory, which `PrivateTmp=true` below makes a private tmpfs. Every publication would then stream into memory — up to `uploads.temporary_capacity_bytes` times `uploads.max_concurrency` of it — and the directory the next section has you create would sit unused. Nothing fails at startup, which is exactly why it is named here.

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

Create `/var/lib/rux-server/uploads` owned by `rux-server` before starting; publication streams bounded multipart uploads through it, and `ProtectSystem=strict` refuses to start a unit whose `ReadWritePaths` names a directory that does not exist:

```bash
sudo install -d -m 0700 -o rux-server -g rux-server /var/lib/rux-server /var/lib/rux-server/uploads
```

`AF_NETLINK` is deliberately absent from `RestrictAddressFamilies`: glibc's resolver opens a netlink socket to enumerate local addresses but treats a refusal as "assume both families are present", so name resolution to Spaces and GitHub still works. If a future dependency resolves names itself rather than through `getaddrinfo`, that assumption is the thing to re-check.

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

Both directories the unit names have to exist before it starts. `useradd --system` does not create a home directory, so `/var/lib/rux-playground` is not there yet, and `/run` is a tmpfs, so `/run/rux-playground` is gone after every boot — either one missing from `ReadWritePaths` fails the unit while systemd is still setting up its mount namespace, before a line of our code runs:

```bash
sudo install -d -m 0750 -o rux-playground -g rux-playground /var/lib/rux-playground
printf 'd /run/rux-playground 0750 rux-playground rux-playground -\n' \
  | sudo tee /etc/tmpfiles.d/rux-playground.conf > /dev/null
sudo systemd-tmpfiles --create /etc/tmpfiles.d/rux-playground.conf
```

The `0750` on the runtime directory is what lets `rux-server` reach the socket through the shared group without letting anything else on the host near it. Nothing below `/var/lib/rux-playground` needs creating by hand: the broker makes `jobs_root` on first use and each job directory `0700` under it. Start the broker before the API so the socket exists on the API's first request.

## The deploy account

The pipeline reaches this host as an unprivileged account that can `sudo`. Its key is generated on your own machine rather than on the droplet, so the private half never rests on the host it opens:

```bash
ssh-keygen -t ed25519 -a 100 -C github-actions-deploy -f ~/.ssh/rux-deploy -N ""
```

No passphrase, deliberately: the workflow writes the key out and runs `ssh` non-interactively, so a passphrase would hang the deploy rather than protect it. What protects this key is that it lives in a GitHub environment restricted to `v*` tags and grants nothing but this one account.

Then create the account, with `~/.ssh/rux-deploy.pub` pasted in place of the placeholder below. The restriction options and the key are one physical line — a wrapped line is read as a second, malformed key rather than as a continuation:

```bash
sudo adduser --disabled-password --gecos '' deploy
sudo install -d -m 0700 -o deploy -g deploy /home/deploy/.ssh
sudo -u deploy tee /home/deploy/.ssh/authorized_keys > /dev/null <<'KEY'
no-agent-forwarding,no-port-forwarding,no-X11-forwarding,no-user-rc ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI... github-actions-deploy
KEY
sudo chmod 0600 /home/deploy/.ssh/authorized_keys
echo 'deploy ALL=(root) NOPASSWD: ALL' | sudo tee /etc/sudoers.d/rux-deploy > /dev/null
sudo chmod 0440 /etc/sudoers.d/rux-deploy
sudo visudo --check
```

The key restrictions matter more than they look. Without `no-port-forwarding` that key could tunnel straight to PostgreSQL on `127.0.0.1:5432`, which is otherwise unreachable from outside the host.

Prove both halves from your machine before a tag depends on them, because the first thing the pipeline does with this key is stop a running service:

```bash
ssh -i ~/.ssh/rux-deploy deploy@<host> 'sudo -n true && echo ok'
```

`ok` says the key is accepted and the sudoers entry needs no password. A `-L` tunnel that fails with the same key is `no-port-forwarding` working, not a broken key. The private half then becomes `DEPLOY_SSH_KEY` in the `production` environment, with the three secrets beside it in [Deploying a new version](#deploying-a-new-version) — `DEPLOY_KNOWN_HOSTS` among them, which is why it is worth reading this host's own fingerprint now, from the session you already trust, rather than believing whatever `ssh-keyscan` returns later:

```bash
ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub
```

Be plain about what this grants: **the deploy key is root-equivalent on this droplet, and pushing a tag is enough to use it.** That is the accepted trade for an automated deploy that also migrates the database — the sequence has to stop services, write to `/srv`, and read a `0400 root:root` configuration file. What the sudoers entry buys is attribution, not containment: every command is `deploy`'s in `auth.log` rather than an anonymous root login. What actually limits the blast radius is on GitHub's side: the `production` environment restricted to `v*` tags, a tag ruleset that only maintainers can create, and branch protection on `main`.

The `deploy` account must **not** join `docker`, `rux-server`, or `rux-playground`. It reaches everything through `sudo`, so a group membership would only add a standing grant.

The remaining one-time preparation:

```bash
sudo install -d -m 0755 -o root -g root /srv/rux-server/bin
sudo apt-get install -y python3 postgresql-client-18 util-linux curl
```

`python3` parses the configuration file, `pg_dump` takes the pre-migration snapshot, and `flock` from `util-linux` keeps two deploys from overlapping. No Rust toolchain is needed: the SQLx CLI travels in the release tarball, already pinned to the workspace's SQLx version.

That lock is `/var/lock/rux-deploy.lock` — `/var/lock` is a symlink to `/run/lock`, so it is the same tmpfs the entries below name — and the script opens it as `deploy` rather than through `sudo`, so confirm once that the account can actually create it. Ubuntu ships `/run/lock` world-writable and sticky, but a hardened image may not, and the failure arrives as a deploy that gives up before it has done anything:

```bash
ls -ld /run/lock && sudo -u deploy touch /var/lock/rux-deploy.lock
```

If it is not, have `tmpfiles.d` keep that one file in existence and owned by `deploy` — opening an existing file for writing needs permission on the file, not on its directory, so this fixes the lock without widening `/run/lock` for everything else on the host and without the deploy script differing from the one in the repository:

```bash
printf 'f /run/lock/rux-deploy.lock 0600 deploy deploy -\n' \
  | sudo tee /etc/tmpfiles.d/rux-deploy.conf > /dev/null
sudo systemd-tmpfiles --create /etc/tmpfiles.d/rux-deploy.conf
```

## Deploying a new version

A tag is a deploy. From a green `dev`:

```powershell
./Run.ps1 release 0.1.1     # bump the workspace version, run the gates, commit
git push origin dev         # let CI go green
./Run.ps1 promote           # fast-forward main, tag v0.1.1, push both
```

`.github/workflows/release.yml` then, in order: checks that the tag matches `[workspace.package] version` in `Cargo.toml` and that the tagged commit is on `main`; runs the whole CI suite against it; builds `--release --locked` on a pinned Ubuntu 24.04 runner; packages the binaries, `migrations/`, and the pinned SQLx CLI into a checksummed tarball; publishes a GitHub Release; and deploys that exact artifact over SSH.

Building on 24.04 while the host runs 26.04 is deliberate. glibc is backward compatible, so an older build runs on a newer host but not the reverse; pinning the runner keeps that floor a decision rather than something GitHub moves when `ubuntu-latest` advances.

On the host, `.github/deploy/deploy.sh` runs the sequence. It verifies the checksum, unpacks, and checks the staged `rux-server` actually loads on this host's glibc — neither binary takes `--version`, so it is started against a deliberately absent configuration file and expected to fail in our own parser rather than in the loader. It then reads `database.url` out of `/etc/rux/config.toml` and reports the pending migrations. **Nothing so far has touched the running service**, and a failure at any of those points leaves production untouched.

Only then does it snapshot the installed binaries as `.previous`, stop `rux-server`, take a custom-format `pg_dump` into `/var/backups/rux`, apply the migrations, install the new binaries, and start the service. Migrations run before the binaries are installed rather than after, so a failed migration leaves the outgoing binary in place and recovery is a plain restart.

Verification is what ends the deploy, and it happens on the host: Caddy answers 404 for `/health` and `/health/*` publicly, so the checks poll `127.0.0.1:8080`.

```bash
curl --fail --silent http://127.0.0.1:8080/health/live
curl --fail --silent http://127.0.0.1:8080/health/ready
```

The `version` field on `/health/live` must equal the version just deployed — that is what proves the process answering is the new one, rather than a restart that silently failed and left the previous binary serving. It identifies the release and not the commit, so the tarball also carries a `COMMIT` file; two builds of one version are otherwise indistinguishable here. Readiness must be 200 with both `postgresql` and `object_storage` healthy. The last sixty journal lines land in the workflow log and the run summary either way.

Secrets live in a GitHub environment named `production`, restricted to `v*` tags: `DEPLOY_HOST`, `DEPLOY_USER`, `DEPLOY_SSH_KEY`, and `DEPLOY_KNOWN_HOSTS` — the pinned output of `ssh-keyscan -t ed25519 <host>`, checked against the droplet console's fingerprint before it is trusted. **No production credential is stored on GitHub.** The database URL, the Spaces keys, and the GitHub OAuth secret stay in `/etc/rux/config.toml`, which is never deployed and never leaves the host, so a compromise of the repository yields command execution here but not the registry's credentials.

To rehearse without touching the service, run the workflow by hand with the tag selected as the ref and `dry_run` true. It stops immediately after the migration pre-flight.

### Deploying by hand

Keep this path working; [recovery.md](recovery.md) sends you here when rebuilding a host, and it is what you use when GitHub is unavailable. Download the release tarball and its `.sha256` from the Releases page, copy them to the host, and run the same script the pipeline runs:

```bash
sha256sum --check --strict rux-server-v0.1.1-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf rux-server-v0.1.1-x86_64-unknown-linux-gnu.tar.gz
bash rux-server-v0.1.1-x86_64-unknown-linux-gnu/deploy.sh 0.1.1 \
  rux-server-v0.1.1-x86_64-unknown-linux-gnu.tar.gz
```

Or do it step by step: stop the service, keep the outgoing binaries as `.previous`, `sqlx migrate info` and `sqlx migrate run` with `DATABASE_URL` read out of `/etc/rux/config.toml`, install the new binaries under `/srv/rux-server/bin/`, start the service, and make the same two health assertions.

## Rolling back

The deploy rolls itself back. If anything fails after the service is stopped, the script restores the `.previous` binaries, restarts, and reports whether the previous release came back healthy. `.previous` is snapshotted *before* the stop, so it always means "what this run displaced" rather than whatever an earlier deploy left behind.

**Migrations are never reverted in production.** If the failure happened after they were applied, the restored binary is running against the new schema and the script says so in capitals, naming the pre-migration dump. That situation is only safe if the previous release tolerates the new schema — which is exactly what the expand-and-contract rule in [migrations.md](migrations.md) exists to guarantee, and why "compatible with the outgoing release" is a requirement rather than advice now that rollback is automatic. If it does not tolerate it, roll forward with a new migration.

The one state that needs a person is a rollback that itself fails to come back healthy; the script prints `THIS HOST NEEDS AN OPERATOR NOW` and the run goes red. Restore the `.previous` binaries by hand, and if the schema is the problem, `pg_restore --clean --if-exists --no-owner` from the dump the script named.

To roll back deliberately rather than on failure, deploy the previous tag again — the artifact is still on the Releases page — after deciding the schema is compatible.

## Operating

Logs are structured JSON on stdout, captured by journald. There is no metrics endpoint and no separate observability stack; `journalctl` is the interface.

```bash
journalctl -u rux-server -f
journalctl -u rux-server --since '1 hour ago' | jq 'select(.level == "ERROR")'
```

Each HTTP request logs its `request_id`, method, route template, status, and duration. Route templates rather than concrete paths are logged on purpose, so a package name or token never lands in a log line. The orphan cleanup sweep logs its counts once per run, and playground runs log mode, outcome, and duration — never submitted source.

Backups, restores, and rehearsals are in [recovery.md](recovery.md).
