# CLI reference - `supply-drop-bbs`

Complete reference for every subcommand and flag.

## Synopsis

```
supply-drop-bbs [OPTIONS] [SUBCOMMAND]
```

Omitting the subcommand is the same as `supply-drop-bbs run`.

---

## Global options

These options are accepted by every subcommand.

| Flag | Env override | Default | Description |
|------|-------------|---------|-------------|
| `--config <PATH>` | - | see below | Path to the TOML config file |
| `--data-dir <PATH>` | `SUPPLY_DROP__BBS__DATA_DIR` | `<config data_dir>` | Override the data directory (database, logs, backups) |
| `--log-level <LEVEL>` | `SUPPLY_DROP__LOGGING__LEVEL` | `INFO` | Log verbosity: `TRACE` `DEBUG` `INFO` `WARN` `ERROR` |
| `--version` | - | - | Print version and exit |
| `--help` / `-h` | - | - | Print help and exit |

### Config file search order

When `--config` is not given the BBS looks for a config file in this order, stopping at the first one found:

1. `./config.toml`
2. `/etc/supply-drop-bbs/config.toml`
3. `~/.config/supply-drop-bbs/config.toml`

If none exists, compiled-in defaults are used. An empty or missing config file is always valid.

### `--data-dir` behaviour

Setting `--data-dir` clears any database path, log file path, and backup directory that were set in the config file - they are re-derived under the new data directory. If you need an explicit database path alongside a different data directory, set both `data_dir` and `database.path` in the TOML file instead.

---

## Subcommands

### `run`

```
supply-drop-bbs run [OPTIONS]
```

Start the BBS. This is the default when no subcommand is given - the following two commands are equivalent:

```sh
supply-drop-bbs
supply-drop-bbs run
```

**What it does:**

1. Loads and resolves configuration
2. Initialises tracing / logging
3. Creates the data directory if it does not exist
4. Opens (and migrates) the SQLite database
5. Constructs the BBS host
6. Starts compiled-in transport plugins (mesh, CLI, web admin)
7. Blocks until `Ctrl-C` or `SIGTERM`
8. Stops plugins in reverse order and exits cleanly

**Examples:**

```sh
# Systemd service - normal production invocation
supply-drop-bbs run --config /etc/supply-drop-bbs/config.toml

# Development - verbose logging, local data directory
supply-drop-bbs run --data-dir ./dev-data --log-level debug

# Override log level via environment
SUPPLY_DROP__LOGGING__LEVEL=trace supply-drop-bbs run
```

---

### `setup`

```
supply-drop-bbs setup [OPTIONS]
```

Run the interactive first-run setup wizard. Detects your radio device, asks configuration questions, and writes a `config.toml`. Safe to run on an existing installation - answers are pre-populated from the current config.

The wizard asks:

1. Radio connection type - USB serial or Pi HAT
2. Serial port *(USB only)* - auto-detected; you confirm or enter manually
3. BBS name - shown to users on connect
4. Data directory - defaults to `/var/lib/supply-drop-bbs`
5. Web admin UI - whether to enable it, bind address, and password

After the wizard completes, restart the service to apply:

```sh
sudo systemctl restart supply-drop-bbs
```

**Example:**

```sh
sudo supply-drop-bbs setup --config /etc/supply-drop-bbs/config.toml
```

---

### `config check`

```
supply-drop-bbs config check [OPTIONS]
```

Validate the config file and exit. Exit code `0` means the config is valid; non-zero means it is not.

```sh
supply-drop-bbs config check
# config OK

supply-drop-bbs config check --config /etc/supply-drop-bbs/config.toml
# config OK
```

Use this before restarting the service after an edit:

```sh
supply-drop-bbs config check && sudo systemctl restart supply-drop-bbs
```

---

### `config show`

```
supply-drop-bbs config show [OPTIONS]
```

Print the effective configuration as TOML - compiled-in defaults, config file values, and environment overrides all merged and displayed together. Useful for verifying that overrides are applied correctly.

```sh
supply-drop-bbs config show
supply-drop-bbs config show --config /etc/supply-drop-bbs/config.toml
SUPPLY_DROP__BBS__DATA_DIR=/tmp/test supply-drop-bbs config show
```

---

### `migrate`

```
supply-drop-bbs migrate [OPTIONS]
```

Apply any pending database migrations and exit. The `run` subcommand always migrates on startup, so this is only needed if you want to migrate without starting the BBS (e.g. as a pre-flight step in a deployment script).

> **Note:** not yet implemented. The command exits with an error in the current release.

---

### `backup`

```
supply-drop-bbs backup [OPTIONS]
```

Trigger an immediate database backup (`VACUUM INTO`) and exit. The backup lands in `<data_dir>/backups/` with a timestamp filename. The running BBS service does not need to be stopped - `VACUUM INTO` is non-blocking.

> **Note:** not yet implemented. The command exits with an error in the current release. Use the **Trigger backup** button in the web admin UI in the meantime.

---

### `user promote`

```
supply-drop-bbs user promote <USERNAME> [OPTIONS]
```

Promote a user account to **Sysop** (permission level 100). The BBS service does not need to be restarted - the change takes effect the next time the user logs in or issues a command.

| Argument | Description |
|----------|-------------|
| `<USERNAME>` | BBS username to promote (case-sensitive) |

```sh
supply-drop-bbs user promote alice
# alice promoted to sysop (level 100)

sudo supply-drop-bbs user promote alice \
  --config /etc/supply-drop-bbs/config.toml
```

**Exit codes:** `0` on success; `1` if the user is not found or the database cannot be opened.

> **Aide level:** There is currently no `--aide` flag. To promote to Aide (level 50) instead of Sysop, use the in-BBS `.EU <username>` command as a Sysop, or update the database directly:
>
> ```sh
> sudo sqlite3 /var/lib/supply-drop-bbs/bbs.sqlite \
>   "UPDATE users SET permission_level = 50 WHERE username = 'alice';"
> ```

---

### `user demote`

```
supply-drop-bbs user demote <USERNAME> [OPTIONS]
```

Demote a user account back to **User** (permission level 10). Removes Sysop or Aide privileges. The change takes effect the next time the user issues a command.

| Argument | Description |
|----------|-------------|
| `<USERNAME>` | BBS username to demote (case-sensitive) |

```sh
supply-drop-bbs user demote alice
# alice demoted to user (level 10)
```

**Exit codes:** `0` on success; `1` if the user is not found or the database cannot be opened.

---

## Permission levels

| Level | Value | How to set |
|-------|-------|-----------|
| Unvalidated | 0 | Assigned on registration; cannot log into BBS features until validated |
| User | 10 | `user demote` or in-BBS `.EU` |
| Aide | 50 | In-BBS `.EU` as Sysop, or direct DB update |
| Sysop | 100 | `user promote` or in-BBS `.EU` as Sysop |

The first account registered on a fresh installation is automatically promoted to Sysop.

---

## Common workflows

### Bootstrap a new installation

```sh
# First user self-promotes to Sysop automatically on registration.
# If you need to promote a second sysop:
sudo supply-drop-bbs user promote alice \
  --config /etc/supply-drop-bbs/config.toml
```

### Recover lost sysop access

```sh
sudo systemctl stop supply-drop-bbs
sudo supply-drop-bbs user promote alice \
  --config /etc/supply-drop-bbs/config.toml
sudo systemctl start supply-drop-bbs
```

### Verify config before restarting

```sh
supply-drop-bbs config check --config /etc/supply-drop-bbs/config.toml \
  && sudo systemctl restart supply-drop-bbs
```

### Run a local dev instance

```sh
cargo build
mkdir -p dev-data
./target/debug/supply-drop-bbs run \
  --config dev-config.toml \
  --data-dir dev-data \
  --log-level debug
```
