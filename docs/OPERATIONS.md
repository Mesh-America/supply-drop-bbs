# Operations guide

How to install, run, update, back up, and recover Supply Drop BBS.

> **Status:** Active. Marked **TBD** where specific commands or
> service names are not yet finalised.

## Audience

Sysops running a Supply Drop BBS deployment. Specifically:

- Hobbyist mesh radio operators on a Raspberry Pi
- Researchers / educators running it for a community
- Anyone managing a small-scale BBS deployment that uses LoRa mesh

If you're a contributor or plugin author, see
[CONTRIBUTING.md](../CONTRIBUTING.md) and
[PLUGIN_API.md](PLUGIN_API.md).

## System requirements

| Component | Minimum                                                              |
|-----------|----------------------------------------------------------------------|
| CPU       | armv7 or better. ARM64 (Pi 4+) recommended.                         |
| RAM       | 256 MB available to the BBS process                                  |
| Disk      | 2 GB free for DB + logs + backups; SD card OK                        |
| Network   | Loopback only (USB mode) or LAN for web UI                           |
| Radio     | MeshCore-compatible: SX1262 HAT **or** USB companion device          |
| OS        | Linux — Debian / Raspberry Pi OS tested. Other Unixes likely work.   |
| Python    | 3.10+ — **HAT mode only** (for `pymc_core`)                         |

The BBS itself is a single static Rust binary with no runtime
dependencies. Python is required **only if you are using a Pi HAT**
([ADR-0007](adr/0007-bridge-stays-pymc-core.md)). USB device operators
need nothing else ([ADR-0013](adr/0013-native-serial-transport-for-usb-devices.md)).

## Architecture at a glance

Supply Drop BBS supports two deployment topologies depending on your
radio hardware.

### USB device (single-process)

```
   ┌──────────────────────────────────────────────────┐
   │  supply-drop-bbs  (Rust — one process)           │
   │                                                  │
   │   bbs-core ── bbs-mesh ── meshcore-companion     │
   │                                 │                │
   │                           serial (USB)           │
   └─────────────────────────────────┼────────────────┘
                                     │
                              USB companion device
                              (Heltec V3, T-Beam, …)
                              running MeshCore firmware
```

For USB-native MeshCore devices the BBS speaks the companion-frame
protocol directly over the serial port. No bridge process, no Python.
One service to manage.

### Pi HAT (two-process)

```
   ┌──────────────────────┐         ┌────────────────────────┐
   │  pymc_core           │         │  supply-drop-bbs       │
   │  CompanionFrame      │ ◄─TCP─► │  (Rust BBS host)       │
   │  Server (Python)     │         │                        │
   │                      │         │  also exposes:         │
   │  manages GPIO/SPI    │         │  - CLI socket          │
   │  for the LoRa HAT    │         │  - web UI (opt-in)     │
   └──────────────────────┘         └────────────────────────┘
            │                                  │
            ▼                                  ▼
      SX1262 LoRa HAT                     sysop / users
      (ZebraHat, MeshAdv, …)
```

`pymc_core` owns the radio hardware. The BBS connects to it over a
localhost TCP companion-frame connection. Two independent processes —
either can restart without breaking the other.

## Installation

### One-line install (recommended)

```sh
curl -fsSL https://get.supply-drop.radio/install.sh | bash
```

The install script:

1. Detects your CPU architecture and downloads the right binary.
2. Drops the binary in `/usr/local/bin/supply-drop-bbs`.
3. Launches the **setup wizard** (`supply-drop-bbs setup`).

The wizard asks a small set of questions and handles everything from
there. You only need to know two things before starting:

- **What kind of radio device you have** — USB companion device
  (Heltec V3, T-Beam, etc.) or a Pi HAT (ZebraHat, MeshAdv, etc.)
- **Your radio's frequency and TX power** — e.g. 910.525 MHz, 22 dBm
  for US; 869.525 MHz, 14 dBm for EU

### What the setup wizard does

The wizard asks:

1. **BBS name** — displayed to users on connect
2. **Device type** — USB companion device or Pi HAT
3. **Serial port or HAT preset** — detected automatically where
   possible; you confirm or override
4. **Frequency and TX power** — must match the rest of your mesh
5. **Sysop username** — the first account; gets promoted to sysop
6. **Install as a system service?** — if yes, installs the systemd
   unit(s) and enables them

Based on your answers it:

- Writes `/etc/supply-drop-bbs/config.toml` with all settings filled in
- Runs database migrations
- For **HAT mode**: installs `pymc_core` (Python), configures SPI/UART,
  adds your user to the `spi`, `gpio`, and `dialout` groups, and
  installs a `pymc-core.service` unit alongside `supply-drop-bbs.service`
- Optionally installs and enables the systemd unit(s)
- Prints a summary of what changed and whether a reboot is needed

### After the wizard

**USB device** — no reboot needed. Start immediately:

```sh
sudo systemctl start supply-drop-bbs
sudo journalctl -u supply-drop-bbs -f
```

**Pi HAT** — a reboot is typically required after group and UART
changes. The wizard tells you if this applies:

```sh
sudo reboot
# After reboot:
sudo systemctl start pymc-core supply-drop-bbs
sudo journalctl -u supply-drop-bbs -f
```

### Building from source

Required: Rust 1.76+ (`rustup install stable`).

```sh
git clone https://github.com/Mesh-America/supply-drop-bbs
cd supply-drop-bbs
cargo build --release --features admin-web   # or omit for no web UI
sudo install -m 0755 target/release/supply-drop-bbs /usr/local/bin/
supply-drop-bbs setup
```

Build artifacts depend only on glibc. On a fresh Pi 4 a release build
takes ~3 minutes.

## systemd units

The setup wizard installs and enables these for you. They are also
included in the release tarball under `systemd/` if you want to
manage them manually.

### USB device — one service

#### `supply-drop-bbs.service`

```ini
[Unit]
Description=Supply Drop BBS
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=bbs
Group=bbs
ExecStart=/usr/local/bin/supply-drop-bbs --config /etc/supply-drop-bbs/config.toml
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/supply-drop-bbs /var/log/supply-drop-bbs
PrivateTmp=true
MemoryMax=512M
TasksMax=200

[Install]
WantedBy=multi-user.target
```

Note: the `bbs` user must be in the `dialout` group to open the
serial port. The setup wizard handles this.

### Pi HAT — two services

#### `pymc-core.service`

```ini
[Unit]
Description=pymc_core CompanionFrameServer (LoRa radio bridge)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=bbs
Group=bbs
WorkingDirectory=/opt/pymc-core
ExecStart=/opt/pymc-core/venv/bin/python -m pymc_core.server
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
```

#### `supply-drop-bbs.service` (HAT variant)

```ini
[Unit]
Description=Supply Drop BBS
After=network-online.target pymc-core.service
Wants=network-online.target
Requires=pymc-core.service

[Service]
Type=simple
User=bbs
Group=bbs
ExecStart=/usr/local/bin/supply-drop-bbs --config /etc/supply-drop-bbs/config.toml
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/supply-drop-bbs /var/log/supply-drop-bbs
PrivateTmp=true
MemoryMax=512M
TasksMax=200

[Install]
WantedBy=multi-user.target
```

The two services are independent at the socket level — the BBS
reconnects automatically if `pymc-core` restarts.

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

### Radio connection keeps dropping

**USB device:**

```sh
ls -la /dev/ttyACM* /dev/ttyUSB*          # confirm device is visible
sudo journalctl -u supply-drop-bbs -f     # look for serial errors
```

Common causes:
- Device not in `dialout` group: `sudo usermod -aG dialout bbs`
- Wrong serial port configured — check `[plugins.mesh] serial_port`
- Firmware crashed — unplug and replug the device

**Pi HAT:**

```sh
sudo systemctl status pymc-core
sudo journalctl -u pymc-core -f
```

Common causes:
- SPI or GPIO not enabled — run `sudo raspi-config` → Interface Options
- `bbs` user not in `spi`/`gpio` groups — the setup wizard adds
  these; a re-login or reboot is required after the change
- Wrong HAT preset or pin override — check `[plugins.mesh.hat]` in config
- `pymc_core` version mismatch — check its own logs for protocol errors

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
