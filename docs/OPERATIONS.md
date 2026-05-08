# Operations guide

How to install, run, update, back up, and recover Supply Drop BBS.

> **Status:** stub. Sections will be fleshed out as the
> implementation lands. Marked **TBD** where final commands aren't
> known yet.

## Audience

Sysops running a Supply Drop BBS deployment. Specifically:

- Hobbyist mesh radio operators on a Raspberry Pi
- Researchers / educators running it for a community
- Anyone managing a small-scale BBS deployment that uses LoRa mesh

If you're a contributor or plugin author, see
[CONTRIBUTING.md](../CONTRIBUTING.md) and
[PLUGIN_API.md](PLUGIN_API.md).

## System requirements

| Component         | Minimum                                       |
|-------------------|-----------------------------------------------|
| CPU               | armv7 or better. ARM64 (Pi 4+) recommended.   |
| RAM               | 256 MB available to the BBS process           |
| Disk              | 2 GB free for DB + logs + backups; SD card OK |
| Network           | Loopback for the bridge; LAN for web UI       |
| Radio             | MeshCore-compatible: SX1262 HAT or USB device |
| OS                | Linux (Debian/Raspberry Pi OS tested). Other Unixes likely work; Windows untested. |
| Python            | 3.10+ (for the radio bridge process)          |

The BBS itself is a single static Rust binary. Python is required
only for the radio bridge ([ADR-0007](adr/0007-bridge-stays-pymc-core.md)).

## Architecture at a glance

A complete deployment has two processes:

```
   ┌─────────────────────┐         ┌────────────────────┐
   │ pymc_core           │         │ supply-drop-bbs    │
   │ CompanionFrame      │ ◄─TCP─► │ (Rust BBS host)    │
   │ Server (Python)     │         │                    │
   │                     │         │ also exposes:      │
   │ owns the radio      │         │ - CLI socket        │
   │                     │         │ - web UI (opt-in)   │
   └─────────────────────┘         └────────────────────┘
            │                                 │
            ▼                                 ▼
        SX1262 HAT                       sysop / users
        / USB device
```

The two processes are independent — one can restart without
breaking the other.

## Installation

### Quick path (binary release)

1. **Identify your architecture.**

   ```sh
   uname -m
   # aarch64  → use the aarch64-unknown-linux-gnu binary
   # armv7l   → use the armv7-unknown-linux-gnueabihf binary
   # x86_64   → use the x86_64-unknown-linux-gnu binary
   ```

2. **Download the release tarball** from
   <https://github.com/Mesh-America/supply-drop-bbs/releases>.

   Pick the variant matching your needs:

   | Variant                       | Includes                          |
   |-------------------------------|-----------------------------------|
   | `supply-drop-bbs-<arch>`      | CLI + mesh transports             |
   | `supply-drop-bbs-web-<arch>`  | CLI + mesh + web admin UI         |
   | `supply-drop-bbs-headless-<arch>` | CLI only (development)         |

3. **Extract and place the binary.**

   ```sh
   tar -xzf supply-drop-bbs-web-aarch64.tar.gz
   sudo install -m 0755 supply-drop-bbs /usr/local/bin/
   ```

   The tarball also contains:

   - `systemd/supply-drop-bbs.service` — the BBS unit file
   - `systemd/supply-drop-bridge.service` — example bridge unit
   - `config.example.toml` — annotated config template

4. **Install the radio bridge.** Per
   [`pymc_core`](https://github.com/meshcore-dev/pymc_core)'s docs:

   ```sh
   sudo apt install python3-venv  # if not already
   python3 -m venv /opt/pymc-bridge/venv
   /opt/pymc-bridge/venv/bin/pip install pymc_core
   ```

   Configure it to run a `CompanionFrameServer` on `127.0.0.1:5000`
   (or any address you want; match it in the BBS config).

5. **Run the BBS first-run setup.**

   ```sh
   sudo supply-drop-bbs init
   ```

   This is interactive. It will:

   - Create the data directory (default `/var/lib/supply-drop-bbs`)
   - Generate a starting `config.toml` with the answers you give
   - Run database migrations
   - Prompt for the initial sysop username and password
   - Optionally install + enable the systemd unit

6. **Verify the config.**

   ```sh
   supply-drop-bbs config check
   supply-drop-bbs config show | less
   ```

   `config check` exits 0 if the config is valid. `config show`
   prints the *effective* config including defaults.

7. **Start the services.**

   ```sh
   sudo systemctl enable --now supply-drop-bridge
   sudo systemctl enable --now supply-drop-bbs
   ```

8. **Smoke test.**

   ```sh
   sudo journalctl -u supply-drop-bbs -f
   # Look for "logging initialised", "transport started", etc.
   ```

   With the web variant, browse to `http://<host>:8080` (or
   whatever you configured) and log in with the sysop credentials
   you set.

### Building from source

Required: Rust 1.76+ (`rustup install stable`).

```sh
git clone https://github.com/Mesh-America/supply-drop-bbs
cd supply-drop-bbs
cargo build --release --features admin-web   # or omit for no web
sudo install -m 0755 target/release/supply-drop-bbs /usr/local/bin/
```

Build artifacts depend only on glibc (and OpenSSL only if a future
feature pulls it in; v1 is rustls-only). On a fresh Pi 4 a release
build takes ~3 minutes.

## systemd units

### `supply-drop-bbs.service`

```ini
[Unit]
Description=Supply Drop BBS
After=network-online.target supply-drop-bridge.service
Wants=network-online.target supply-drop-bridge.service

[Service]
Type=simple
User=bbs
Group=bbs
ExecStart=/usr/local/bin/supply-drop-bbs --config /etc/supply-drop-bbs/config.toml
Restart=on-failure
RestartSec=5

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/supply-drop-bbs /var/log/supply-drop-bbs
PrivateTmp=true
PrivateDevices=true

# Resource limits
MemoryMax=512M
TasksMax=200

[Install]
WantedBy=multi-user.target
```

The `init` subcommand can install this for you, or you can drop it
into `/etc/systemd/system/` manually.

### `supply-drop-bridge.service`

**TBD** — example unit file for the `pymc_core` bridge process.
Will be in the release tarball as
`systemd/supply-drop-bridge.service`. Specifies user, working
directory, the bridge command, and `Restart=on-failure`.

## Update

The update flow:

1. **Stop the BBS** (the bridge can stay running):

   ```sh
   sudo systemctl stop supply-drop-bbs
   ```

2. **Replace the binary.**

   ```sh
   sudo install -m 0755 supply-drop-bbs /usr/local/bin/
   ```

3. **Apply pending migrations.**

   ```sh
   sudo -u bbs supply-drop-bbs migrate
   ```

   Migrations are forward-only. We do not ship downgrades.

4. **Start the BBS.**

   ```sh
   sudo systemctl start supply-drop-bbs
   ```

5. **Tail the logs** to confirm clean startup:

   ```sh
   sudo journalctl -u supply-drop-bbs -f
   ```

For minor patch releases, the binary swap is enough — no migration
runs. The `migrate` step is fast no-op when there's nothing to do.

## Backups

### Automatic backups

If `[backup] enabled = true` (default), the BBS runs
`VACUUM INTO 'backup-YYYY-MM-DD-HHMMSS.sqlite'` on the configured
interval. Backups land in `[backup] directory` (default
`<data_dir>/backups`).

`VACUUM INTO` is non-blocking — the live DB keeps serving while
the backup runs.

Retention defaults: 7 daily + 4 weekly. Configurable.

### Manual backup

```sh
supply-drop-bbs backup
```

Triggers an immediate backup. Useful before risky operations
(major upgrades, schema changes, etc.). Or via the web admin UI's
"Trigger backup" button.

### Off-host backups

The BBS doesn't push backups anywhere. That's deliberate — no
phone-home. Operators handle off-host retention themselves:

```sh
# Cron: every night, copy the latest backup to a remote host
0 3 * * *  rsync -a /var/lib/supply-drop-bbs/backups/ \
              backup-host:/srv/bbs-backups/$(hostname)/
```

## Disaster recovery

### Live DB is corrupted

1. **Stop the BBS.**

   ```sh
   sudo systemctl stop supply-drop-bbs
   ```

2. **Move the corrupted file aside.** Don't delete it; future
   diagnosis may benefit.

   ```sh
   sudo mv /var/lib/supply-drop-bbs/bbs.sqlite /var/lib/supply-drop-bbs/bbs.sqlite.corrupt
   sudo mv /var/lib/supply-drop-bbs/bbs.sqlite-wal /var/lib/supply-drop-bbs/bbs.sqlite-wal.corrupt 2>/dev/null
   sudo mv /var/lib/supply-drop-bbs/bbs.sqlite-shm /var/lib/supply-drop-bbs/bbs.sqlite-shm.corrupt 2>/dev/null
   ```

3. **Identify the latest viable backup.**

   ```sh
   ls -lt /var/lib/supply-drop-bbs/backups/
   ```

4. **Restore.**

   ```sh
   sudo cp /var/lib/supply-drop-bbs/backups/backup-2026-05-08-030000.sqlite \
           /var/lib/supply-drop-bbs/bbs.sqlite
   sudo chown bbs:bbs /var/lib/supply-drop-bbs/bbs.sqlite
   ```

5. **If the backup is from an older schema version, migrate.**

   ```sh
   sudo -u bbs supply-drop-bbs migrate
   ```

6. **Start the BBS.**

   ```sh
   sudo systemctl start supply-drop-bbs
   ```

### Lost sysop access

If the only sysop account is locked out:

```sh
sudo systemctl stop supply-drop-bbs
sudo -u bbs supply-drop-bbs admin reset-sysop --username <name>
sudo systemctl start supply-drop-bbs
```

This prompts for a new password. **TBD** — exact subcommand name
when implemented.

This requires direct access to the DB file, so it can only be done
on the host where the BBS runs.

## Monitoring

### Health endpoint

If the web admin plugin is enabled, `GET /health` returns:

```json
{
  "status": "healthy",
  "uptime_seconds": 1234567,
  "version": "0.1.0",
  "bridge_connected": true,
  "transports": {
    "cli": "running",
    "mesh": "running",
    "web": "running"
  },
  "db": { "size_bytes": 12345678, "last_backup": "2026-05-08T03:00:00Z" }
}
```

`status` is `"healthy"` only if every transport reports running
and the bridge is connected. Otherwise `"degraded"` with a
description of what's wrong.

### Prometheus metrics

Off by default. Enable with `[plugins.web] prometheus = true`.
Endpoint: `GET /metrics`.

Exposed metrics (**TBD** — full list when implemented):

- `supply_drop_uptime_seconds` (gauge)
- `supply_drop_db_pool_active` / `_idle` / `_waiting` (gauges)
- `supply_drop_transport_connections{transport=...}` (gauge)
- `supply_drop_commands_processed_total{transport=...}` (counter)
- `supply_drop_messages_posted_total` (counter)
- `supply_drop_login_failures_total{source=...}` (counter)
- `supply_drop_bridge_connected` (gauge: 0 or 1)
- `supply_drop_backup_last_success_timestamp_seconds` (gauge)

### Logs

Default location: `<data_dir>/log/bbs.log`. Rotated automatically.

For systemd deployments:

```sh
journalctl -u supply-drop-bbs -f          # tail
journalctl -u supply-drop-bbs --since "1 hour ago" -p err  # errors only
```

## Reverse-proxy setup (HTTPS)

The web admin plugin doesn't terminate TLS. Use nginx or caddy:

### Caddy example

```
admin.bbs.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

Set the BBS config:

```toml
[plugins.web]
bind = "127.0.0.1:8080"
external_origin = "https://admin.bbs.example.com"
cookie_secure = true
```

Caddy handles cert issuance + renewal via Let's Encrypt
automatically.

### nginx example

**TBD** — full config snippet with HSTS, CSP-passthrough, etc.

## Troubleshooting

### BBS won't start

Run `supply-drop-bbs config check`. Most startup failures come
from invalid config.

If config is valid but the BBS still fails:

```sh
sudo journalctl -u supply-drop-bbs -n 100 --no-pager
```

Look for the first ERROR line. Subsequent errors are usually
cascading consequences of the first.

### Bridge connection keeps dropping

Check the bridge process:

```sh
sudo systemctl status supply-drop-bridge
sudo journalctl -u supply-drop-bridge -f
```

Common causes:

- Radio HAT not responding (SPI bus issue, power, antenna)
- Bridge's `pymc_core` config doesn't match BBS's `bridge_addr`
- Firewall blocking 127.0.0.1 connections (rare but happens)

### Database is locked

Almost always indicates two BBS instances running against the same
DB. Check:

```sh
sudo systemctl status supply-drop-bbs
ps -ef | grep supply-drop-bbs
```

If clean, but you still see `database is locked`, investigate
file permissions on the DB and the WAL files (they must all be
owned by the BBS process user).

### Web UI returns 502 / connection refused

- Verify the BBS is running: `systemctl status supply-drop-bbs`
- Verify it's listening: `ss -ltnp | grep supply-drop-bbs`
- Verify the reverse proxy points at the right address
- Check `[plugins.web] external_origin` matches what the browser
  actually requests

### Lost messages after a restart

Should not happen with the disk-WAL DB strategy. If it does:

- Check `[database] synchronous` setting (`OFF` is the only setting
  that can lose meaningful data)
- Check disk health (`dmesg | grep -i error`)
- Check for filesystem-level issues (`fsck`)

## Where to get help

- **Bugs:** GitHub issues
- **Security:** see [SECURITY.md](../SECURITY.md)
- **General questions:** GitHub Discussions (when enabled)

## Versioning and stability

Pre-1.0: assume each release may include breaking changes. Always
read the release notes. We document upgrade caveats prominently.

After 1.0: semver. Major version bumps include breaking changes;
minor releases add features compatibly; patches are bug fixes only.
