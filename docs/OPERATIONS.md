# Operations guide

How to install, configure, update, back up, and remove Supply Drop BBS.

::: tip Quick install
Auto-detects your architecture (`arm64`, `armhf`, or `amd64`):

```sh
ARCH=$(dpkg --print-architecture)
curl -fsSL \
  "https://github.com/Mesh-America/supply-drop-bbs/releases/latest/download/supply-drop-bbs_${ARCH}.deb" \
  -o supply-drop-bbs.deb
sudo dpkg -i supply-drop-bbs.deb
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
| Python    | 3.10+ - **Pi HAT mode only** (for `openhop_core`) |

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
   ┌─────────────────────────┐      ┌────────────────────────┐
   │  pymc-companion         │      │  supply-drop-bbs       │
   │  (Python - openhop_core)│◄TCP► │  (Rust BBS host)       │
   │  CompanionRadio +       │      │                        │
   │  CompanionFrameServer   │      │  also exposes:         │
   │                         │      │  - web UI (opt-in)     │
   │  manages GPIO/SPI       │      └────────────────────────┘
   │  for the LoRa HAT       │
   └─────────────────────────┘
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
The `.deb` installs the BBS itself, but it does **not** perform first-time
`pymc-companion` (the Python radio bridge) setup — the `.deb` and raw-binary
methods only ever touch the `supply-drop-bbs` binary on a fresh install (a
[`.deb` update](#debian-package-update-recommended) *will* keep an
already-configured `pymc-companion` current, but that's not the same as
setting one up). If you need Pi HAT support, after the BBS is running use
[Option 3 — Guided setup script](#option-3--guided-setup-script), which
installs and configures `pymc-companion` for you (see [what the Pi HAT
steps do](#pi-hat-additional-wizard-steps-in-the-installer)). There is
currently no supported way to set up the Pi HAT bridge for the first time
without it.
:::

### Option 2 — Raw binary

Works on **any systemd-based Linux** — not just Debian. Download the binary,
verify its checksum, then install it and the service unit manually.

```sh
# Set these for your system
TAG=v0.12.0   # replace with the latest release tag
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
# Config dir ownership is set automatically by 'sudo supply-drop-bbs setup'.
# If you create config.toml manually, fix ownership so the web admin can save changes:
sudo chown supply-drop:supply-drop /etc/supply-drop-bbs /etc/supply-drop-bbs/config.toml
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
(`openhop_core`, SPI, `pymc-companion.service`). Download and read it before running:

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
4. Falls back to building from source if no pre-built binary is available (5–15 min on a Pi); see [Node.js for the source build](#nodejs-for-the-source-build)
5. Installs the binary to `/usr/local/bin/supply-drop-bbs`
6. Creates the `supply-drop` system user and the config/data directories
7. Installs the `supply-drop-bbs.service` systemd unit
8. Runs the **setup wizard**
9. **Pi HAT only:** installs `openhop_core` in a Python venv, writes `pymc-companion.yaml`, and enables `pymc-companion.service`
10. Enables and starts both services

#### Node.js for the source build

Building from source compiles the web admin UI, which needs npm and a Node.js
the web build's dependencies accept: 18.17 or newer on the 18 line, 20.3 or
newer on the 20 line, or 22 and later. The installer keeps an existing Node.js
when `node` and `npm` both run as root, `node` is a Linux build in one of those
ranges, and neither binary is the Windows copy that WSL adds to the `PATH` under
`/mnt`. That includes a NodeSource install. Otherwise it installs `nodejs` (and
`npm`, where the distribution ships it separately) from apt.

A Node.js installed with nvm in your home directory is usually not visible to
the installer: `sudo` resets `PATH`, so root doesn't see `~/.nvm`. The installer
then installs its own from apt, which may be too old on some distributions
(Ubuntu 22.04 ships 12). It warns when it ends up with a Node.js the web build
can't use, and you can install a newer one system-wide (for example from
NodeSource) and re-run it.

### What the setup wizard asks

1. **Radio connection type** - USB serial or Pi HAT
2. **Serial port** *(USB only)* - detected automatically; you confirm or enter manually. A radio that reports its own USB serial number is offered by its stable `/dev/serial/by-id/...` name (with the `/dev/ttyACMn` it currently is shown for reference), and that name is what is written to the config. Radios without a unique serial number are offered by their plain path and flagged, and if your current config names a port that is not plugged in, a "Keep current" row is offered first
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
- Creates `/opt/pymc-companion/venv` with `openhop_core`, `spidev`, and `lgpio`
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
| 7 | FemtoFox SX1262 1W | Uses gpiod backend — see note below |
| 8 | FemtoFox SX1262 2W | Uses gpiod backend — see note below |
| 9 | NebraHat 2W | |
| 10 | RAK6421 + RAK13300x (Slot 1) | Uses gpiod backend — see note below |
| 11 | RAK6421 + RAK13300x (Slot 2) | Uses gpiod backend — see note below |
| 12 | Zindello UltraPeater E22 | Uses gpiod backend — see note below |
| 13 | Zindello UltraPeater E22P | Uses gpiod backend — see note below |
| 14 | uConsole LoRa Module v1 | |
| 15 | uConsole LoRa Module v2 | |

::: warning gpiod backend — known upstream bug
Presets 7, 8, and 10–13 above set `use_gpiod_backend: true`. `openhop_core`'s
`GPIOPinManager` only wires up its gpiod support under automatic backend
detection, not when a caller explicitly requests the gpiod backend the way
`SX1262Radio` does here — the module-level `GPIO` name is left bound to
whatever it was at import (`None`, on these presets, since `install.sh`'s
gpiod branch doesn't install `python-periphery` either), so the first real
GPIO call fails with `TypeError: 'NoneType' object is not callable`, not a
more obviously-diagnostic error. If you're on one of these presets and hit
that error, this is a known, tracked issue (supply-drop-bbs-85n /
[#234](https://github.com/Mesh-America/supply-drop-bbs/issues/234)), not
something specific to your setup — it needs an upstream fix in
`openhop_core`, which this project doesn't control.
:::

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

Required: Rust 1.96 (the version pinned in `rust-toolchain.toml`; rustup installs it on the first build), and npm with a supported Node.js
(18.17+, 20.3+ or 22+; see [Node.js for the source build](#nodejs-for-the-source-build)),
because the default build compiles the web admin UI.

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
Restart=always
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

> **Web "Restart service" button.** The web admin's restart action works by having
> the BBS process **exit**, which `Restart=always` then brings back up — it does
> *not* shell out to `systemctl`, because the unit runs as a non-root user with
> `NoNewPrivileges=yes` (so `sudo` can't escalate). No sudoers rule is needed; if
> an older install left `/etc/sudoers.d/supply-drop-bbs`, it's removed on upgrade.

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

`dpkg` stops the running service, replaces the binary, and restarts it automatically — your config and data are untouched. If a Pi HAT bridge (`pymc-companion`) is already configured on this host, it's brought up to date too — script, unit, and `openhop-core` dependency — with no separate step needed.

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

Use this path only if no pre-built binary is available for your architecture.
The build needs the same Node.js and npm as the installer's source build (see
[Node.js for the source build](#nodejs-for-the-source-build)):

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

If `[backup] enabled = true` (default), the BBS takes a backup on the configured
interval. Each backup is one `backup_YYYYMMDD_HHMMSS.zip` in `<data_dir>/backups`
holding the database (made with `VACUUM INTO`, which is non-blocking) and the
`config.toml` the BBS is running with, so the settings (the BBS name, welcome
message, timezone, location and so on) are backed up with the data. The bundle
is readable only by the service user, since the database holds password hashes.
If the config file can't be found or read, the BBS logs a warning and the backup
holds the database alone; the Backups page marks the ones that include settings.

Retention defaults: 7 daily + 4 weekly. Configurable.

### Manual backup

```sh
supply-drop-bbs backup
```

Or use the **create backup** button on the **Backups** page of the web admin UI.
Both make the same `.zip` bundle as the automatic backups. `supply-drop-bbs
backup` includes the config file it loaded, so pass `--config` if it is not in
one of the default places.

To see and remove backups from the command line, use `supply-drop-bbs backup
list` and `supply-drop-bbs backup delete <name>`; they work even when the
database is broken. See [the CLI reference](CLI.md).

### Off-host backups

```sh
# Cron: copy the latest backup nightly to a remote host
0 3 * * *  rsync -a /var/lib/supply-drop-bbs/backups/ \
              backup-host:/srv/bbs-backups/$(hostname)/
```

### Restoring from a backup

```sh
supply-drop-bbs restore stage /var/lib/supply-drop-bbs/backups/<file>.db
supply-drop-bbs restore apply
sudo systemctl restart supply-drop-bbs
```

Or use the **Backups** page in the web admin UI. To restore a backup listed
there, click its **restore** button and confirm. The backup is validated,
staged and confirmed in one step, and the service then restarts on the
restored database. That restart needs the BBS running under systemd;
otherwise the restore is confirmed and the page tells you to restart the BBS
yourself, which applies it. While the restore applies, the page shows a
full-screen notice and blocks everything else, then reloads by itself once the
restarted service answers (to the login screen, since sessions don't survive a
restart). If it hasn't seen the restart after two minutes it offers a reload
button. To restore a backup from another system, upload
the file instead (a `.zip` bundle from a backup, or a raw `.db`), then confirm the restore once it validates. From the
command line, or with an upload, staging and confirming are separate steps:
nothing changes until you confirm, and the database is only swapped the next
time the BBS process starts. The command line works even when the live database
is broken (see [Corrupted database](#corrupted-database)) — staging and
confirming never require the live database to open successfully; the web UI
needs a running BBS.

A restore brings back the database and, from a `.zip` bundle that has one, the
settings in its `config.toml`. Anything written since the backup was made is
lost. Backups made before bundles carried the settings (bare `.db` files, or
older zips without a config) restore the database only.

Some settings belong to this machine, or decide what it runs and who can reach
it, so they are not taken from the backup: they keep this machine's current
values (or stay unset if this machine doesn't set them). They are:

- where things live: `bbs.data_dir`, `logging.file`, and the whole `[database]`
  and `[backup]` sections (which also keeps this machine's backup schedule and
  retention);
- how the BBS is reached: the whole `[plugins.web]` and `[plugins.cli]` tables;
- what it executes: `[[plugins.process]]`;
- the hardware and its cost settings: each radio's `connection_type`, `addr`,
  `serial_port` and `baud_rate`, its `[radio]` settings, whether it is
  `enabled`, MeshCore's `app_target_version` and `advert_scope` (the region its
  floods are scoped to), and each radio's
  `protected_contact_cap` (all describe the device on this machine), and the
  whole `[security]` section (password-hashing cost is tuned to the machine);
- the radio's name: Meshtastic's `short_name` and `long_name`, which the BBS
  reconciles with the radio on every connect, so a backup from another node
  would rename this one (and reboot it);

A key this machine doesn't set stays unset, so a radio that isn't `enabled`
in this machine's config gets the built-in default (MeshCore on, Meshtastic
off), not the backup's value.

A backup taken on another host, or a hostile one, would otherwise point the
admin UI or a radio somewhere that doesn't exist here, or run a command. Everything
else comes from the backup: the other `[bbs]` settings (name, welcome message,
starting room, timezone and so on), `[location]`, `[logging]` apart from the
file, and the rest of the mesh and Meshtastic settings.

The web UI has a checkbox, on by default, to restore the settings; clear it (or
use `supply-drop-bbs restore apply --no-config`) to restore the database only.
The settings are applied when the BBS starts, after the database swap. The
config file it replaces is saved as `config.toml.pre-restore` next to it (next to the real file, if `config.toml` is a symlink). If the
restored file doesn't load, or the service user can't write the config file,
the previous settings stay, the database restore still stands, the log says why
and the audit log entry `restore_completed` records it. The log level and log
format from the backup take effect at the restart after that.

Staging a restore checks the file without loading it into memory, and a `.zip` is
streamed to disk while it is extracted. The web UI restores files up to 2 GiB,
whether uploaded or already in the backup directory; `restore stage` on the CLI
has no limit on the file. Everywhere, a database larger than 4 GiB, raw or
inside a zip, is refused. Extracting a zip and copying a staged file between
filesystems first check that the disk has room (and extraction re-checks as it
goes), failing with a message instead of leaving a half-written file; the check
is skipped if the free space can't be read.

The web upload makes the same check before it starts writing and again as it
goes, and keeps 16 MiB free for the rest of the system. Copies left behind by a
restore that was cut off are removed when the BBS starts: the web upload and
backup copies (`restore_upload_*.tmp`, `restore_backup_*.tmp`) at once, and the
CLI's (`restore_cli_*.tmp`), the apply step's `pre-restore-safety-*.partial`
snapshot copy and the files it leaves beside the database (`<db>-wal.restore-aside`,
`<db>-shm.restore-aside` and `<db>.restore.tmp`, by those exact names) once they
have gone an hour untouched.

Before the swap, the current database is copied to
`pre-restore-safety-<unix-seconds>.db` in the data directory. The three most
recent snapshots are kept and older ones are deleted after each restore, so the
snapshot of your original data survives a second restore. The snapshots and
`pending_restore*` files are not backups: if the backup directory is the data
directory they are not listed, downloaded, deleted or staged by name, so stage a
snapshot by its path, as below. To roll back to a snapshot, restore it like any
other backup:

```sh
supply-drop-bbs restore stage /var/lib/supply-drop-bbs/pre-restore-safety-<unix-seconds>.db
supply-drop-bbs restore apply
sudo systemctl restart supply-drop-bbs
```

If a `pre-restore-safety-<unix-seconds>.db-wal` file sits beside the snapshot, the
database it was taken from could not be opened (it was probably damaged), so its
`-wal` file was kept as it was rather than lost. Keep the two together. That file
holds commits the `.db` alone does not, and staging the `.db` by itself, as the
commands above do, leaves them out. To get at them, open the pair together with
the `sqlite3` tool (for a damaged one, its `.recover` command) before staging.

A restore that cannot be applied never stops the BBS from starting. If there is
no room for the snapshot, the live database is open in another process, or the
snapshot or the swap fails, the live database is left as it was, the log says
`database restore NOT applied` with the reason, an audit log entry
`restore_failed` records it, and the confirmed file is renamed to
`pending_restore.failed.db` so it is not retried on every start. The web page
that confirmed the restore cannot see this, so check the audit log or the logs
if the data did not change. Fix the cause and stage the restore again, or delete
`pending_restore.failed.db`. A successful restore is recorded as
`restore_completed`. Staging also runs SQLite's integrity check on the file and
refuses one with damaged pages, which takes time proportional to its size.

A restore that is confirmed but not yet applied (the BBS wasn't restarted)
is a file named `pending_restore.db` in the data directory. Delete it before
the next start to cancel the restore.

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

### Open access and guest rooms (SHTF mode)

In normal operation every new account must be verified by an aide or sysop
before the user can read or post in any room.  When sysops are unreachable
(off-grid emergency / SHTF scenario) two complementary knobs keep the BBS
useful.

#### `require_verify = false` — open access

All registrations are immediately treated as `User`-level.  No aide or sysop
action is required.  Set this in `config.toml`:

```toml
[bbs]
require_verify = false
```

Or via the CLI (restart required):

```sh
sudo supply-drop-bbs config require-verify off \
  --config /etc/supply-drop-bbs/config.toml
sudo systemctl restart supply-drop-bbs
```

Or live from within the BBS (sysop session, no restart):

```
OPENACCESS          # disable verification immediately
CLOSEACCESS         # restore verification immediately
```

The web admin **Settings → Access policy** card also exposes this toggle.

#### `guest_room = "Guests"` — guest room confinement

Unverified users land in a single designated room and can only read and post
there.  All other rooms and mail are invisible until a sysop verifies them.
The room is created automatically on startup if it does not already exist.

```toml
[bbs]
guest_room = "Guests"
```

Or via the CLI (restart required):

```sh
sudo supply-drop-bbs config guest-room Guests \
  --config /etc/supply-drop-bbs/config.toml
sudo systemctl restart supply-drop-bbs
```

Or live from within the BBS (sysop session, no restart):

```
GUESTROOM Guests    # set guest room (created automatically if needed)
GUESTROOM OFF       # disable guest room
```

The web admin **Settings → Access policy** card also exposes this field.

#### Composing the two options

The options are independent and compose naturally:

| `require_verify` | `guest_room` set | Behaviour |
|-----------------|-----------------|-----------|
| `true` (default) | no | Default strict mode — no access until verified |
| `true` | yes | Unverified users allowed into the guest room only |
| `false` | no | All registrations get full access immediately |
| `false` | yes | Full access immediately; guest room exists as an ordinary room |

## Disaster recovery

### Corrupted database

Preferred: `restore stage`/`restore apply` work even when the live database
won't open — staging and confirming never touch it directly, and a safety
snapshot of whatever's currently at the live path is taken automatically
before the swap.

```sh
sudo -u supply-drop supply-drop-bbs restore stage /var/lib/supply-drop-bbs/backups/<file>.zip
sudo -u supply-drop supply-drop-bbs restore apply --yes
sudo systemctl restart supply-drop-bbs
```

Fallback if the CLI itself won't run (e.g. the binary is missing or the
data directory is inaccessible to the `supply-drop` account):

```sh
sudo systemctl stop supply-drop-bbs
sudo mv /var/lib/supply-drop-bbs/bbs.sqlite /var/lib/supply-drop-bbs/bbs.sqlite.corrupt
sudo mv /var/lib/supply-drop-bbs/bbs.sqlite-wal /var/lib/supply-drop-bbs/bbs.sqlite-wal.corrupt 2>/dev/null || true
ls -lt /var/lib/supply-drop-bbs/backups/
sudo unzip -p /var/lib/supply-drop-bbs/backups/<latest>.zip '*.db' > /var/lib/supply-drop-bbs/bbs.sqlite
sudo chown supply-drop:supply-drop /var/lib/supply-drop-bbs/bbs.sqlite
sudo systemctl start supply-drop-bbs
```

To read the settings a backup carries without restoring it, run
`unzip -p <backup>.zip config.toml`.

The fallback skips all of `restore`'s validation (SQLite-format check,
migration-history check, room-structure check) — only use it when the CLI
path genuinely isn't available.

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

If the web admin plugin is enabled, `GET /api/v1/health` answers without a
login:

```json
{ "status": "ok", "boot_id": "5f6c1a5e-8d2b-4c0e-9a1f-3b7e2d4c6a10" }
```

`status` is `"ok"` whenever the web server is answering. `boot_id` is a random
id created when the BBS process starts, so it changes on every restart; the
Backups page uses it to tell that the restart after a restore has finished.
It also lets anyone who can reach the web port see when the BBS restarts. For
uptime, use the authenticated `GET /api/v1/status`.

### Mesh link health

When the web admin UI is enabled, the **Metrics** page includes a **Mesh link
health** section that surfaces reply-delivery quality for the MeshCore
transport — so you can see whether replies are reaching users, which nodes have
bad links, how fast round-trips are, and whether reliability is trending up or
down as you tune the link:

- summary cards — confirm rate (per reply), route failures, total sends,
  gave-up count, and average round-trip latency
- a **worst-links** table — per-node delivery stats, joined to advert names
- a per-minute confirm-rate trend sparkline

The same data backs a sysop-authenticated JSON API (handy for external
dashboards or scripting against a logged-in session):

```
GET /api/v1/transports/meshcore/stats     # current snapshot
GET /api/v1/transports/meshcore/history   # recent ~8h of samples
```

Latency and the per-node breakdown are populated only when reply retransmission
is active (`reply_max_attempts > 1`). Retransmission is **off by default** —
enable it only on a link with a non-zero confirm rate (see
[CONFIG.md](CONFIG.md)). Samples are persisted in the database and pruned after
7 days, so the trend survives a service restart or upgrade rather than resetting.

### Diagnosing message deliverability

When users report that DMs to the BBS are "less reliable than normal messaging"
or that "the BBS didn't respond", the stats snapshot
(`GET /api/v1/transports/meshcore/stats`) carries **inbound** counters alongside
the outbound reply metrics above. Together they localize *where* a message is
being lost — the radio return path or the BBS's own dedup:

- `inbound_received` — plain-text DMs the BBS accepted for processing.
- `dedup_dropped_timestamp` / `dedup_dropped_text` — inbound messages the BBS
  dropped as retransmissions before acting on them.

(Messages queued at the bridge while the BBS was offline are all processed on
reconnect — they get replies like live traffic — so there is no separate
reconnect-loss counter.)

Read them against the outbound `sends_total` / `confirm_rate` / `failed_no_route`
to tell the failure modes apart:

| Symptom | What the numbers show | Likely cause |
|---------|-----------------------|--------------|
| BBS replies, user never sees them | `sends_total` climbs, `confirm_rate ≈ 0`, `failed_no_route ≈ 0` | **Outbound return-path loss.** The device accepts every send but no end-to-end ACK returns — a lossy multi-hop reverse path. Replies are not retried by default (`reply_max_attempts = 1`). |
| BBS "ignores" repeated commands | `dedup_dropped_timestamp` rises against `inbound_received` for that node | **Inbound dedup.** The dedup keys on the timestamp the *sender* stamped. Senders bridged through pyMC are re-stamped whole-second `int(time.time())` at send time, so two identical sends from such a sender in the same second collide and the second is dropped. Senders on real firmware carry their app's stamp, which is normally distinct per send. Confirm with a `DEBUG` log line `dropping retransmitted message (timestamp dedup)`. |

For a live trace, raise the log level to `DEBUG` (Settings page or `[logging]
level = "DEBUG"`) and watch for `mesh: inbound message received` and
`mesh: dropping retransmitted message …`.

> Note: retrying lost **outbound** replies is not as simple as raising
> `reply_max_attempts` — on a link that never confirms, the tracker would
> retransmit every reply to exhaustion and flood the mesh with duplicates. A
> confirm-gated retransmission strategy for far nodes is planned separately.

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

Common causes: wrong `serial_port` in config; **permission denied** on the port
(see [Serial port: Permission denied](#serial-port-permission-denied) below);
firmware crashed (unplug and replug).

**Handshake timeouts after re-attaching a radio, or with two radios:**
`/dev/ttyACM0`, `/dev/ttyACM1` and so on are numbered in the order the radios
attach, so they can swap when a radio is unplugged and plugged back in (on WSL2,
after a `usbipd attach`) or after a reboot. The BBS then speaks each protocol to
the wrong radio and logs `AppStart handshake timeout` or
`payload length ... exceeds MAX_PAYLOAD_SIZE`. Use the stable name instead:

```sh
ls -l /dev/serial/by-id/
```

Copy the entry for the radio into `serial_port`, for example
`serial_port = "/dev/serial/by-id/usb-Heltec_HT-n5262_D42292EF51268EE1-if00"`.
Two boards of the same model have different names, because the name includes the
USB serial number (a board that reports none, or two that share one, cannot be told apart this way). The BBS logs a warning at connect when `serial_port` is a
numbered `/dev/ttyACMn` or `/dev/ttyUSBn` name and a stable alias exists for it.

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

### Serial port: Permission denied

Symptom — the log repeats:

```
could not open serial port /dev/ttyACM0: Permission denied (os error 13)
```

The service (running as the `supply-drop` user) is not in the group that owns the
serial device. Check the owning group:

```sh
ls -l /dev/ttyACM* /dev/ttyUSB*
```

```
crw-rw----+ 1 root plugdev 166, 0 … /dev/ttyACM0
```

The **owning group varies by distro** — `dialout` on many systems, but `plugdev`
on current Raspberry Pi OS / Debian 13 "Trixie". The trailing `+` is a POSIX ACL
that grants the interactive desktop (seat) user; a headless system service never
receives it, so access depends on group membership.

**Packaged installs** (`.deb` / `install.sh`) ship a udev rule
(`/lib/udev/rules.d/70-supply-drop-bbs.rules`) that re-owns the radio's tty to the
`supply-drop` group automatically. If it has not taken effect, reapply it:

```sh
sudo udevadm control --reload-rules && sudo udevadm trigger --subsystem-match=tty
sudo systemctl restart supply-drop-bbs
```

**Manual / raw-binary installs** — add the service user to the owning group, then
restart:

```sh
sudo usermod -aG <owning-group> supply-drop   # e.g. plugdev or dialout
sudo systemctl restart supply-drop-bbs
```

`udevadm trigger` re-applies device ownership without unplugging the radio, but a
**group-membership change takes effect only after the service restarts** (a reboot
always works).

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

### Web admin Settings page shows "Config file is not writable by the server process"

This warning appears when the `supply-drop` service user does not have write
permission on `config.toml` (or its parent directory). The Settings page needs
write access to save configuration changes.

**Fix for new installs — run setup as root:**

```sh
sudo supply-drop-bbs setup --config /etc/supply-drop-bbs/config.toml
```

When run as root, the setup wizard automatically sets the owner of `config.toml`
and its parent directory to `supply-drop:supply-drop` and prints:

```
  ownership set to supply-drop:supply-drop (web admin can save config)
```

**Fix for existing installs — chown manually:**

```sh
sudo chown supply-drop:supply-drop \
  /etc/supply-drop-bbs \
  /etc/supply-drop-bbs/config.toml
```

No restart is required — the Settings page will work immediately after ownership
is corrected.

## Where to get help

- **Bugs:** GitHub Issues
- **Security:** see [SECURITY.md](../SECURITY.md)
- **General questions:** GitHub Discussions

## Versioning

Pre-1.0: each release may include breaking changes - read the release notes.

After 1.0: semver. Major bumps are breaking; minor releases add features
compatibly; patches are bug fixes only.
