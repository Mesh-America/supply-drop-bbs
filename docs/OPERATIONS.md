# Operations guide

How to install, configure, update, back up, and remove Supply Drop BBS.

::: tip Quick install
Download the `.deb` for your hardware from the [latest release](https://github.com/Mesh-America/supply-drop-bbs/releases/latest) — `arm64` for Pi 4/5, `armhf` for Pi 2/3/Zero 2, `amd64` for x86-64 — then:

```sh
sudo dpkg -i supply-drop-bbs_VERSION_ARCH.deb
sudo supply-drop-bbs setup
sudo systemctl start supply-drop-bbs
```

Jump to the [full installation section](#installation) for all options and details.
:::

::: info This guide covers MeshCore
The installation and configuration steps here are written for MeshCore LoRa hardware (USB companion devices and Pi HATs). Supply Drop BBS supports additional transports — Meshtastic, APRS, Telnet, and others — and dedicated guides for those will be added as they mature.
:::

## Audience

Sysops running a Supply Drop BBS deployment:

- Hobbyist mesh radio operators on a Raspberry Pi
- Researchers and educators running a community BBS
- Anyone managing a small-scale BBS deployment over LoRa mesh

If you're a contributor or plugin author, see
[CONTRIBUTING.md](../CONTRIBUTING.md) and [PLUGIN_API.md](PLUGIN_API.md).

## System requirements

| Component | Minimum |
|-----------|---------|
| CPU       | ARMv7 or better. ARM64 (Pi 4+) recommended. |
| RAM       | 256 MB available to the BBS process |
| Disk      | 2 GB free for DB + logs + backups; SD card OK |
| Radio     | SX1262 Pi HAT **or** USB MeshCore companion device |
| OS        | Linux - Raspberry Pi OS / Debian tested. Other Unixes likely work. |
| Python    | 3.10+ - **Pi HAT mode only** (for `pymc_core`) |

The BBS itself is a single static Rust binary with no runtime dependencies.
Python is only required when using a Pi HAT
([ADR-0007](adr/0007-bridge-stays-pymc-core.md)). USB device operators need
nothing else ([ADR-0013](adr/0013-native-serial-transport-for-usb-devices.md)).

## Architecture at a glance

Supply Drop BBS supports two deployment topologies depending on your radio
hardware.

### USB device (single-process)

```
   ┌──────────────────────────────────────────────────┐
   │  supply-drop-bbs  (Rust - one process)           │
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

The BBS speaks the companion-frame protocol directly over the serial port.
No bridge process, no Python. One service to manage.

### Pi HAT (two-process)

```
   ┌──────────────────────┐         ┌────────────────────────┐
   │  pymc-companion      │         │  supply-drop-bbs       │
   │  (Python - pymc_core │◄─TCP──► │  (Rust BBS host)       │
   │  CompanionRadio +    │         │                        │
   │  CompanionFrameServer│         │  also exposes:         │
   │                      │         │  - web UI (opt-in)     │
   │  manages GPIO/SPI    │         └────────────────────────┘
   │  for the LoRa HAT    │
   └──────────────────────┘
            │
            ▼
      SX1262 LoRa HAT
      (ZebraHat, Waveshare, PiMesh, …)
```

`pymc-companion` owns the radio hardware. The BBS connects to it over
`127.0.0.1:5000`. Two independent processes - either can restart without
breaking the other.

## Installation

Before installing, have ready:

- **Radio type** — USB companion device or Pi HAT
- **HAT model** — if using a Pi HAT (ZebraHat, Waveshare, PiMesh, etc.)
- **Region / frequency** — US (910.525 MHz) or EU (869.618 MHz), or your local frequency

### Option 1 — Debian package (recommended)

The `.deb` package is the easiest way to install on Raspberry Pi OS, Ubuntu,
or any Debian-based system. It creates the service user, sets up the directory
layout, and registers the systemd unit — no cloning required.

**One-liner install** — auto-detects your architecture:

```sh
ARCH=$(dpkg --print-architecture)   # arm64, armhf, or amd64
curl -fsSL \
  "https://github.com/Mesh-America/supply-drop-bbs/releases/latest/download/supply-drop-bbs_${ARCH}.deb" \
  -o supply-drop-bbs.deb
sudo dpkg -i supply-drop-bbs.deb
sudo supply-drop-bbs setup
sudo systemctl start supply-drop-bbs
```

Or download manually from the
[latest release](https://github.com/Mesh-America/supply-drop-bbs/releases/latest)
if you prefer to inspect the file first:

| Hardware | File |
|---|---|
| Raspberry Pi 4 / 5 (64-bit) | `supply-drop-bbs_arm64.deb` |
| Raspberry Pi 2 / 3 / Zero 2 (32-bit) | `supply-drop-bbs_armhf.deb` |
| x86-64 Linux | `supply-drop-bbs_amd64.deb` |

The package:

1. Creates the `supply-drop` system user
2. Creates `/var/lib/supply-drop-bbs` (data) and `/etc/supply-drop-bbs` (config)
3. Installs the `supply-drop-bbs.service` systemd unit and enables it

Then run the setup wizard and start:

```sh
sudo supply-drop-bbs setup
sudo systemctl start supply-drop-bbs
```

::: info Pi HAT users
The `.deb` installs the BBS itself. For Pi HAT support you still need
`pymc-companion` (the Python radio bridge). After the BBS is running, follow
the [Pi HAT section](#pi-hat-additional-wizard-steps-in-the-installer) below,
or use the guided setup script (Option 3) which handles the full HAT setup
automatically.
:::

### Option 2 — Raw binary

Works on **any systemd-based Linux** — not just Debian. Download the binary,
verify its checksum, then install it and the service unit manually.

```sh
# Set these for your system
TAG=v0.6.1   # replace with the latest release tag
ARCH=$(uname -m)
case "$ARCH" in
  aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
  armv7l)  TARGET="armv7-unknown-linux-gnueabihf" ;;
  x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
esac

# Download binary and checksum file
curl -fsSL \
  "https://github.com/Mesh-America/supply-drop-bbs/releases/download/${TAG}/supply-drop-bbs-${TAG}-${TARGET}" \
  -o supply-drop-bbs
curl -fsSL \
  "https://github.com/Mesh-America/supply-drop-bbs/releases/download/${TAG}/SHA256SUMS" \
  -o SHA256SUMS

# Verify before installing
grep "supply-drop-bbs-${TAG}-${TARGET}" SHA256SUMS | sha256sum -c

# Install binary and service unit
sudo install -m 755 supply-drop-bbs /usr/local/bin/supply-drop-bbs
sudo useradd --system --no-create-home --shell /sbin/nologin \
  --home-dir /var/lib/supply-drop-bbs supply-drop 2>/dev/null || true
sudo mkdir -p /var/lib/supply-drop-bbs /etc/supply-drop-bbs
sudo chown supply-drop:supply-drop /var/lib/supply-drop-bbs
sudo curl -fsSL \
  "https://raw.githubusercontent.com/Mesh-America/supply-drop-bbs/${TAG}/supply-drop-bbs.service" \
  -o /lib/systemd/system/supply-drop-bbs.service
sudo systemctl daemon-reload
sudo systemctl enable supply-drop-bbs

# Run setup then start
sudo supply-drop-bbs setup
sudo systemctl start supply-drop-bbs
```

Append `-headless` to the binary name for a smaller build without the admin web UI (e.g. `supply-drop-bbs-${TAG}-${TARGET}-headless`).

### Option 3 — Guided setup script

The `install.sh` script handles the full stack including Pi HAT configuration
(`pymc_core`, SPI, `pymc-companion.service`). Download and read it before running:

```sh
curl -fsSL https://raw.githubusercontent.com/Mesh-America/supply-drop-bbs/main/install.sh \
  -o install.sh
less install.sh    # review before running
sudo bash install.sh
```

#### What the script does

1. Installs minimal system packages (`curl`, `git`, `figlet`)
2. Clones (or updates) the repository to `/opt/supply-drop-bbs`
3. Downloads the pre-built binary for your architecture and verifies its SHA256 checksum
4. Falls back to building from source if no pre-built binary is available (5–15 min on a Pi)
5. Installs the binary to `/usr/local/bin/supply-drop-bbs`
6. Creates the `supply-drop` system user and the config/data directories
7. Installs the `supply-drop-bbs.service` systemd unit
8. Runs the **setup wizard**
9. **Pi HAT only:** installs `pymc_core` in a Python venv, writes `pymc-companion.yaml`, and enables `pymc-companion.service`
10. Enables and starts both services

### What the setup wizard asks

1. **Radio connection type** - USB serial or Pi HAT
2. **Serial port** *(USB only)* - detected automatically; you confirm or enter manually
3. **BBS name** - displayed to users on connect
4. **Data directory** - defaults to `/var/lib/supply-drop-bbs`
5. **Web admin UI** - whether to enable it, and if so, the password and bind address

The wizard writes `/etc/supply-drop-bbs/config.toml`. Run
`supply-drop-bbs setup` at any time to reconfigure.

### Pi HAT additional wizard steps (in the installer)

After the BBS wizard, the installer asks:

1. **Region / frequency** - US, EU, or enter manually
2. **HAT model** - choose from the supported list

The installer then:

- Enables SPI via `raspi-config` if not already active
- Creates `/opt/pymc-companion/venv` with `pymc_core`, `spidev`, and `lgpio`
- Writes `/etc/supply-drop-bbs/pymc-companion.yaml` with your HAT's pin config
- Installs and enables `pymc-companion.service`

### Supported Pi HATs

| # | Model | Notes |
|---|-------|-------|
| 1 | ZebraHat 1W | wehooper4 |
| 2 | Waveshare SX1262 LoRa HAT | |
| 3 | PiMesh-1W (V1) | |
| 4 | PiMesh-1W (V2) | |
| 5 | MeshAdv Mini | |
| 6 | MeshAdv | |
| 7 | FemtoFox SX1262 1W | Uses gpiod backend |
| 8 | FemtoFox SX1262 2W | Uses gpiod backend |
| 9 | NebraHat 2W | |
| 10 | RAK6421 + RAK13300x (Slot 1) | Uses gpiod backend |
| 11 | RAK6421 + RAK13300x (Slot 2) | Uses gpiod backend |
| 12 | Zindello UltraPeater E22 | Uses gpiod backend |
| 13 | Zindello UltraPeater E22P | Uses gpiod backend |
| 14 | uConsole LoRa Module v1 | |
| 15 | uConsole LoRa Module v2 | |

### After installation

Check both services are running:

```sh
sudo systemctl status supply-drop-bbs
sudo systemctl status pymc-companion   # Pi HAT only
```

Tail the logs:

```sh
sudo journalctl -u supply-drop-bbs -f
sudo journalctl -u pymc-companion -f   # Pi HAT only
```

If the web admin UI is enabled, open it at `http://<your-pi-ip>:8080`.

### Reconfigure

Re-run the BBS setup wizard at any time:

```sh
sudo supply-drop-bbs setup --config /etc/supply-drop-bbs/config.toml
sudo systemctl restart supply-drop-bbs
```

To change the HAT or frequency, edit `/etc/supply-drop-bbs/pymc-companion.yaml`
and restart:

```sh
sudo systemctl restart pymc-companion
```

### Building from source (manual)

Required: Rust 1.88+ (`rustup install 1.88`).

```sh
git clone https://github.com/Mesh-America/supply-drop-bbs
cd supply-drop-bbs
cargo build --release
sudo install -m 0755 target/release/supply-drop-bbs /usr/local/bin/
supply-drop-bbs setup
```

Use `--profile release-min` instead of `--release` to produce a smaller
binary with the same settings used in the official releases (`opt-level = "z"`,
`lto = "thin"`, debug symbols stripped).

## Uninstall

### Debian package uninstall

```sh
sudo dpkg -r supply-drop-bbs
```

This stops the service and removes the binary and systemd unit. Config
(`/etc/supply-drop-bbs`) and data (`/var/lib/supply-drop-bbs`) are
**intentionally preserved** — remove them manually when you're sure you no
longer need them:

```sh
sudo rm -rf /etc/supply-drop-bbs
sudo rm -rf /var/lib/supply-drop-bbs   # contains your message store!
sudo userdel supply-drop 2>/dev/null || true
```

### Script-installed uninstall

If you installed via `install.sh`, use the built-in uninstall flag:

```sh
sudo bash /opt/supply-drop-bbs/install.sh --uninstall
```

The uninstaller:

1. Stops and disables `supply-drop-bbs` and `pymc-companion`
2. Removes their systemd unit files
3. Removes the binary (`/usr/local/bin/supply-drop-bbs`)
4. Removes `/opt/pymc-companion` and `/opt/supply-drop-bbs`
5. **Asks before deleting** the config directory (`/etc/supply-drop-bbs`)
6. **Asks before deleting** the data directory (`/var/lib/supply-drop-bbs`) - this contains your message store and identity key
7. **Asks before removing** the `supply-drop` system user

If you answer N to the data directory prompt, your messages and identity are
preserved and can be used with a fresh install.

## systemd units

### USB device - one service

**`supply-drop-bbs.service`**

```ini
[Unit]
Description=Supply Drop BBS
After=network.target
Wants=network.target

[Service]
Type=simple
User=supply-drop
Group=supply-drop
SupplementaryGroups=dialout
ExecStart=/usr/local/bin/supply-drop-bbs run --config /etc/supply-drop-bbs/config.toml
Restart=on-failure
RestartSec=5s
StandardOutput=journal
StandardError=journal
SyslogIdentifier=supply-drop-bbs
ReadWritePaths=/var/lib/supply-drop-bbs
NoNewPrivileges=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
```

### Pi HAT - two services

**`pymc-companion.service`** starts first (the BBS connects to it):

```ini
[Unit]
Description=pymc-companion - LoRa radio bridge for Supply Drop BBS
After=network.target
Before=supply-drop-bbs.service

[Service]
Type=simple
User=supply-drop
Group=supply-drop
SupplementaryGroups=dialout spi gpio
ExecStart=/opt/pymc-companion/venv/bin/python \
    /opt/pymc-companion/pymc-companion.py \
    --config /etc/supply-drop-bbs/pymc-companion.yaml
Restart=on-failure
RestartSec=5s
StandardOutput=journal
StandardError=journal
SyslogIdentifier=pymc-companion
ReadWritePaths=/var/lib/supply-drop-bbs

[Install]
WantedBy=multi-user.target
```

**`supply-drop-bbs.service`** (HAT variant) waits for pymc-companion:

```ini
[Unit]
Description=Supply Drop BBS
After=network.target pymc-companion.service
Wants=network.target

[Service]
...same as USB variant above...
```

The two services are independent at the socket level - the BBS reconnects
automatically if `pymc-companion` restarts.

## Update

### Debian package update (recommended)

`dpkg` stops the running service, replaces the binary, and restarts it automatically — your config and data are untouched.

```sh
ARCH=$(dpkg --print-architecture)
curl -fsSL \
  "https://github.com/Mesh-America/supply-drop-bbs/releases/latest/download/supply-drop-bbs_${ARCH}.deb" \
  -o supply-drop-bbs.deb
sudo dpkg -i supply-drop-bbs.deb
```

**Typical update time: under a minute.**

### Manual binary-only update

For non-Debian systems, download the new binary directly:

```sh
ARCH=$(uname -m)
case "$ARCH" in
  aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
  armv7l)  TARGET="armv7-unknown-linux-gnueabihf" ;;
  x86_64)  TARGET="x86_64-unknown-linux-gnu" ;;
esac

TAG=$(curl -sSf https://api.github.com/repos/Mesh-America/supply-drop-bbs/releases/latest \
  | python3 -c "import sys,json; print(json.load(sys.stdin)['tag_name'])")

curl -fsSL \
  "https://github.com/Mesh-America/supply-drop-bbs/releases/download/${TAG}/supply-drop-bbs-${TAG}-${TARGET}" \
  -o /tmp/supply-drop-bbs-new
curl -fsSL \
  "https://github.com/Mesh-America/supply-drop-bbs/releases/download/${TAG}/SHA256SUMS" \
  | grep "${TARGET}" | sha256sum -c

sudo systemctl stop supply-drop-bbs
sudo install -m 755 /tmp/supply-drop-bbs-new /usr/local/bin/supply-drop-bbs
sudo systemctl start supply-drop-bbs
supply-drop-bbs --version
```

### Guided script update

If you originally installed via the setup script and want it to handle everything (including updating `pymc-companion` for Pi HAT users):

```sh
curl -fsSL https://raw.githubusercontent.com/Mesh-America/supply-drop-bbs/main/install.sh \
  -o install.sh
less install.sh    # review before running
sudo bash install.sh
```

When asked **"Reconfigure now?"**, answer **N** to keep your existing config unchanged.

### Source build update

Use this path only if no pre-built binary is available for your architecture:

```sh
sudo systemctl stop supply-drop-bbs
cd /opt/supply-drop-bbs
sudo git pull
sudo cargo build --release
sudo install -m 0755 target/release/supply-drop-bbs /usr/local/bin/
sudo systemctl start supply-drop-bbs
```

## Backups

### Automatic backups

If `[backup] enabled = true` (default), the BBS runs
`VACUUM INTO 'backup-YYYY-MM-DD-HHMMSS.sqlite'` on the configured interval.
Backups land in `<data_dir>/backups`. `VACUUM INTO` is non-blocking.

Retention defaults: 7 daily + 4 weekly. Configurable.

### Manual backup

```sh
supply-drop-bbs backup
```

Or use the **Trigger backup** button in the web admin UI.

### Off-host backups

```sh
# Cron: copy the latest backup nightly to a remote host
0 3 * * *  rsync -a /var/lib/supply-drop-bbs/backups/ \
              backup-host:/srv/bbs-backups/$(hostname)/
```

## Rooms and access control

### Built-in rooms

Supply Drop BBS ships with five permanent rooms that cannot be deleted.
Three of them (**Mail**, **Aides**, **Sysop**) also have locked permission
settings — their minimum access level and read-only flag cannot be changed,
even by a sysop.

| Room | ID | Min access | Read-only | Locked |
|------|----|-----------|-----------|--------|
| Lobby | 1 | Unvalidated | No | No |
| Mail | 2 | User | No | **Yes** |
| Aides | 3 | Aide | No | **Yes** |
| Sysop | 4 | Sysop | No | **Yes** |
| System | 5 | Sysop | Yes | No |

**Locked** means the web admin edit form and the REST API will refuse any
attempt to change the permission level or read-only setting for that room.
The description field remains editable on all rooms.

### Mail privacy

Mail is strictly private. Every message stored in Mail has an explicit sender
and recipient; the database query enforces that a user can only retrieve
messages where they are the sender or the recipient. There is no admin view
that bypasses this.

**System notifications** (such as new-user registration alerts) are sent as
mail from the reserved sender `bbs`. The new user is never the sender of their
own registration notification, so they cannot see it in their own inbox.
Only the addressed sysops receive it.

### User-created rooms

Rooms created with `.C <name>` (in-session) or `supply-drop-bbs room create`
(CLI) start with:

- **min permission level** — User (10)
- **read-only** — false

Sysops can change these through the web admin **Rooms** page or a future
in-session command. Any min-level from Unvalidated (0) to Sysop (100) is
valid.

### Web admin — Rooms page

The Rooms table shows all rooms. For **Mail**, **Aides**, and **Sysop** the
edit form shows only the description field; the permission-level and read-only
controls are hidden. Any API attempt to set those fields on those rooms returns
`422 Unprocessable Entity`.

## Disaster recovery

### Corrupted database

```sh
sudo systemctl stop supply-drop-bbs
sudo mv /var/lib/supply-drop-bbs/bbs.sqlite /var/lib/supply-drop-bbs/bbs.sqlite.corrupt
sudo mv /var/lib/supply-drop-bbs/bbs.sqlite-wal /var/lib/supply-drop-bbs/bbs.sqlite-wal.corrupt 2>/dev/null || true
ls -lt /var/lib/supply-drop-bbs/backups/
sudo cp /var/lib/supply-drop-bbs/backups/<latest>.sqlite /var/lib/supply-drop-bbs/bbs.sqlite
sudo chown supply-drop:supply-drop /var/lib/supply-drop-bbs/bbs.sqlite
sudo systemctl start supply-drop-bbs
```

### Lost sysop access

```sh
sudo systemctl stop supply-drop-bbs
sudo supply-drop-bbs user promote <username> \
  --config /etc/supply-drop-bbs/config.toml
sudo systemctl start supply-drop-bbs
```

### Reset a user's password

Three options, from most to least convenient:

**In-session** (BBS must be running; no server access needed):

A logged-in sysop types `.PW <username>` and follows two prompts (new
password, then confirm). The change takes effect immediately.

**From the web admin UI** (BBS must be running; requires sysop login):

Open **Users**, click the username to open the detail drawer, then click **reset password**.

**From the CLI** (BBS does not need to be stopped):

```sh
sudo supply-drop-bbs user set-password <username> \
  --config /etc/supply-drop-bbs/config.toml
```

The new password must be at least 8 characters. The action is audit-logged when performed via the web UI.

See [CLI.md](CLI.md) for the full `user` subcommand reference.

## Monitoring

### Health endpoint

If the web admin plugin is enabled, `GET /health` returns:

```json
{
  "status": "healthy",
  "uptime_seconds": 1234567,
  "version": "0.1.0",
  "bridge_connected": true,
  "transports": { "mesh": "running", "web": "running" },
  "db": { "size_bytes": 12345678, "last_backup": "2026-05-08T03:00:00Z" }
}
```

`status` is `"healthy"` only if every transport reports running and the bridge
is connected; otherwise `"degraded"`.

### Logs

```sh
journalctl -u supply-drop-bbs -f                             # tail
journalctl -u supply-drop-bbs --since "1 hour ago" -p err   # errors only
journalctl -u pymc-companion -f                              # Pi HAT radio bridge
```

The default log level is `INFO`. To increase verbosity temporarily, change
the **Log level** dropdown in the web admin **Settings** page — the change
takes effect immediately without restarting the service. To make it permanent,
edit `[logging] level = "DEBUG"` in `config.toml`.

## Reverse-proxy setup (HTTPS)

The web admin plugin doesn't terminate TLS. Use nginx or Caddy:

### Caddy

```
admin.bbs.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

```toml
[plugins.web]
bind = "127.0.0.1:8080"
external_origin = "https://admin.bbs.example.com"
cookie_secure = true
```

### nginx

**TBD**

## Troubleshooting

### BBS won't start

```sh
supply-drop-bbs config check
sudo journalctl -u supply-drop-bbs -n 100 --no-pager
```

Look for the first `ERROR` line. Subsequent errors are usually cascading from it.

### Radio bridge disconnected

**USB device:**

```sh
ls -la /dev/ttyACM* /dev/ttyUSB*
sudo journalctl -u supply-drop-bbs -f
```

Common causes: wrong `serial_port` in config; device not in `dialout` group
(`sudo usermod -aG dialout supply-drop`); firmware crashed (unplug and replug).

**Pi HAT:**

```sh
sudo systemctl status pymc-companion
sudo journalctl -u pymc-companion -f
```

Common causes:

- SPI not enabled - `sudo raspi-config` → Interface Options → SPI
- `supply-drop` user not in `spi`/`gpio` groups (installer adds these; a reboot may be needed)
- Missing Python dependency - `sudo /opt/pymc-companion/venv/bin/pip install spidev lgpio`
- Wrong HAT selected - edit `/etc/supply-drop-bbs/pymc-companion.yaml` and restart pymc-companion

### Database locked

Almost always means two BBS instances are running against the same DB:

```sh
sudo systemctl status supply-drop-bbs
ps -ef | grep supply-drop-bbs
```

### Web UI returns 502

```sh
systemctl status supply-drop-bbs
ss -ltnp | grep supply-drop-bbs
```

Check `[plugins.web] bind` and `external_origin` in config.

## Where to get help

- **Bugs:** GitHub Issues
- **Security:** see [SECURITY.md](../SECURITY.md)
- **General questions:** GitHub Discussions

## Versioning

Pre-1.0: each release may include breaking changes - read the release notes.

After 1.0: semver. Major bumps are breaking; minor releases add features
compatibly; patches are bug fixes only.
