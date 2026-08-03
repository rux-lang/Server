# Production host provisioning

Production provisioning configures one existing, dedicated x86-64 Ubuntu 26.04 LTS host. The playbook is safe to repeat and owns the operating-system policy described here. It does not create DigitalOcean resources or DNS records and does not deploy an API release. Releases use the separate, operator-approved [production release workflow](releases.md).

## Prerequisites

- Create the droplet from an Ubuntu 26.04 image and point `rux-lang.dev` and `api.rux-lang.dev` at it before expecting Caddy to obtain certificates.
- Use a bootstrap account with root or passwordless sudo access.
- Decide the exact IPv4 and IPv6 CIDRs allowed to administer SSH. The playbook replaces the host firewall policy, so verify the current operator address is included before the first run.
- Install Python 3.13 or newer on the Ansible controller. Windows operators should run Ansible in WSL rather than directly in PowerShell.

Install the pinned controller dependencies and collection:

```bash
python3 -m venv .venv
. .venv/bin/activate
pip install --requirement deploy/ansible/requirements.txt
ansible-galaxy collection install --requirements-file deploy/ansible/requirements.yml
```

## Inventory and secrets

Copy `deploy/ansible/inventory/production.example.yml` to the ignored `production.yml` and replace the host, bootstrap login, administrator public keys, allowed CIDRs, domains, and Spaces values. The inventory contains no secret values.

Create an untracked file outside the repository, or encrypt the equivalent file with Ansible Vault, containing:

```yaml
rux_postgres_password: replace-with-at-least-24-characters
rux_storage_access_key: replace
rux_storage_secret_key: replace
rux_github_client_id: replace
rux_github_client_secret: replace
rux_grafana_admin_password: replace-with-at-least-24-characters
rux_alert_warning_webhook_url: https://warning-receiver.example/secret-path
rux_alert_critical_webhook_url: https://critical-receiver.example/secret-path
rux_backup_access_key: replace
rux_backup_secret_key: replace
rux_backup_cipher_passphrase: replace-with-at-least-32-random-characters
```

Both alert URLs are required, must use HTTPS, and must be distinct. Secret tasks use Ansible's output redaction and render only root-owned `0640` files. Do not place credentials in inventory, command-line values, shell history, or CI logs.

## Converge the host

Preview the change first:

```bash
ANSIBLE_CONFIG=deploy/ansible/ansible.cfg ansible-playbook \
  -i deploy/ansible/inventory/production.yml \
  deploy/ansible/site.yml \
  --extra-vars @/secure/path/production-secrets.yml \
  --check --diff
```

Apply the same command without `--check --diff`. The first run may restart SSH, but it installs and validates the new administrator key and allowed firewall rule before doing so. Log in as the configured administrator and rerun the playbook through that account; a second unchanged run must report zero changes.

Ansible never reboots automatically. If `/run/reboot-required` exists, schedule a reboot and rerun the playbook afterward. Ubuntu security updates remain automatic, while held Caddy, Prometheus, Alertmanager, and Grafana versions are advanced only through a reviewed repository change.

## Resulting boundary

| Listener                          | Exposure                       |
| --------------------------------- | ------------------------------ |
| SSH                               | Configured operator CIDRs only |
| Caddy HTTP/HTTPS/HTTP3            | Public                         |
| API `127.0.0.1:8080`              | Caddy only                     |
| PostgreSQL `127.0.0.1:5432`       | Local only                     |
| API metrics `127.0.0.1:9464`      | Prometheus only                |
| Prometheus `127.0.0.1:9090`       | SSH tunnel/local only          |
| Alertmanager `127.0.0.1:9093`     | SSH tunnel/local only          |
| Grafana `127.0.0.1:3001`          | SSH tunnel/local only          |
| Recovery metrics `127.0.0.1:9100` | Prometheus only                |

The playbook creates `/srv/rux-server/releases`, `/srv/rux-server/shared`, and the private upload directory. It installs and enables `rux-server.service` without changing the state of an already deployed service. On a new host the unit remains stopped and its `ConditionPathIsExecutable` is false until the release playbook installs `/srv/rux-server/current/bin/rux-server`. Release automation owns migrations, symlink promotion, readiness checks, and application rollback.

## Operations and recovery

Validate configuration locally on the host with:

```bash
sudo sshd -t
sudo nft --check --file /etc/nftables.conf
sudo caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo promtool check config /etc/prometheus/prometheus.yml
sudo amtool check-config /etc/alertmanager/alertmanager.yml
```

Inspect a service with `systemctl status` and `journalctl -u`. Preserve logs and identify the configuration or dependency failure before restarting it. Rotate a database, provider, OAuth, Grafana, or webhook secret by updating the secure input and rerunning the playbook; affected services restart only when their rendered input changes.

PostgreSQL data remains in the Ubuntu package's standard versioned data directory. Provisioning configures encrypted cross-region base backups, continuous WAL archival, monitored systemd schedules, and an initial full backup. The target Space must already exist with versioning enabled. See the [production recovery runbook](recovery.md) for repository prerequisites, restore procedures, and quarterly rehearsals.
