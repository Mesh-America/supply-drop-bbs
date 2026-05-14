# Configuration reference

Every configuration knob Supply Drop BBS exposes, with type, default,
and behaviour. The example file at
[`config.example.toml`](../config.example.toml) is a runnable
starting point; this document is the dictionary.

> **Status:** stub. Each section will be fleshed out as the
> corresponding code lands. Sections marked **TBD** are
> placeholders.

## Format and overlay

Format: TOML. See
[ADR-0008](adr/0008-toml-config-with-env-overrides.md).

Configuration sources, in increasing priority:

1. Compiled-in defaults
2. The TOML file (resolved per the search order in `[file
   resolution](#file-resolution)`)
3. Environment variables
4. Command-line flags

Each source can override settings from a lower-priority source.
Operators see what's actually in effect via:

```sh
supply-drop-bbs config show
```

## File resolution

The first file found in this order is used:

1. The path given to `--config <PATH>` on the command line
2. The path given by the `SUPPLY_DROP_CONFIG` environment variable
3. `./config.toml` (the current working directory)
4. `/etc/supply-drop-bbs/config.toml` (system install)
5. `~/.config/supply-drop-bbs/config.toml` (user install)

If none of these exist and no `init` flag is set, the BBS exits
with an error pointing at the recommended location.

## Environment variable overrides

Pattern: `SUPPLY_DROP__SECTION__KEY=value`. Double underscores
separate hierarchy levels.

Examples:

| Env var                                  | Equivalent TOML                          |
|------------------------------------------|------------------------------------------|
| `SUPPLY_DROP__BBS__NAME="Foo BBS"`       | `[bbs]` `name = "Foo BBS"`               |
| `SUPPLY_DROP__DATABASE__PATH=/srv/bbs.db`| `[database]` `path = "/srv/bbs.db"`      |
| `SUPPLY_DROP__LOGGING__LEVEL=DEBUG`      | `[logging]` `level = "DEBUG"`            |
| `SUPPLY_DROP__PLUGINS__WEB__BIND=:8080`  | `[plugins.web]` `bind = ":8080"`         |

Values are parsed using TOML's coercion rules. A bare integer
`8080` becomes the integer `8080`; a quoted string `"8080"`
becomes the string. Booleans use TOML conventions (`true`/`false`).

## Command-line flags

The CLI accepts a small set of overrides:

| Flag                    | Effect                                              |
|-------------------------|-----------------------------------------------------|
| `--config <PATH>`       | Use this config file                                |
| `--data-dir <PATH>`     | Override `[bbs] data_dir`                           |
| `--log-level <LEVEL>`   | Override `[logging] level` (announced loudly)        |
| `--bind-cli <PATH>`     | Override `[plugins.cli] socket`                     |
| `--bind-web <ADDR>`     | Override `[plugins.web] bind` (only if web feature) |
| `--no-web`              | Disable the web plugin even if the feature is built |

Subcommands:

- `supply-drop-bbs setup` - interactive setup wizard (device type, radio config, systemd install)
- `supply-drop-bbs config check` - validate config without starting
- `supply-drop-bbs config show` - print the effective config
- `supply-drop-bbs migrate` - apply pending DB migrations
- `supply-drop-bbs backup` - trigger a manual backup
- `supply-drop-bbs version` - print version + features compiled in

## Top-level sections

The config is split into the following top-level sections:

| Section                | Purpose                                           |
|------------------------|---------------------------------------------------|
| `[bbs]`                | System identity, data paths, room defaults        |
| `[location]`           | GPS coordinates broadcast to the radio on connect |
| `[database]`           | DB path, pool sizes, PRAGMA overrides             |
| `[logging]`            | Level, file path, rotation, per-target overrides  |
| `[security]`           | Argon2 parameters, session lifetimes, rate limits |
| `[backup]`             | Schedule, retention, target directory             |
| `[plugins.cli]`        | CLI transport: socket path, permissions           |
| `[plugins.mesh]`       | Mesh transport: bridge address, retry policy      |
| `[plugins.web]`        | Web admin: bind, CSRF, CSP, config editor path    |
| `[plugins.<other>]`    | Per-plugin sections for any other loaded plugins  |

Sections referencing plugins not loaded at compile time are an
error (typo protection). The compiled-in feature set determines
which plugin sections are valid.

## `[bbs]` - system identity

| Key              | Type   | Default                  | Required | Description                                      |
|------------------|--------|--------------------------|----------|--------------------------------------------------|
| `name`           | string | `"Supply Drop BBS"`      | no       | Display name shown to users on connect           |
| `data_dir`       | path   | `/var/lib/supply-drop-bbs` (root) or `~/.local/share/supply-drop-bbs` (user) | no | Where the BBS stores its data |
| `starting_room`  | string | `"Lobby"`                | no       | Room a newly logged-in user lands in             |
| `welcome_msg`    | string | `"Welcome to {name}."`   | no       | Banner shown on connect; supports `{name}` substitution |
| `timezone`       | string | `"UTC"`                  | no       | Display timezone for sysop UI; storage is always UTC |
| `require_verify` | bool   | `true`                   | no       | When `false`, skip sysop verification — new accounts are treated as `User` immediately (SHTF mode) |
| `guest_room`     | string | (unset)                  | no       | Name of a room unverified users are confined to; created automatically on startup if it does not exist |

### Access control and SHTF mode

By default every new account must be validated by an aide or sysop before the
user can post or read messages.  In emergency situations (sysops unreachable)
two knobs relax this:

**Open access (`require_verify = false`)** — all registrations immediately
receive full User-level access.  No aide or sysop action is required.

```toml
[bbs]
require_verify = false
```

**Guest room (`guest_room = "Guests"`)** — unverified users are placed in a
single designated room and can read and post there, but all other rooms and
mail are invisible until a sysop verifies them.  The room is created
automatically on the first BBS start after this key is set.

```toml
[bbs]
guest_room = "Guests"
```

The two options compose: with `require_verify = false` the guest room still
exists as an ordinary room but carries no access restriction.

Both settings can be changed without a restart via in-BBS sysop commands:

```
OPENACCESS              # disable verification immediately
CLOSEACCESS             # restore verification immediately
GUESTROOM Guests        # set guest room (created if needed)
GUESTROOM OFF           # disable guest room
```

…or via the CLI (takes effect on the next restart):

```sh
supply-drop-bbs config require-verify off
supply-drop-bbs config guest-room "Guests"
supply-drop-bbs config guest-room off
```

## `[location]` - GPS coordinates

When set, the mesh transport sends your node's coordinates to the radio
immediately after each connection is established, so it appears on the
MeshCore map in LoRa adverts. **Takes effect on the next mesh transport
reconnect — no server restart needed.**

| Key         | Type  | Default | Required | Description                          |
|-------------|-------|---------|----------|--------------------------------------|
| `latitude`  | float | (unset) | no       | WGS-84 latitude, degrees (-90…90)    |
| `longitude` | float | (unset) | no       | WGS-84 longitude, degrees (-180…180) |

Both keys must be set together; setting only one has no effect.
Remove both (or omit the section) to stop broadcasting GPS coordinates.

The setup wizard prompts for these during initial configuration and
writes them here. They can also be edited live from the web admin's
**Settings** page without restarting the server.

```toml
[location]
latitude  = 37.7749
longitude = -122.4194
```

## `[database]` - persistence

| Key                       | Type    | Default                 | Required | Description                          |
|---------------------------|---------|-------------------------|----------|--------------------------------------|
| `path`                    | path    | `<data_dir>/bbs.sqlite` | no       | SQLite file location                 |
| `read_pool_size`          | integer | `cpu_count + 2`         | no       | Read-only connection pool size       |
| `busy_timeout_ms`         | integer | `5000`                  | no       | SQLite busy_timeout in milliseconds  |
| `synchronous`             | enum    | `"NORMAL"`              | no       | `"NORMAL"` / `"FULL"` / `"OFF"`. See [ADR-0005](adr/0005-db-strategy.md). |
| `wal_autocheckpoint`      | integer | `10000`                 | no       | WAL pages between checkpoints        |
| `journal_size_limit_bytes`| integer | `67108864`              | no       | Max WAL file size                    |

## `[logging]` - observability

| Key                | Type    | Default                                     | Required | Description                       |
|--------------------|---------|---------------------------------------------|----------|-----------------------------------|
| `level`            | enum    | `"INFO"`                                    | no       | Root level: TRACE/DEBUG/INFO/WARN/ERROR |
| `file`             | path    | `<data_dir>/log/bbs.log`                    | no       | Log file path                     |
| `max_bytes`        | integer | `10485760`                                  | no       | Rotation size per file (10 MB)    |
| `backup_count`     | integer | `5`                                         | no       | Number of rotated files to keep   |
| `format`           | enum    | `"compact"`                                 | no       | `"compact"`, `"pretty"`, `"json"` |
| `targets`          | table   | `{}`                                        | no       | Per-target level overrides; see below |

Per-target overrides example:

```toml
[logging.targets]
"supply_drop_bbs::transport::mesh" = "DEBUG"
"sqlx::query" = "INFO"
"meshcore_companion::frame" = "WARN"
```

See [ADR-0009](adr/0009-tracing-config-respected.md) for the
no-silent-overrides rule.

## `[security]` - authentication and rate limiting

| Key                          | Type    | Default | Required | Description                          |
|------------------------------|---------|---------|----------|--------------------------------------|
| `argon2_memory_kib`          | integer | `19456` | no       | Argon2 memory cost (~19 MB; tuned for ~250ms on Pi 4) |
| `argon2_iterations`          | integer | `2`     | no       | Argon2 time cost                     |
| `argon2_parallelism`         | integer | `1`     | no       | Argon2 parallelism                   |
| `session_lifetime_web_secs`  | integer | `43200` | no       | Web session lifetime (12 hours)      |
| `session_lifetime_mesh_secs` | integer | `259200`| no       | Mesh session lifetime (3 days)       |
| `login_rate_per_min`         | integer | `5`     | no       | Failed login attempts per minute per source |
| `command_rate_per_min`       | integer | `60`    | no       | Commands per minute per session       |

## `[backup]` - disaster recovery

| Key                | Type    | Default                       | Required | Description                       |
|--------------------|---------|-------------------------------|----------|-----------------------------------|
| `enabled`          | bool    | `true`                        | no       | Whether the periodic backup runs  |
| `interval_hours`   | integer | `6`                           | no       | Hours between automatic backups   |
| `directory`        | path    | `<data_dir>/backups`          | no       | Where backup files go             |
| `keep_daily`       | integer | `7`                           | no       | Daily backups to retain           |
| `keep_weekly`      | integer | `4`                           | no       | Weekly backups to retain          |

## `[plugins.cli]` - CLI transport

| Key            | Type    | Default                       | Required | Description                       |
|----------------|---------|-------------------------------|----------|-----------------------------------|
| `enabled`      | bool    | `true`                        | no       | Whether to start the CLI listener |
| `socket`       | path    | `<data_dir>/cli.sock`         | no       | Unix socket path                  |
| `socket_mode`  | string  | `"0600"`                      | no       | Octal mode of the socket file     |
| `socket_owner` | string  | (process owner)               | no       | Username/UID to chown socket to   |

## `[plugins.mesh]` - mesh transport

### Connection type

| Key                | Type   | Default    | Required | Description                                  |
|--------------------|--------|------------|----------|----------------------------------------------|
| `enabled`          | bool   | `true`     | no       | Whether to start the mesh transport          |
| `connection_type`  | enum   | `"serial"` | no       | How to reach the radio: `"serial"`, `"tcp"`, or `"hat"`. See [ADR-0013](adr/0013-native-serial-transport-for-usb-devices.md). |
| `command_prefix`   | string | `""`       | no       | Optional single-character prefix for BBS commands (e.g., `"!"`). Empty string means no prefix - every message is treated as a command. |

### Serial mode (`connection_type = "serial"`)

Used for USB-native MeshCore devices (Heltec V3, T-Beam, etc.) that
run companion-frame firmware. No `pymc_core` required.

| Key           | Type    | Default          | Required | Description                              |
|---------------|---------|------------------|----------|------------------------------------------|
| `serial_port` | string  | `"/dev/ttyACM0"` | no       | Serial device path. The setup wizard auto-detects this. |
| `baud_rate`   | integer | `115200`         | no       | Serial baud rate                         |

### TCP / HAT mode (`connection_type = "tcp"` or `"hat"`)

Used when `pymc_core` is running separately (Pi HAT setups or any
external `CompanionFrameServer`). `"hat"` and `"tcp"` are identical at
the transport level; the distinction tells the setup wizard to install
`pymc_core` as a systemd dependency.

| Key                          | Type    | Default            | Required | Description                                             |
|------------------------------|---------|--------------------|----------|---------------------------------------------------------|
| `addr`                       | string  | `"127.0.0.1:5000"` | no       | Address of the `CompanionFrameServer`                   |
| `reconnect_delay_initial_ms` | integer | `1000`             | no       | Initial reconnect delay after disconnect (ms)           |
| `reconnect_delay_max_ms`     | integer | `60000`            | no       | Maximum reconnect delay after repeated failures (ms). Backoff is exponential between initial and max. |
| `app_target_version`         | integer | `3`                | no       | Companion protocol version to negotiate. Leave at default unless you know the bridge speaks an older version. |

### HAT pin configuration (`connection_type = "hat"`)

Pin config lives under `[plugins.mesh.hat]`. The setup wizard
populates this from the chosen preset; manual overrides are supported.

| Key                 | Type    | Default | Required | Description                                     |
|---------------------|---------|---------|----------|-------------------------------------------------|
| `preset`            | string  | -       | yes (hat)| HAT model: `"zebrahat"`, `"meshadv-mini"`, `"meshadv"`, `"waveshare"`, `"uconsole"`, `"custom"` |
| `bus_id`            | integer | `0`     | no       | SPI bus                                         |
| `cs_pin`            | integer | -       | custom   | SPI chip-select GPIO (BCM numbering)            |
| `reset_pin`         | integer | -       | custom   | Radio reset GPIO                                |
| `busy_pin`          | integer | -       | custom   | Radio busy GPIO                                 |
| `irq_pin`           | integer | -       | custom   | Radio IRQ GPIO                                  |
| `txen_pin`          | integer | `-1`    | no       | TX-enable GPIO (`-1` = not connected)           |
| `rxen_pin`          | integer | `-1`    | no       | RX-enable GPIO (`-1` = not connected)           |

Preset defaults (set automatically by the wizard; override only if
your wiring differs from the standard layout):

| Preset        | CS | Reset | Busy | IRQ | Notes                        |
|---------------|----|-------|------|-----|------------------------------|
| `zebrahat`    | 24 | 17    | 27   | 22  |                              |
| `meshadv-mini`| 8  | 24    | 20   | 16  |                              |
| `meshadv`     | 21 | 18    | 20   | 16  | TXEN=13, RXEN=12             |
| `waveshare`   | 21 | 18    | 20   | 16  | TXEN=13, RXEN=12             |
| `uconsole`    | -1 | 25    | 24   | 26  | bus_id=1, hardware CS        |

## `[plugins.meshtastic]` - Meshtastic transport (only if `transport-meshtastic` feature enabled)

Meshtastic is a separate radio protocol from MeshCore. Both can run simultaneously
on the same BBS — each talks to its own radio device. Meshtastic radios connect
either via USB serial or TCP to a running `meshtasticd` instance.

### Connection type

| Key                | Type   | Default    | Required | Description                                         |
|--------------------|--------|------------|----------|-----------------------------------------------------|
| `enabled`          | bool   | `false`    | no       | Whether to start the Meshtastic transport           |
| `connection_type`  | enum   | `"serial"` | no       | How to reach the radio: `"serial"` or `"tcp"`      |
| `command_prefix`   | string | (unset)    | no       | Optional single-character prefix for BBS commands   |
| `welcome_message`  | string | see default | no      | Greeting sent to a node on first contact            |
| `max_payload_bytes`| integer| `220`      | no       | Maximum UTF-8 bytes per outbound text packet        |
| `node_credential_ttl_days` | integer | `14` | no | Days a node auto-login credential remains valid (`0` disables) |
| `hop_limit`        | integer| `3`        | no       | Hop limit for outbound replies and notifications    |
| `want_ack`         | bool   | `true`     | no       | Request Meshtastic radio-layer acknowledgements     |
| `reconnect_delay_initial_ms` | integer | `1000` | no | Initial reconnect delay after disconnect           |
| `reconnect_delay_max_ms`     | integer | `60000`| no | Maximum reconnect delay after repeated failures    |

### Serial mode (`connection_type = "serial"`)

Direct USB connection to a Meshtastic radio (Heltec V3, T-Beam, RAK4631, etc.).
No daemon required.

| Key           | Type    | Default          | Required | Description           |
|---------------|---------|------------------|----------|-----------------------|
| `serial_port` | string  | `"/dev/ttyACM0"` | no       | Serial device path    |
| `baud_rate`   | integer | `115200`         | no       | Serial baud rate      |

### TCP mode (`connection_type = "tcp"`)

Connects to a running `meshtasticd` instance or any Meshtastic node that exposes
a TCP stream. Default port for `meshtasticd` is `4403`.

| Key    | Type   | Default            | Required | Description                 |
|--------|--------|--------------------|----------|-----------------------------|
| `addr` | string | `"127.0.0.1:4403"` | no       | Address of the meshtasticd listener |

### Example

```toml
[plugins.meshtastic]
enabled         = true
connection_type = "serial"
serial_port     = "/dev/ttyUSB0"
```

## `[plugins.web]` - admin web (only if `admin-web` feature enabled)

| Key                     | Type    | Default                | Required | Description                                |
|-------------------------|---------|------------------------|----------|--------------------------------------------|
| `enabled`               | bool    | `true`                 | no       | Whether to start the web listener (set false to disable without recompiling) |
| `bind`                  | string  | `"127.0.0.1:8080"`     | no       | Address to bind. **Default 127.0.0.1.**    |
| `external_origin`       | string  | (none)                 | when behind reverse proxy | Public origin URL for CSRF / cookie policy |
| `cookie_secure`         | bool    | `true`                 | no       | `Secure` flag on cookies. Set false only for local dev. |
| `prometheus`            | bool    | `false`                | no       | Expose `/metrics`                          |
| `csp`                   | string  | (built-in strict CSP)  | no       | Content-Security-Policy override           |
| `config_path`           | path    | (none)                 | no       | Absolute path to the config file the web admin's **Settings** page reads and writes. Must be writable by the server process. When unset, the Settings page loads in read-only mode and explains the gap. |

### Enabling the Settings page

The **Settings** page (`/settings` in the web admin) lets sysops view and
edit the config file through a form UI without touching the TOML directly.
Point `config_path` at the same file the BBS is reading:

```toml
[plugins.web]
config_path = "/etc/supply-drop-bbs/config.toml"
```

The server process must have write permission to that file. If it does not,
the Settings page opens in read-only mode and shows the exact `chown`/`chmod`
commands needed to fix the permission. Changes written through the UI require
a restart to take effect (the GPS `[location]` section is the one exception —
it applies on the next mesh reconnect).

If `bind` is `0.0.0.0` and `external_origin` is unset, startup
fails with an error: binding to all interfaces without specifying
the public origin makes CSRF protection unsafe.

## `[[plugins.process]]` - externally-spawned transport plugins (only if `transport-process` feature enabled)

Process transport plugins are external executables that speak the Supply Drop
IPC protocol over stdin/stdout. Each plugin is a separate entry in an array
table. Manage them with `supply-drop-bbs plugin add/remove/enable/disable` or
directly in the web admin UI.

```toml
[[plugins.process]]
name              = "my-telnet"           # unique stable identifier
command           = "/usr/local/bin/my-telnet-plugin"
args              = ["--port", "23"]      # optional
enabled           = true
restart_on_crash  = true                  # respawn automatically on non-zero exit
restart_delay_secs = 5                    # seconds to wait before respawn

[[plugins.process]]
name    = "my-aprs"
command = "/opt/aprs-bridge/aprs-bridge"
enabled = false                           # start disabled; enable via web UI
```

| Key                  | Type     | Default | Required | Description |
|----------------------|----------|---------|----------|-------------|
| `name`               | string   | —       | **yes**  | Unique stable identifier. Used in logs, web UI, and CLI output. Lowercase ASCII with hyphens. |
| `command`            | string   | —       | **yes**  | Path to the plugin executable. |
| `args`               | string[] | `[]`    | no       | Arguments passed verbatim to the executable. |
| `enabled`            | bool     | `true`  | no       | Whether to start this plugin on BBS startup. |
| `restart_on_crash`   | bool     | `true`  | no       | Respawn the process when it exits with a non-zero code. |
| `restart_delay_secs` | integer  | `5`     | no       | Seconds to wait before respawning after a crash. |

See [PROCESS_TRANSPORTS_OPS.md](PROCESS_TRANSPORTS_OPS.md) for plugin
installation, the IPC protocol, and troubleshooting.

## Validation rules

Validation runs at startup, before any service starts. Failures
exit the process with a clear error message including:

- **TOML well-formedness.** Errors include `file:line:col`.
- **Required keys present.** Every field without a default fails
  with the missing path (e.g., `plugins.web.bind`).
- **Type correctness.** `8080` is not a string; `"foo"` is not a
  port.
- **Range checks.** Ports `1..=65535`; positive sizes; valid
  enum variants.
- **Cross-references.** Plugin sections must correspond to loaded
  plugins; referenced rooms must exist; backup directory must be
  writable.
- **Permission/ownership.** Files containing secrets must be
  mode 0600 or stricter on Unix.
- **Conflicting settings.** `bind = "0.0.0.0"` without
  `external_origin` is an error, not a warning.

## Sysop bootstrap

The first sysop account is created during `supply-drop-bbs init`.
It is **not** in the config file (passwords don't go in TOML).
The init flow prompts for username + password and creates the
account in the DB.

To bootstrap a sysop after init (e.g., when migrating between
machines), use:

```sh
supply-drop-bbs admin create-sysop --username <name>
```

This prompts for a password and creates the account, gated on
having direct DB write access (i.e., it requires running on the
machine, with read access to the DB file). It is not an HTTP
endpoint.

## Reload behaviour

Most config changes require a restart. The keys that **can** change
without restart are:

- `[logging] level` (via `SIGHUP`, future enhancement)
- `[security] login_rate_per_min` (via `SIGHUP`, future enhancement)

All other keys require a process restart. We document this on each
key as the implementation lands. **TBD.**

## See also

- [ADR-0008](adr/0008-toml-config-with-env-overrides.md) - why
  TOML, why this overlay model
- [`config.example.toml`](../config.example.toml) - runnable
  starting point
- [OPERATIONS.md](OPERATIONS.md) - install and operations guide
