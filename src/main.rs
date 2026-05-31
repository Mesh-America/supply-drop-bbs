//! Supply Drop BBS — entry point.
//!
//! Parses the command line, loads config, and dispatches to the
//! appropriate subcommand. The `run` subcommand (default) executes the
//! host supervisor: opens the database, wires up the `BbsHost`, and
//! starts all compiled-in transport plugins. It shuts down cleanly on
//! Ctrl-C / SIGTERM.
//!
//! Architecture: see `docs/ARCHITECTURE.md`.

#![allow(missing_docs)]

// Use mimalloc as the global allocator. It reduces heap fragmentation on
// long-running Pi deployments and is faster than the system allocator for
// the allocation patterns this crate produces (many small, short-lived strings).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod config;
mod mesh_presets;
mod setup;

use std::{path::PathBuf, sync::Arc};

use bbs_core::{BbsHost, Database};
use clap::{Parser, Subcommand};
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, reload, util::SubscriberInitExt, EnvFilter};

type LogReloadFn = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "supply-drop-bbs",
    about = env!("CARGO_PKG_DESCRIPTION"),
    version,
    propagate_version = true,
)]
struct Cli {
    /// Path to the TOML config file.
    ///
    /// If omitted, the BBS searches the standard locations in order:
    /// ./config.toml → /etc/supply-drop-bbs/config.toml →
    /// ~/.config/supply-drop-bbs/config.toml.
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Override the data directory (database, logs, backups).
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        env = "SUPPLY_DROP__BBS__DATA_DIR"
    )]
    data_dir: Option<PathBuf>,

    /// Override the log level (TRACE/DEBUG/INFO/WARN/ERROR).
    ///
    /// When this flag is used the effective level is announced in the
    /// first log line (ADR-0009: no silent stomps).
    #[arg(
        long,
        global = true,
        value_name = "LEVEL",
        env = "SUPPLY_DROP__LOGGING__LEVEL"
    )]
    log_level: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the BBS (default when no subcommand is given).
    Run,

    /// Interactive first-run setup wizard.
    ///
    /// Detects your radio device, writes a config file, and optionally
    /// installs systemd unit(s).
    Setup,

    /// Configuration management.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },

    /// Apply any pending database migrations.
    Migrate,

    /// Trigger an immediate database backup.
    Backup,

    /// Manage user accounts.
    User {
        #[command(subcommand)]
        action: UserAction,
    },

    /// Manage rooms.
    Room {
        #[command(subcommand)]
        action: RoomAction,
    },

    /// Manage externally-spawned transport plugins.
    ///
    /// These commands modify config.toml directly. Changes take effect on the
    /// next BBS restart (or use the web UI for live runtime management).
    #[cfg(feature = "transport-process")]
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },

    /// Print a system metrics snapshot (CPU, memory, disk, network) and exit.
    Metrics,

    /// Manage the USB serial MeshCore companion device's node identity.
    ///
    /// These commands open the serial port directly to communicate with the device.
    /// The BBS service must not be running on the same port when you execute them.
    Node {
        #[command(subcommand)]
        action: NodeAction,
    },
}

#[derive(Subcommand)]
enum UserAction {
    /// Create a new user account without going through the mesh registration workflow.
    ///
    /// Useful for bootstrapping the first sysop account when no BBS session is available.
    /// Prompts for a password interactively (input is hidden).
    Create {
        /// BBS username to create.
        username: String,
        /// Grant Sysop permission (level 100) immediately.
        ///
        /// Without this flag the account is created as a regular User (level 10).
        #[arg(long)]
        sysop: bool,
    },
    /// List user accounts.
    ///
    /// By default lists all accounts. Use --pending to show only those
    /// waiting for sysop validation (permission level 0).
    List {
        /// Show only unvalidated accounts (permission level 0).
        #[arg(long)]
        pending: bool,
    },
    /// Verify (validate) a user account, promoting it from Unvalidated to User.
    ///
    /// Equivalent to the in-session `V <username>` sysop command.
    Verify {
        /// BBS username to verify.
        username: String,
    },
    /// Promote a user to Sysop (permission level 100).
    Promote {
        /// BBS username to promote.
        username: String,
    },
    /// Demote a user back to regular User (permission level 10).
    Demote {
        /// BBS username to demote.
        username: String,
    },
    /// Reset a user's password without requiring the old password.
    ///
    /// Prompts for the new password interactively (input is hidden).
    SetPassword {
        /// BBS username whose password will be reset.
        username: String,
    },
}

#[derive(Subcommand)]
enum RoomAction {
    /// Create a new room and append it to the end of the room list.
    Create {
        /// Room name (must be unique; spaces are allowed).
        name: String,
        /// Optional one-line description shown in room listings.
        #[arg(long)]
        description: Option<String>,
    },
}

#[cfg(feature = "transport-process")]
#[derive(Subcommand)]
enum PluginAction {
    /// List all configured process transport plugins.
    List,

    /// Add a new process transport plugin to config.toml.
    ///
    /// Restart the BBS (or use the web UI) to start the plugin immediately.
    Add {
        /// Unique plugin name.
        name: String,
        /// Path to the plugin executable.
        command: String,
        /// Arguments for the executable (space-separated if multiple).
        #[arg(long, num_args = 0..)]
        args: Vec<String>,
        /// Disable the plugin after adding (default: enabled).
        #[arg(long)]
        disabled: bool,
        /// Do not restart the plugin on crash (default: restart).
        #[arg(long)]
        no_restart: bool,
        /// Seconds to wait between crash restarts.
        #[arg(long, default_value = "5")]
        restart_delay: u64,
    },

    /// Remove a process transport plugin from config.toml.
    Remove {
        /// Plugin name to remove.
        name: String,
    },

    /// Enable a disabled plugin in config.toml.
    Enable {
        /// Plugin name to enable.
        name: String,
    },

    /// Disable a running plugin in config.toml.
    Disable {
        /// Plugin name to disable.
        name: String,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Validate the config file and exit (exit code 0 = valid).
    Check,

    /// Print the effective configuration as TOML (defaults filled in,
    /// environment overrides applied).
    Show,

    /// Enable or disable the verification requirement.
    ///
    /// When disabled (`off`), all registrations immediately receive
    /// User-level access without aide/sysop validation.
    /// Changes take effect on the next BBS restart.
    RequireVerify {
        /// `on` to require verification (default), `off` to skip it.
        enabled: String,
    },

    /// Set or clear the guest room.
    ///
    /// Unverified users are allowed into this room and nowhere else.
    /// The room is created automatically on the next BBS start if it does not exist.
    /// Use `off` to disable the guest room feature.
    /// Changes take effect on the next BBS restart.
    GuestRoom {
        /// Room name, or `off` to disable.
        value: String,
    },
}

#[derive(Subcommand)]
#[allow(clippy::enum_variant_names)] // "Key" postfix is intentional — all actions operate on a key
enum NodeAction {
    /// Show the companion device's public key (hex).
    ///
    /// Connects to the device, performs the AppStart handshake, and prints the
    /// 32-byte public key from the SelfInfo response.
    ShowKey {
        /// Serial port to connect to (e.g. /dev/ttyACM0, COM3).
        /// Defaults to the port in config.toml [plugins.mesh].
        #[arg(long)]
        port: Option<String>,
        /// Baud rate. Defaults to the value in config.toml or 115200.
        #[arg(long)]
        baud: Option<u32>,
    },
    /// Export the companion device's private key as hex.
    ///
    /// The private key uniquely identifies the node on the mesh.
    /// Keep it secret and back it up — you will need it to restore the node's
    /// identity after a firmware flash or to migrate to new hardware.
    ExportKey {
        #[arg(long)]
        port: Option<String>,
        #[arg(long)]
        baud: Option<u32>,
    },
    /// Import a private key into the companion device.
    ///
    /// The key must be exactly 64 hex characters (32 bytes).
    /// The device's current key is replaced immediately. Back it up first with
    /// `export-key` if you may need to restore it.
    ImportKey {
        /// 64 hex characters (32 bytes).
        key: String,
        #[arg(long)]
        port: Option<String>,
        #[arg(long)]
        baud: Option<u32>,
    },

    /// Apply radio configuration to the companion device.
    ///
    /// Reads parameters from `[plugins.mesh.radio]` in config.toml, with
    /// optional per-flag overrides. The device persists the settings in its
    /// own flash — they survive power cycles and reconnects without the BBS
    /// re-sending them.
    ///
    /// The BBS service must not be running on the same port when you run this.
    ///
    /// Examples:
    ///
    ///   # Apply preset from config.toml and save any flag overrides back
    ///   supply-drop-bbs node set-radio
    ///
    ///   # Apply a specific preset and save it to config.toml
    ///   supply-drop-bbs node set-radio --preset "USA/Canada" --save
    ///
    ///   # List available presets
    ///   supply-drop-bbs node set-radio --list-presets
    SetRadio {
        #[arg(long)]
        port: Option<String>,
        #[arg(long)]
        baud: Option<u32>,
        /// Named region preset (e.g. "USA/Canada"). Overrides config.toml preset.
        #[arg(long)]
        preset: Option<String>,
        /// Carrier frequency in Hz. Overrides preset.
        #[arg(long)]
        frequency_hz: Option<u64>,
        /// Channel bandwidth in Hz. Overrides preset.
        #[arg(long)]
        bandwidth_hz: Option<u32>,
        /// Spreading factor 7–12. Overrides preset.
        #[arg(long)]
        spreading_factor: Option<u8>,
        /// Coding rate 5–8. Overrides preset.
        #[arg(long)]
        coding_rate: Option<u8>,
        /// TX power in dBm. Overrides preset.
        #[arg(long)]
        tx_power_dbm: Option<i32>,
        /// Also save the resolved settings to config.toml.
        #[arg(long)]
        save: bool,
        /// Print all available region preset names and exit.
        #[arg(long)]
        list_presets: bool,
    },

    /// Apply Meshtastic LoRa radio configuration from config.toml to the device.
    ///
    /// Reads `[plugins.meshtastic.radio]` (region and modem preset) and pushes
    /// them to the connected Meshtastic radio.  The device stores the settings
    /// in its own flash so they survive power cycles.
    ///
    /// The BBS service must **not** be running on the same port when you run this.
    ///
    /// Example:
    ///   supply-drop-bbs node set-meshtastic-radio
    #[cfg(feature = "transport-meshtastic")]
    SetMeshtasticRadio {
        /// Serial port (e.g. /dev/ttyUSB0, COM3). Defaults to serial_port in config.toml.
        #[arg(long)]
        port: Option<String>,
        /// Baud rate. Defaults to the value in config.toml or 115200.
        #[arg(long)]
        baud: Option<u32>,
        /// TCP address for meshtasticd (e.g. 127.0.0.1:4403). Defaults to addr in config.toml.
        #[arg(long)]
        addr: Option<String>,
    },

    /// Apply Meshtastic node name (long name + short name) from config.toml to the device.
    ///
    /// Reads `[plugins.meshtastic]` `long_name` and `short_name` and pushes them
    /// to the connected Meshtastic radio.  The existing PKC public key is
    /// preserved by fetching owner info from the device first.
    ///
    /// The BBS service must **not** be running on the same port when you run this.
    ///
    /// Example:
    ///   supply-drop-bbs node set-meshtastic-owner
    #[cfg(feature = "transport-meshtastic")]
    SetMeshtasticOwner {
        /// Serial port. Defaults to serial_port in config.toml.
        #[arg(long)]
        port: Option<String>,
        /// Baud rate. Defaults to the value in config.toml or 115200.
        #[arg(long)]
        baud: Option<u32>,
        /// TCP address. Defaults to addr in config.toml.
        #[arg(long)]
        addr: Option<String>,
    },
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    // Extract config path before match consumes cli.command.
    let config_path = cli.config.clone();

    match cli.command {
        None | Some(Commands::Run) => cmd_run(&cli).await,
        Some(Commands::Setup) => cmd_setup(config_path.as_deref()),
        Some(Commands::Config { action }) => cmd_config(config_path.as_deref(), action),
        Some(Commands::Migrate) => cmd_migrate(&cli).await,
        Some(Commands::Backup) => cmd_backup(&cli).await,
        Some(Commands::User { ref action }) => cmd_user(&cli, action).await,
        Some(Commands::Room { ref action }) => cmd_room(&cli, action).await,
        #[cfg(feature = "transport-process")]
        Some(Commands::Plugin { action }) => cmd_plugin(config_path.as_deref(), action),
        Some(Commands::Metrics) => cmd_metrics(),
        Some(Commands::Node { action }) => cmd_node(config_path.as_deref(), action).await,
    }
}

// ── Subcommand handlers ───────────────────────────────────────────────────────

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Load and resolve the effective [`config::Config`] for a CLI invocation.
///
/// Applies the `--data-dir` flag (and the `SUPPLY_DROP__BBS__DATA_DIR` env var
/// it is aliased to) on top of the TOML config file so that *every* subcommand
/// honours the override — not just `run`.
fn load_config(cli: &Cli) -> config::Config {
    let mut cfg = match config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error loading config: {e}");
            std::process::exit(1);
        }
    };
    if let Some(ref dd) = cli.data_dir {
        cfg.bbs.data_dir = Some(dd.clone());
        // Clear derived paths so resolve() re-computes them under the new root.
        cfg.database.path = None;
        cfg.logging.file = None;
        cfg.backup.directory = None;
        #[cfg(feature = "transport-cli")]
        {
            cfg.plugins.cli.socket = None;
        }
        cfg = cfg.resolve();
    }
    cfg
}

/// Open the SQLite database at `path`, or print an actionable error and exit.
///
/// When the open fails the error message includes a hint about the service
/// user — the most common cause on a `.deb` install is running a maintenance
/// command as `root` instead of as the `supply-drop` service account.
async fn open_database(path: &std::path::Path) -> Database {
    match Database::open(&path.to_string_lossy()).await {
        Ok(db) => db,
        Err(e) => {
            eprintln!("error opening database: {e}");
            eprintln!();
            eprintln!(
                "  The database may not be accessible from the current user.\n\
                 \n\
                   On .deb installs the database is owned by the 'supply-drop'\n\
                   service account. Try running the command as that user:\n\
                 \n\
                     sudo -u supply-drop supply-drop-bbs <subcommand> ...\n\
                 \n\
                   Or pass the data directory explicitly:\n\
                 \n\
                     sudo supply-drop-bbs --data-dir /var/lib/supply-drop-bbs <subcommand> ..."
            );
            std::process::exit(1);
        }
    }
}

/// Host supervisor — the real `run` path.
///
/// 1. Load + resolve config (apply CLI overrides)
/// 2. Initialise tracing
/// 3. Ensure data directory exists
/// 4. Open database
/// 5. Construct `BbsHost`
/// 6. Init and start compiled-in transport plugins
/// 7. Block until Ctrl-C / SIGTERM
/// 8. Stop plugins in reverse order
async fn cmd_run(cli: &Cli) {
    // ── 1. Config ─────────────────────────────────────────────────────────────
    let mut cfg = match config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error loading config: {e}");
            std::process::exit(1);
        }
    };

    // Apply --data-dir override.  When this flag is set we clear the
    // derived paths (DB, log file, backup dir, CLI socket) so that
    // resolve() re-derives them under the new data_dir.  Callers who
    // want to keep an explicit database.path can set it in the TOML.
    if let Some(ref dd) = cli.data_dir {
        cfg.bbs.data_dir = Some(dd.clone());
        cfg.database.path = None;
        cfg.logging.file = None;
        cfg.backup.directory = None;
        #[cfg(feature = "transport-cli")]
        {
            cfg.plugins.cli.socket = None;
        }
        cfg = cfg.resolve();
    }

    // Apply --log-level override; parsed before tracing init so we can
    // announce the stomp (ADR-0009) in the first log line.
    let cli_level_str = cli.log_level.clone();
    if let Some(ref level_str) = cli_level_str {
        use std::str::FromStr;
        match config::LogLevel::from_str(level_str) {
            Ok(l) => cfg.logging.level = l,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }

    // ── 2. Tracing ────────────────────────────────────────────────────────────

    // With the admin-web feature the error tracker and log capture layers are
    // wired into the subscriber registry so WARN/ERROR events are captured and
    // all INFO+ events appear in the web log view.  Without it we use the
    // plain init_tracing path.
    #[cfg(feature = "admin-web")]
    let (error_tracker_layer, error_store, error_tx) = bbs_web::error_tracker::new_error_tracker();

    #[cfg(feature = "admin-web")]
    let (log_capture_layer, app_log_buf) = bbs_web::log_capture::new_log_capture_layer();

    #[cfg(feature = "admin-web")]
    let log_reload =
        init_tracing_with_error_layer(&cfg.logging, error_tracker_layer, log_capture_layer);

    #[cfg(not(feature = "admin-web"))]
    let log_reload = init_tracing(&cfg.logging);

    // Non-web builds have no consumer for the reload handle; silence the warning.
    #[cfg(not(feature = "admin-web"))]
    let _ = log_reload;

    // ADR-0009: announce CLI-level stomps loudly.
    if let Some(ref s) = cli_level_str {
        warn!(
            effective_level = s.to_ascii_uppercase(),
            "log level overridden by --log-level CLI flag"
        );
    }

    info!(
        name = %cfg.bbs.name,
        version = env!("CARGO_PKG_VERSION"),
        "supply-drop-bbs starting"
    );

    // ── 3. Data directory ─────────────────────────────────────────────────────
    let data_dir = cfg
        .bbs
        .data_dir
        .as_ref()
        .expect("data_dir set by resolve()");

    if let Err(e) = std::fs::create_dir_all(data_dir) {
        error!(path = %data_dir.display(), "could not create data directory: {e}");
        std::process::exit(1);
    }

    // ── 4. Database ───────────────────────────────────────────────────────────
    let db_path = cfg
        .database
        .path
        .as_ref()
        .expect("database.path set by resolve()");

    info!(path = %db_path.display(), "opening database");

    let db = match Database::open(&db_path.to_string_lossy()).await {
        Ok(d) => d,
        Err(e) => {
            error!("could not open database: {e}");
            std::process::exit(1);
        }
    };

    // ── 5. Host ───────────────────────────────────────────────────────────────

    // Resolve the config file path (same search order used by the process
    // transport and web plugin) so the host can persist policy changes.
    let host_config_path: Option<std::path::PathBuf> = cli
        .config
        .as_deref()
        .and_then(|p| p.canonicalize().ok())
        .or_else(|| {
            [
                std::path::PathBuf::from("config.toml"),
                std::path::PathBuf::from("/etc/supply-drop-bbs/config.toml"),
            ]
            .iter()
            .find(|p| p.exists())
            .and_then(|p| p.canonicalize().ok())
        });

    let access_policy = bbs_core::host::AccessPolicy {
        require_verify: cfg.bbs.require_verify,
        guest_room_name: cfg.bbs.guest_room.clone(),
    };

    let bbs = BbsHost::with_config(
        db,
        cfg.location.as_coords(),
        access_policy,
        host_config_path,
    );

    if let Err(e) = bbs.ensure_guest_room().await {
        error!("guest room setup failed: {e}");
        std::process::exit(1);
    }

    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(bbs);
    info!("host initialised");

    // ── 6. Backup task ───────────────────────────────────────────────────────
    if cfg.backup.enabled {
        if let Some(backup_dir) = cfg.backup.directory.clone() {
            let keep_daily = cfg.backup.keep_daily;
            let keep_weekly = cfg.backup.keep_weekly;
            let interval_hours = cfg.backup.interval_hours;
            let host_backup = Arc::clone(&host);
            info!(
                dir = %backup_dir.display(),
                interval_hours,
                "starting automatic backup task"
            );
            tokio::spawn(async move {
                let period = tokio::time::Duration::from_secs(u64::from(interval_hours) * 3600);
                let mut ticker = tokio::time::interval(period);
                ticker.tick().await; // skip immediate first tick
                loop {
                    ticker.tick().await;
                    if let Err(e) = tokio::fs::create_dir_all(&backup_dir).await {
                        warn!("backup: could not create backup dir: {e}");
                        continue;
                    }
                    let dir_str = backup_dir.to_string_lossy();
                    match host_backup.admin_trigger_backup(&dir_str).await {
                        Ok(rec) => {
                            info!(filename = %rec.filename, "automatic backup completed");
                            prune_backups(&host_backup, &dir_str, keep_daily, keep_weekly).await;
                        }
                        Err(e) => warn!("automatic backup failed: {e}"),
                    }
                }
            });
        }
    }

    // ── 8. Plugins ────────────────────────────────────────────────────────────
    //
    // Each plugin is init'd then start'd.  Errors at init abort startup;
    // errors at start are fatal.  Plugins are stopped in reverse order on
    // shutdown.  Only compiled-in plugins appear here (cargo features gate
    // what's available — see ADR-0004).

    #[cfg(feature = "transport-cli")]
    let cli_transport = init_cli_plugin(&cfg.plugins.cli, Arc::clone(&host)).await;

    #[cfg(feature = "transport-mesh")]
    let mesh_cfg = {
        let mut c = cfg.plugins.mesh.clone();
        // Substitute {name} placeholder before wiring into mesh transport.
        c.welcome_message = cfg.bbs.welcome_msg.replace("{name}", &cfg.bbs.name);
        c
    };

    #[cfg(feature = "transport-mesh")]
    let mesh_transport = init_mesh_plugin(&mesh_cfg, Arc::clone(&host)).await;

    #[cfg(feature = "transport-meshtastic")]
    let meshtastic_transport =
        init_meshtastic_plugin(&cfg.plugins.meshtastic, Arc::clone(&host)).await;

    // Process transport plugins — start manager, then hand registry to web.
    #[cfg(feature = "transport-process")]
    let process_registry = {
        use bbs_process_transport::ProcessPluginManager;
        let config_path = cli
            .config
            .as_deref()
            .and_then(|p| p.canonicalize().ok())
            .or_else(|| {
                [
                    std::path::PathBuf::from("config.toml"),
                    std::path::PathBuf::from("/etc/supply-drop-bbs/config.toml"),
                ]
                .iter()
                .find(|p| p.exists())
                .and_then(|p| p.canonicalize().ok())
            });
        let plugins_d = config_path
            .as_deref()
            .map(config::plugins_d_dir)
            .filter(|d| d.exists());
        ProcessPluginManager::new(
            cfg.plugins.process.clone(),
            Arc::clone(&host),
            config_path,
            plugins_d,
        )
        .await
    };

    #[cfg(feature = "admin-web")]
    let web_plugin = {
        // Resolve the config file to an absolute path so the web plugin can
        // bundle the correct config.toml into backup zips regardless of the
        // process working directory (e.g. systemd starts from /).
        let cfg_abs: Option<String> = if let Some(ref p) = cli.config {
            p.canonicalize()
                .ok()
                .map(|abs| abs.to_string_lossy().into_owned())
        } else {
            // No --config flag: try the same search order as config::load so
            // we can still find the file that was actually loaded.
            [
                std::path::PathBuf::from("config.toml"),
                std::path::PathBuf::from("/etc/supply-drop-bbs/config.toml"),
            ]
            .iter()
            .find(|p| p.exists())
            .and_then(|p| p.canonicalize().ok())
            .map(|abs| abs.to_string_lossy().into_owned())
        };
        let wp = init_web_plugin(
            &cfg.plugins.web,
            Arc::clone(&host),
            cfg_abs,
            Arc::clone(&log_reload),
            error_store,
            error_tx,
            app_log_buf,
        )
        .await;
        if let Some(ref plugin) = wp {
            // Wire the backup directory: single source of truth is [backup] directory.
            // The web plugin has no separate backup_dir setting of its own.
            let backup_dir = cfg
                .backup
                .directory
                .as_ref()
                .map(|d| d.to_string_lossy().into_owned());
            plugin.set_backup_dir(backup_dir);
        }
        #[cfg(feature = "transport-process")]
        if let Some(ref plugin) = wp {
            let registry =
                Arc::clone(&process_registry) as Arc<dyn bbs_plugin_api::PluginRegistryApi>;
            plugin.set_plugin_registry(registry);
        }
        if let Some(ref plugin) = wp {
            plugin.set_active_transports(bbs_web::TransportFlags {
                #[cfg(feature = "transport-mesh")]
                meshcore: cfg.plugins.mesh.enabled,
                #[cfg(not(feature = "transport-mesh"))]
                meshcore: false,
                #[cfg(feature = "transport-meshtastic")]
                meshtastic: cfg.plugins.meshtastic.enabled,
                #[cfg(not(feature = "transport-meshtastic"))]
                meshtastic: false,
                compiled_mesh: cfg!(feature = "transport-mesh"),
                compiled_meshtastic: cfg!(feature = "transport-meshtastic"),
                compiled_cli: cfg!(feature = "transport-cli"),
            });
        }
        wp
    };

    // ── 9. Wait for shutdown signal ───────────────────────────────────────────
    info!("supply-drop-bbs ready — press Ctrl-C to stop");

    match tokio::signal::ctrl_c().await {
        Ok(()) => info!("Ctrl-C received — shutting down"),
        Err(e) => error!("error waiting for Ctrl-C: {e}"),
    }

    // ── 10. Stop plugins (reverse order) ─────────────────────────────────────
    #[cfg(feature = "admin-web")]
    if let Some(ref t) = web_plugin {
        use bbs_plugin_api::Plugin;
        if let Err(e) = t.stop().await {
            warn!("web plugin stop error: {e}");
        }
    }

    #[cfg(feature = "transport-meshtastic")]
    if let Some(ref t) = meshtastic_transport {
        use bbs_plugin_api::Plugin;
        if let Err(e) = t.stop().await {
            warn!("meshtastic transport stop error: {e}");
        }
    }

    #[cfg(feature = "transport-mesh")]
    if let Some(ref t) = mesh_transport {
        use bbs_plugin_api::Plugin;
        if let Err(e) = t.stop().await {
            warn!("mesh transport stop error: {e}");
        }
    }

    #[cfg(feature = "transport-cli")]
    if let Some(ref t) = cli_transport {
        use bbs_plugin_api::Plugin;
        if let Err(e) = t.stop().await {
            warn!("cli transport stop error: {e}");
        }
    }

    info!("supply-drop-bbs stopped");
}

/// Initialise and start the mesh transport plugin.
///
/// Returns `None` if the plugin fails to initialise (error is logged and
/// the process exits). On success returns the running `MeshTransport`
/// handle so the supervisor can stop it on shutdown.
#[cfg(feature = "transport-mesh")]
async fn init_mesh_plugin(
    mesh_cfg: &bbs_mesh::MeshConfig,
    host: Arc<dyn bbs_plugin_api::Host>,
) -> Option<bbs_mesh::MeshTransport> {
    use bbs_plugin_api::Plugin;

    if !mesh_cfg.enabled {
        info!("mesh transport (MeshCore): disabled in config — skipping");
        return None;
    }

    let transport = match bbs_mesh::MeshTransport::init(mesh_cfg.clone(), host).await {
        Ok(t) => t,
        Err(e) => {
            error!("mesh transport init failed: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = transport.start().await {
        error!("mesh transport start failed: {e}");
        std::process::exit(1);
    }

    Some(transport)
}

/// Initialise and start the Meshtastic transport plugin.
///
/// Returns `None` when the plugin is disabled in config or fails to
/// initialise (error is logged and the process exits).
#[cfg(feature = "transport-meshtastic")]
async fn init_meshtastic_plugin(
    cfg: &bbs_meshtastic::MeshtasticConfig,
    host: Arc<dyn bbs_plugin_api::Host>,
) -> Option<bbs_meshtastic::MeshtasticTransport> {
    use bbs_plugin_api::Plugin;

    if !cfg.enabled {
        info!("meshtastic transport: disabled in config — skipping");
        return None;
    }

    let transport = match bbs_meshtastic::MeshtasticTransport::init(cfg.clone(), host).await {
        Ok(t) => t,
        Err(e) => {
            error!("meshtastic transport init failed: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = transport.start().await {
        error!("meshtastic transport start failed: {e}");
        std::process::exit(1);
    }

    Some(transport)
}

/// Initialise and start the CLI transport plugin.
///
/// Returns `None` when the plugin is disabled in config or fails to
/// initialise (error is logged and the process exits).  On success returns
/// the running `CliTransport` handle so the supervisor can stop it on
/// shutdown.
#[cfg(feature = "transport-cli")]
async fn init_cli_plugin(
    cli_cfg: &bbs_cli::CliConfig,
    host: Arc<dyn bbs_plugin_api::Host>,
) -> Option<bbs_cli::CliTransport> {
    use bbs_plugin_api::Plugin;

    if !cli_cfg.enabled {
        info!("cli transport: disabled in config — skipping");
        return None;
    }

    let transport = match bbs_cli::CliTransport::init(cli_cfg.clone(), host).await {
        Ok(t) => t,
        Err(e) => {
            error!("cli transport init failed: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = transport.start().await {
        error!("cli transport start failed: {e}");
        std::process::exit(1);
    }

    Some(transport)
}

/// Initialise and start the web admin plugin.
///
/// `config_file_path` is the absolute path of the config file that was
/// loaded at startup. When `Some`, it overrides the web config's default
/// `config_path` so backup zips always include the correct config.toml
/// regardless of the process working directory.
///
/// Returns `None` when the plugin is disabled in config or fails to
/// initialise (error is logged and the process exits).  On success returns
/// the running `WebPlugin` handle so the supervisor can stop it on
/// shutdown.
#[cfg(feature = "admin-web")]
async fn init_web_plugin(
    web_cfg: &bbs_web::WebConfig,
    host: Arc<dyn bbs_plugin_api::Host>,
    config_file_path: Option<String>,
    log_reload: LogReloadFn,
    error_store: std::sync::Arc<std::sync::Mutex<bbs_web::error_tracker::ErrorStore>>,
    error_tx: tokio::sync::broadcast::Sender<bbs_web::error_tracker::ErrorEntry>,
    app_log_buf: std::sync::Arc<std::sync::Mutex<bbs_web::log_capture::LogBuffer>>,
) -> Option<bbs_web::WebPlugin> {
    use bbs_plugin_api::Plugin;

    if !web_cfg.enabled {
        info!("web admin: disabled in config — skipping");
        return None;
    }

    let mut web_cfg = web_cfg.clone();

    // Inject the resolved absolute config path so backup zips always bundle
    // the correct file, even when running from a different working directory
    // (e.g. systemd services that start from /).
    if let Some(abs_path) = config_file_path {
        web_cfg.config_path = Some(abs_path);
    }

    let plugin = match bbs_web::WebPlugin::init(web_cfg, host).await {
        Ok(p) => p,
        Err(e) => {
            error!("web admin init failed: {e}");
            std::process::exit(1);
        }
    };
    plugin.set_log_reload(log_reload);
    plugin.set_error_store(error_store, error_tx);
    plugin.set_log_buffer(app_log_buf);

    if let Err(e) = plugin.start().await {
        error!("web admin start failed: {e}");
        std::process::exit(1);
    }

    Some(plugin)
}

fn cmd_metrics() {
    use sysinfo::{Disks, Networks, ProcessesToUpdate, System};

    let mut sys = System::new();
    sys.refresh_cpu_usage();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_usage();
    let cpu = sys.global_cpu_usage();

    sys.refresh_memory();
    let mem_used = sys.used_memory();
    let mem_total = sys.total_memory();
    let swap_used = sys.used_swap();
    let swap_total = sys.total_swap();

    let rss = sysinfo::get_current_pid().ok().and_then(|pid| {
        sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), false);
        sys.process(pid).map(|p| p.memory())
    });

    println!("── System Metrics ──────────────────────────────────────");
    println!("  CPU        : {cpu:.1}%");
    println!(
        "  Memory     : {} / {} ({:.0}%)",
        fmt_bytes(mem_used),
        fmt_bytes(mem_total),
        if mem_total > 0 {
            mem_used as f64 / mem_total as f64 * 100.0
        } else {
            0.0
        }
    );
    if swap_total > 0 {
        println!(
            "  Swap       : {} / {} ({:.0}%)",
            fmt_bytes(swap_used),
            fmt_bytes(swap_total),
            swap_used as f64 / swap_total as f64 * 100.0
        );
    }
    if let Some(r) = rss {
        println!("  Process RSS: {}", fmt_bytes(r));
    }

    let disks = Disks::new_with_refreshed_list();
    if !disks.is_empty() {
        println!("── Disks ───────────────────────────────────────────────");
        println!(
            "  {:<20} {:<8} {:>8} {:>8} {:>8}  used%",
            "mount", "fs", "used", "free", "total"
        );
        for d in disks.iter() {
            let avail = d.available_space();
            let total = d.total_space();
            let used = total.saturating_sub(avail);
            let pct = if total > 0 {
                used as f64 / total as f64 * 100.0
            } else {
                0.0
            };
            println!(
                "  {:<20} {:<8} {:>8} {:>8} {:>8}  {:.0}%",
                d.mount_point().to_string_lossy(),
                d.file_system().to_string_lossy(),
                fmt_bytes(used),
                fmt_bytes(avail),
                fmt_bytes(total),
                pct
            );
        }
    }

    let networks = Networks::new_with_refreshed_list();
    let active: Vec<_> = networks
        .iter()
        .filter(|(_, d)| d.total_received() > 0 || d.total_transmitted() > 0)
        .collect();
    if !active.is_empty() {
        println!("── Network (since boot) ────────────────────────────────");
        println!("  {:<16} {:>12} {:>12}", "interface", "RX", "TX");
        for (name, data) in &active {
            println!(
                "  {:<16} {:>12} {:>12}",
                name,
                fmt_bytes(data.total_received()),
                fmt_bytes(data.total_transmitted())
            );
        }
    }
}

fn fmt_bytes(b: u64) -> String {
    const GB: u64 = 1_073_741_824;
    const MB: u64 = 1_048_576;
    const KB: u64 = 1_024;
    if b >= GB {
        format!("{:.1}GB", b as f64 / GB as f64)
    } else if b >= MB {
        format!("{:.1}MB", b as f64 / MB as f64)
    } else if b >= KB {
        format!("{:.0}KB", b as f64 / KB as f64)
    } else {
        format!("{b}B")
    }
}

fn cmd_setup(config_path: Option<&std::path::Path>) {
    setup::run_wizard(config_path);
}

fn cmd_config(config_path: Option<&std::path::Path>, action: ConfigAction) {
    let cfg = match config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    };

    match action {
        ConfigAction::Check => {
            println!("config OK");
        }
        ConfigAction::Show => match toml::to_string_pretty(&cfg) {
            Ok(s) => print!("{s}"),
            Err(e) => {
                eprintln!("error serializing config: {e}");
                std::process::exit(1);
            }
        },
        ConfigAction::RequireVerify { enabled } => {
            let value = match enabled.to_ascii_lowercase().as_str() {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                other => {
                    eprintln!("error: expected on|off, got '{other}'");
                    std::process::exit(1);
                }
            };
            config_edit_bbs_bool(config_path, "require_verify", value);
            println!("require_verify = {value}. Restart the BBS for the change to take effect.");
        }
        ConfigAction::GuestRoom { value } => {
            if value.eq_ignore_ascii_case("off") {
                config_remove_bbs_key(config_path, "guest_room");
                println!("guest_room cleared. Restart the BBS for the change to take effect.");
            } else {
                config_edit_bbs_string(config_path, "guest_room", &value);
                println!(
                    "guest_room = \"{value}\". Restart the BBS for the change to take effect."
                );
            }
        }
    }
}

// ── Config-edit helpers ───────────────────────────────────────────────────────
//
// Used by `config require-verify` and `config guest-room` to update
// config.toml in place via toml_edit.

/// Open the config file and return its parsed document + resolved path.
fn open_config_for_edit(
    config_path: Option<&std::path::Path>,
) -> (std::path::PathBuf, toml_edit::DocumentMut) {
    #[cfg(feature = "transport-process")]
    let path = match config::resolve_config_path(config_path) {
        Some(p) => p,
        None => {
            eprintln!("error: no config file found");
            std::process::exit(1);
        }
    };
    #[cfg(not(feature = "transport-process"))]
    let path = {
        let explicit = config_path.map(|p| p.to_path_buf());
        let found = explicit.or_else(|| {
            [
                std::path::PathBuf::from("config.toml"),
                std::path::PathBuf::from("/etc/supply-drop-bbs/config.toml"),
            ]
            .iter()
            .find(|p| p.exists())
            .cloned()
        });
        match found {
            Some(p) => p,
            None => {
                eprintln!("error: no config file found");
                std::process::exit(1);
            }
        }
    };

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error reading {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    let doc = match content.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error parsing {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    (path, doc)
}

fn config_edit_bbs_bool(config_path: Option<&std::path::Path>, key: &str, value: bool) {
    let (path, mut doc) = open_config_for_edit(config_path);
    if doc.get("bbs").is_none() {
        doc["bbs"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["bbs"][key] = toml_edit::value(value);
    if let Err(e) = std::fs::write(&path, doc.to_string()) {
        eprintln!("error writing {}: {e}", path.display());
        std::process::exit(1);
    }
}

fn config_edit_bbs_string(config_path: Option<&std::path::Path>, key: &str, value: &str) {
    let (path, mut doc) = open_config_for_edit(config_path);
    if doc.get("bbs").is_none() {
        doc["bbs"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["bbs"][key] = toml_edit::value(value);
    if let Err(e) = std::fs::write(&path, doc.to_string()) {
        eprintln!("error writing {}: {e}", path.display());
        std::process::exit(1);
    }
}

fn config_remove_bbs_key(config_path: Option<&std::path::Path>, key: &str) {
    let (path, mut doc) = open_config_for_edit(config_path);
    if let Some(bbs) = doc.get_mut("bbs").and_then(|t| t.as_table_mut()) {
        bbs.remove(key);
    }
    if let Err(e) = std::fs::write(&path, doc.to_string()) {
        eprintln!("error writing {}: {e}", path.display());
        std::process::exit(1);
    }
}

/// Manage process transport plugins.
///
/// New plugins are written to `plugins.d/<name>.toml` alongside `config.toml`.
/// Legacy entries already in `config.toml` continue to be managed there.
/// Changes take effect on the next BBS restart (or immediately via the web UI).
#[cfg(feature = "transport-process")]
fn cmd_plugin(config_path: Option<&std::path::Path>, action: PluginAction) {
    use bbs_plugin_api::ProcessPluginConfig;

    let path = match config::resolve_config_path(config_path) {
        Some(p) => p,
        None => {
            eprintln!("error: no config file found");
            std::process::exit(1);
        }
    };

    let plugins_d = config::plugins_d_dir(&path);

    // Load plugins from config.toml.
    let mut toml_plugins: Vec<ProcessPluginConfig> = {
        let cfg = config::load(config_path).unwrap_or_default();
        cfg.plugins.process
    };

    // Load plugins from plugins.d (plugins.d takes precedence on name conflicts).
    let mut d_plugins: Vec<ProcessPluginConfig> = load_plugins_d_configs(&plugins_d);

    // Merged view (plugins.d wins): remove config.toml entries shadowed by plugins.d.
    let d_names: std::collections::HashSet<&str> =
        d_plugins.iter().map(|p| p.name.as_str()).collect();
    toml_plugins.retain(|p| !d_names.contains(p.name.as_str()));

    // Combined list for listing / existence checks.
    let all_plugins: Vec<&ProcessPluginConfig> =
        toml_plugins.iter().chain(d_plugins.iter()).collect();

    match action {
        PluginAction::List => {
            if all_plugins.is_empty() {
                println!("No process plugins configured.");
                return;
            }
            println!("{:<20} {:<12} {:<10} COMMAND", "NAME", "ENABLED", "SOURCE");
            for p in &all_plugins {
                let in_d = d_names.contains(p.name.as_str());
                let args = if p.args.is_empty() {
                    String::new()
                } else {
                    format!(" {}", p.args.join(" "))
                };
                println!(
                    "{:<20} {:<12} {:<10} {}{}",
                    p.name,
                    if p.enabled { "yes" } else { "no" },
                    if in_d { "plugins.d" } else { "config.toml" },
                    p.command,
                    args
                );
            }
        }

        PluginAction::Add {
            name,
            command,
            args,
            disabled,
            no_restart,
            restart_delay,
        } => {
            if all_plugins.iter().any(|p| p.name == name) {
                println!("plugin '{name}' is already configured — no changes made.");
                return;
            }
            let cfg = ProcessPluginConfig {
                name: name.clone(),
                command,
                args,
                enabled: !disabled,
                restart_on_crash: !no_restart,
                restart_delay_secs: restart_delay,
            };
            // Always write new plugins to plugins.d.
            if let Err(e) = std::fs::create_dir_all(&plugins_d) {
                eprintln!("error: cannot create plugins.d directory: {e}");
                std::process::exit(1);
            }
            write_plugin_file_sync(&plugins_d, &cfg);
            println!(
                "Added plugin '{name}' to {}. Restart the BBS to start it (or use the web UI).",
                plugins_d.display()
            );
        }

        PluginAction::Remove { name } => {
            let in_d = d_names.contains(name.as_str());
            let in_toml = toml_plugins.iter().any(|p| p.name == name);
            if !in_d && !in_toml {
                eprintln!("error: plugin '{name}' not found");
                std::process::exit(1);
            }
            if in_d {
                let file = plugins_d.join(format!("{name}.toml"));
                if let Err(e) = std::fs::remove_file(&file) {
                    eprintln!("error: cannot remove {}: {e}", file.display());
                    std::process::exit(1);
                }
            }
            if in_toml {
                toml_plugins.retain(|p| p.name != name);
                let raw = std::fs::read_to_string(&path).unwrap_or_default();
                let mut doc: toml_edit::DocumentMut = raw.parse().unwrap_or_default();
                write_plugins(&mut doc, &toml_plugins, &path);
            }
            println!("Removed plugin '{name}'.");
        }

        PluginAction::Enable { name } => {
            update_plugin_enabled(
                &name,
                true,
                &mut toml_plugins,
                &mut d_plugins,
                &path,
                &plugins_d,
            );
        }

        PluginAction::Disable { name } => {
            update_plugin_enabled(
                &name,
                false,
                &mut toml_plugins,
                &mut d_plugins,
                &path,
                &plugins_d,
            );
        }
    }
}

/// Load `[[plugins.process]]` entries from all `.toml` files in `dir`.
#[cfg(feature = "transport-process")]
fn load_plugins_d_configs(dir: &std::path::Path) -> Vec<bbs_plugin_api::ProcessPluginConfig> {
    use bbs_plugin_api::ProcessPluginConfig;

    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "toml").unwrap_or(false))
        .collect();
    paths.sort();

    #[derive(serde::Deserialize)]
    struct PluginFile {
        plugins: Option<PluginsSection>,
    }
    #[derive(serde::Deserialize)]
    struct PluginsSection {
        process: Option<Vec<ProcessPluginConfig>>,
    }

    let mut out = Vec::new();
    for path in &paths {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: cannot read {}: {e}", path.display());
                continue;
            }
        };
        if let Ok(f) = toml::from_str::<PluginFile>(&raw) {
            if let Some(plugins) = f.plugins.and_then(|p| p.process) {
                out.extend(plugins);
            }
        }
    }
    out
}

/// Write a single plugin config to `plugins.d/<name>.toml` (sync version for CLI).
#[cfg(feature = "transport-process")]
fn write_plugin_file_sync(dir: &std::path::Path, cfg: &bbs_plugin_api::ProcessPluginConfig) {
    #[derive(serde::Serialize)]
    struct Out<'a> {
        plugins: OutPlugins<'a>,
    }
    #[derive(serde::Serialize)]
    struct OutPlugins<'a> {
        process: &'a [bbs_plugin_api::ProcessPluginConfig],
    }
    let content = match toml::to_string_pretty(&Out {
        plugins: OutPlugins {
            process: std::slice::from_ref(cfg),
        },
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot serialise plugin config: {e}");
            std::process::exit(1);
        }
    };
    let path = dir.join(format!("{}.toml", cfg.name));
    if let Err(e) = std::fs::write(&path, content) {
        eprintln!("error: cannot write {}: {e}", path.display());
        std::process::exit(1);
    }
}

/// Enable or disable a plugin, writing to the appropriate file.
#[cfg(feature = "transport-process")]
fn update_plugin_enabled(
    name: &str,
    enabled: bool,
    toml_plugins: &mut [bbs_plugin_api::ProcessPluginConfig],
    d_plugins: &mut [bbs_plugin_api::ProcessPluginConfig],
    config_path: &std::path::Path,
    plugins_d: &std::path::Path,
) {
    let verb = if enabled { "Enabled" } else { "Disabled" };
    if let Some(p) = d_plugins.iter_mut().find(|p| p.name == name) {
        p.enabled = enabled;
        write_plugin_file_sync(plugins_d, p);
        println!("{verb} '{name}'.");
        return;
    }
    if let Some(p) = toml_plugins.iter_mut().find(|p| p.name == name) {
        p.enabled = enabled;
        let raw = std::fs::read_to_string(config_path).unwrap_or_default();
        let mut doc: toml_edit::DocumentMut = raw.parse().unwrap_or_default();
        write_plugins(&mut doc, toml_plugins, config_path);
        println!("{verb} '{name}'.");
        return;
    }
    eprintln!("error: plugin '{name}' not found");
    std::process::exit(1);
}

#[cfg(feature = "transport-process")]
fn write_plugins(
    doc: &mut toml_edit::DocumentMut,
    plugins: &[bbs_plugin_api::ProcessPluginConfig],
    path: &std::path::Path,
) {
    let mut aot = toml_edit::ArrayOfTables::new();
    for p in plugins {
        let mut tbl = toml_edit::Table::new();
        tbl.insert("name", toml_edit::value(&p.name));
        tbl.insert("command", toml_edit::value(&p.command));
        if !p.args.is_empty() {
            let mut arr = toml_edit::Array::default();
            for a in &p.args {
                arr.push(a.as_str());
            }
            tbl.insert("args", toml_edit::value(arr));
        }
        tbl.insert("enabled", toml_edit::value(p.enabled));
        tbl.insert("restart_on_crash", toml_edit::value(p.restart_on_crash));
        tbl.insert(
            "restart_delay_secs",
            toml_edit::value(p.restart_delay_secs as i64),
        );
        aot.push(tbl);
    }
    doc["plugins"]["process"] = toml_edit::Item::ArrayOfTables(aot);
    if let Err(e) = std::fs::write(path, doc.to_string()) {
        eprintln!("error writing config: {e}");
        std::process::exit(1);
    }
}

/// Apply any pending database migrations and exit.
///
/// `Database::open` runs migrations automatically, so this command is
/// equivalent to opening the database and immediately closing it.  It exists
/// as an explicit step for deployment scripts that want a clear "migrations
/// done" signal before starting the BBS process.
async fn cmd_migrate(cli: &Cli) {
    let cfg = load_config(cli);

    let db_path = cfg
        .database
        .path
        .as_ref()
        .expect("database.path set by resolve()");

    println!("Applying migrations to: {}", db_path.display());

    open_database(db_path).await;
    println!("Migrations applied successfully.");
}

/// Trigger an immediate database backup and report the result.
async fn cmd_backup(cli: &Cli) {
    let cfg = load_config(cli);

    let db_path = cfg
        .database
        .path
        .as_ref()
        .expect("database.path set by resolve()");

    let backup_dir = cfg
        .backup
        .directory
        .as_ref()
        .expect("backup.directory set by resolve()");

    let db = open_database(db_path).await;

    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(BbsHost::new(db));

    if let Err(e) = tokio::fs::create_dir_all(backup_dir).await {
        eprintln!("error creating backup directory: {e}");
        std::process::exit(1);
    }

    let dir_str = backup_dir.to_string_lossy();
    match host.admin_trigger_backup(&dir_str).await {
        Ok(rec) => {
            println!("Backup created: {}", rec.filename);
            println!("  size:     {} bytes", rec.size_bytes);
            println!("  location: {}", backup_dir.display());
        }
        Err(e) => {
            eprintln!("error creating backup: {e}");
            std::process::exit(1);
        }
    }
}

async fn cmd_user(cli: &Cli, action: &UserAction) {
    let cfg = load_config(cli);

    let db_path = cfg
        .database
        .path
        .as_ref()
        .expect("database.path set by resolve()");

    let db = open_database(db_path).await;

    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(BbsHost::new(db));

    match action {
        UserAction::List { pending } => match host.admin_list_users(None, 500, 0).await {
            Ok(users) => {
                let users: Vec<_> = if *pending {
                    users
                        .into_iter()
                        .filter(|u| u.permission_level == 0)
                        .collect()
                } else {
                    users
                };
                if users.is_empty() {
                    println!(
                        "{}",
                        if *pending {
                            "No pending users."
                        } else {
                            "No users."
                        }
                    );
                } else {
                    println!(
                        "{:<20} {:<12} {:<12} {:<10}",
                        "username", "level", "status", "created"
                    );
                    println!("{}", "-".repeat(56));
                    for u in &users {
                        let level = match u.permission_level {
                            0 => "unvalidated",
                            10 => "user",
                            50 => "aide",
                            100 => "sysop",
                            n => {
                                println!("  {:<20} level={n}", u.username);
                                continue;
                            }
                        };
                        println!(
                            "{:<20} {:<12} {:<12} {:<10}",
                            u.username,
                            level,
                            u.status,
                            u.created_at.get(..10).unwrap_or(&u.created_at),
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        },

        UserAction::Verify { username } => {
            match host.admin_update_user(username, None, Some(10)).await {
                Ok(()) => println!("verified: {username} (promoted to User)"),
                Err(bbs_plugin_api::HostError::NotFound(_)) => {
                    eprintln!("error: user '{username}' not found");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }

        UserAction::Create { username, sysop } => {
            let password = dialoguer::Password::new()
                .with_prompt("Password")
                .with_confirmation("Confirm password", "passwords do not match")
                .interact()
                .unwrap_or_else(|e| {
                    eprintln!("error reading password: {e}");
                    std::process::exit(1);
                });

            let level = if *sysop { 100u8 } else { 10u8 };
            let label = if *sysop { "sysop" } else { "user" };

            match host.admin_create_user(username, &password, level).await {
                Ok(()) => println!("created {label} account: {username}"),
                Err(bbs_plugin_api::HostError::PreconditionFailed(msg)) => {
                    eprintln!("error: {msg}");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }

        UserAction::SetPassword { username } => {
            let password = dialoguer::Password::new()
                .with_prompt(format!("New password for {username}"))
                .with_confirmation("Confirm password", "passwords do not match")
                .interact()
                .unwrap_or_else(|e| {
                    eprintln!("error reading password: {e}");
                    std::process::exit(1);
                });

            if password.len() < 6 {
                eprintln!("error: password must be at least 6 characters");
                std::process::exit(1);
            }

            match host.admin_set_password(username, &password).await {
                Ok(()) => println!("password reset for {username}"),
                Err(bbs_plugin_api::HostError::NotFound(_)) => {
                    eprintln!("error: user '{username}' not found");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }

        action => {
            let (username, new_level, label) = match action {
                UserAction::Promote { username } => (username, 100u8, "sysop"),
                UserAction::Demote { username } => (username, 10u8, "user"),
                UserAction::Create { .. }
                | UserAction::List { .. }
                | UserAction::Verify { .. }
                | UserAction::SetPassword { .. } => {
                    unreachable!()
                }
            };

            match host
                .admin_update_user(username, None, Some(new_level))
                .await
            {
                Ok(()) => println!("{username} promoted to {label} (level {new_level})"),
                Err(bbs_plugin_api::HostError::NotFound(_)) => {
                    eprintln!("error: user '{username}' not found");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

async fn cmd_room(cli: &Cli, action: &RoomAction) {
    let cfg = load_config(cli);

    let db_path = cfg
        .database
        .path
        .as_ref()
        .expect("database.path set by resolve()");

    let db = open_database(db_path).await;

    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(BbsHost::new(db));

    match action {
        RoomAction::Create { name, description } => {
            match host.admin_create_room(name, description.as_deref()).await {
                Ok(room) => println!("created room '{}' (id {})", room.name, room.id),
                Err(bbs_plugin_api::HostError::PreconditionFailed(msg)) => {
                    eprintln!("error: {msg}");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
    }
}

// ── Radio helpers ─────────────────────────────────────────────────────────────

#[cfg(feature = "transport-mesh")]
struct ResolvedRadio {
    frequency_hz: u32,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate: u8,
    tx_power_dbm: i32,
}

/// Resolve final radio parameters by layering: preset → config fields → CLI flags.
#[cfg(feature = "transport-mesh")]
fn resolve_radio(
    cfg_radio: Option<&bbs_mesh::config::RadioConfig>,
    preset_override: Option<&str>,
    freq_override: Option<u64>,
    bw_override: Option<u32>,
    sf_override: Option<u8>,
    cr_override: Option<u8>,
    pwr_override: Option<i32>,
) -> Result<ResolvedRadio, String> {
    let mut freq: Option<u32> = None;
    let mut bw: Option<u32> = None;
    let mut sf: Option<u8> = None;
    let mut cr: Option<u8> = None;
    let mut pwr: Option<i32> = None;

    // 1. Named preset (CLI flag > config.toml preset).
    let preset_name = preset_override.or_else(|| cfg_radio.and_then(|r| r.preset.as_deref()));
    if let Some(name) = preset_name {
        let p = mesh_presets::REGION_PRESETS
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                format!(
                    "unknown preset '{name}' — run \
                     'supply-drop-bbs node set-radio --list-presets' to see valid names"
                )
            })?;
        freq = Some(p.frequency_hz as u32);
        bw = Some(p.bandwidth_hz);
        sf = Some(p.spreading_factor);
        cr = Some(p.coding_rate);
        pwr = Some(p.tx_power_dbm);
    }

    // 2. Individual config.toml fields overlay preset.
    if let Some(r) = cfg_radio {
        if let Some(v) = r.frequency_hz {
            freq = Some(v as u32);
        }
        if let Some(v) = r.bandwidth_hz {
            bw = Some(v);
        }
        if let Some(v) = r.spreading_factor {
            sf = Some(v);
        }
        if let Some(v) = r.coding_rate {
            cr = Some(v);
        }
        if let Some(v) = r.tx_power_dbm {
            pwr = Some(v);
        }
    }

    // 3. CLI flag overrides take highest precedence.
    if let Some(v) = freq_override {
        freq = Some(v as u32);
    }
    if let Some(v) = bw_override {
        bw = Some(v);
    }
    if let Some(v) = sf_override {
        sf = Some(v);
    }
    if let Some(v) = cr_override {
        cr = Some(v);
    }
    if let Some(v) = pwr_override {
        pwr = Some(v);
    }

    Ok(ResolvedRadio {
        frequency_hz: freq
            .ok_or("frequency_hz not set — specify a preset or frequency_hz in config")?,
        bandwidth_hz: bw.ok_or("bandwidth_hz not set")?,
        spreading_factor: sf.ok_or("spreading_factor not set")?,
        coding_rate: cr.ok_or("coding_rate not set")?,
        tx_power_dbm: pwr.ok_or("tx_power_dbm not set")?,
    })
}

/// Write radio settings back to config.toml under `[plugins.mesh.radio]`.
#[cfg(feature = "transport-mesh")]
fn save_radio_config(config_path: Option<&std::path::Path>, r: &ResolvedRadio) {
    #[cfg(feature = "transport-process")]
    let path_opt = config::resolve_config_path(config_path);
    #[cfg(not(feature = "transport-process"))]
    let path_opt = {
        let explicit = config_path.map(|p| p.to_path_buf());
        explicit.or_else(|| {
            [
                std::path::PathBuf::from("config.toml"),
                std::path::PathBuf::from("/etc/supply-drop-bbs/config.toml"),
            ]
            .iter()
            .find(|p| p.exists())
            .cloned()
        })
    };

    let path = match path_opt {
        Some(p) => p,
        None => {
            eprintln!("warning: --save: no config file found, skipping write");
            return;
        }
    };

    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = content.parse().unwrap_or_default();

    // Ensure [plugins] and [plugins.mesh] exist.
    if doc.get("plugins").is_none() {
        doc["plugins"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if doc["plugins"].get("mesh").is_none() {
        doc["plugins"]["mesh"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    // Write [plugins.mesh.radio] fields.
    doc["plugins"]["mesh"]["radio"]["frequency_hz"] = toml_edit::value(r.frequency_hz as i64);
    doc["plugins"]["mesh"]["radio"]["bandwidth_hz"] = toml_edit::value(r.bandwidth_hz as i64);
    doc["plugins"]["mesh"]["radio"]["spreading_factor"] =
        toml_edit::value(r.spreading_factor as i64);
    doc["plugins"]["mesh"]["radio"]["coding_rate"] = toml_edit::value(r.coding_rate as i64);
    doc["plugins"]["mesh"]["radio"]["tx_power_dbm"] = toml_edit::value(r.tx_power_dbm as i64);

    if let Err(e) = std::fs::write(&path, doc.to_string()) {
        eprintln!("warning: --save: could not write {}: {e}", path.display());
    } else {
        eprintln!("Saved radio config to {}.", path.display());
    }
}

// ── Meshtastic node command handler ──────────────────────────────────────────

#[cfg(feature = "transport-meshtastic")]
async fn cmd_node_meshtastic(config_path: Option<&std::path::Path>, action: NodeAction) {
    let cfg = config::load(config_path).unwrap_or_default();
    let mt_cfg = &cfg.plugins.meshtastic;

    let (port_flag, baud_flag, addr_flag, is_set_radio) = match &action {
        NodeAction::SetMeshtasticRadio { port, baud, addr } => {
            (port.clone(), *baud, addr.clone(), true)
        }
        NodeAction::SetMeshtasticOwner { port, baud, addr } => {
            (port.clone(), *baud, addr.clone(), false)
        }
        _ => unreachable!(),
    };

    let result = if is_set_radio {
        bbs_meshtastic::apply_radio_from_config(mt_cfg, port_flag, baud_flag, addr_flag).await
    } else {
        bbs_meshtastic::apply_owner_from_config(mt_cfg, port_flag, baud_flag, addr_flag).await
    };

    match result {
        Ok(()) => {
            if is_set_radio {
                eprintln!("Meshtastic radio config applied successfully.");
            } else {
                eprintln!("Meshtastic owner info applied successfully.");
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

async fn cmd_node(config_path: Option<&std::path::Path>, action: NodeAction) {
    // ── Meshtastic node commands ──────────────────────────────────────────────
    #[cfg(feature = "transport-meshtastic")]
    match &action {
        NodeAction::SetMeshtasticRadio { .. } | NodeAction::SetMeshtasticOwner { .. } => {
            cmd_node_meshtastic(config_path, action).await;
            return;
        }
        _ => {}
    }

    // ── MeshCore node commands ────────────────────────────────────────────────
    #[cfg(feature = "transport-mesh")]
    {
        use meshcore_companion::{
            client::{ClientEvent, CompanionClient, SerialConfig},
            constants::APP_TARGET_VER_V3,
            frame::{InboundFrame, OutboundFrame},
        };
        use std::time::Duration;
        use tokio::time::timeout;

        // ── Resolve port / baud from flags or config ──────────────────────
        let cfg = config::load(config_path).unwrap_or_default();
        let mesh_cfg = &cfg.plugins.mesh;

        // --list-presets is handled before we even open the port.
        if let NodeAction::SetRadio {
            list_presets: true, ..
        } = &action
        {
            println!("{:<28} FREQUENCY   BANDWIDTH  SF  CR  PWR", "PRESET NAME");
            println!("{}", "-".repeat(72));
            for p in mesh_presets::REGION_PRESETS {
                println!(
                    "{:<28} {:>10.3} MHz  {:>6} kHz  {:>2}  {:>2}  {:>3} dBm",
                    p.name,
                    p.frequency_hz as f64 / 1_000_000.0,
                    p.bandwidth_hz / 1_000,
                    p.spreading_factor,
                    p.coding_rate,
                    p.tx_power_dbm,
                );
            }
            return;
        }

        let (port_flag, baud_flag) = match &action {
            NodeAction::ShowKey { port, baud } => (port.clone(), *baud),
            NodeAction::ExportKey { port, baud } => (port.clone(), *baud),
            NodeAction::ImportKey { port, baud, .. } => (port.clone(), *baud),
            NodeAction::SetRadio { port, baud, .. } => (port.clone(), *baud),
            // Meshtastic commands are intercepted and returned-from early above.
            // Only present when the meshtastic feature adds those NodeAction
            // variants; without it the arms above are already exhaustive.
            #[cfg(feature = "transport-meshtastic")]
            _ => unreachable!("Meshtastic commands should have been handled above"),
        };

        let port = match port_flag.or_else(|| {
            if mesh_cfg.connection_type == bbs_mesh::config::ConnectionType::Serial {
                mesh_cfg.serial_port.clone()
            } else {
                None
            }
        }) {
            Some(p) => p,
            None => {
                eprintln!(
                    "error: no serial port specified. Use --port or set [plugins.mesh] \
                     connection_type = \"serial\" and serial_port in config.toml"
                );
                std::process::exit(1);
            }
        };

        let baud = baud_flag.unwrap_or(mesh_cfg.baud_rate);

        eprintln!("Connecting to {port} at {baud} baud...");

        // ── Resolve radio params for set-radio ────────────────────────────
        // Do this before connecting so we can fail fast on bad config.
        let resolved_radio: Option<ResolvedRadio> = if let NodeAction::SetRadio {
            preset,
            frequency_hz,
            bandwidth_hz,
            spreading_factor,
            coding_rate,
            tx_power_dbm,
            save,
            ..
        } = &action
        {
            match resolve_radio(
                cfg.plugins.mesh.radio.as_ref(),
                preset.as_deref(),
                *frequency_hz,
                *bandwidth_hz,
                *spreading_factor,
                *coding_rate,
                *tx_power_dbm,
            ) {
                Ok(r) => {
                    eprintln!(
                        "Radio: {:.3} MHz  BW={} kHz  SF={}  CR={}  PWR={} dBm",
                        r.frequency_hz as f64 / 1_000_000.0,
                        r.bandwidth_hz / 1_000,
                        r.spreading_factor,
                        r.coding_rate,
                        r.tx_power_dbm,
                    );
                    if *save {
                        save_radio_config(config_path, &r);
                    }
                    Some(r)
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    eprintln!(
                        "Tip: add a [plugins.mesh.radio] section to config.toml, or use --preset."
                    );
                    eprintln!(
                        "     Run 'supply-drop-bbs node set-radio --list-presets' for preset names."
                    );
                    std::process::exit(1);
                }
            }
        } else {
            None
        };

        // ── Build the command to send after Connected ─────────────────────
        let cmd_to_send: OutboundFrame = match &action {
            NodeAction::ShowKey { .. } => {
                // We only need SelfInfo from the Connected event — no extra command needed.
                // Send GetBattAndStorage as a no-op to keep the connection alive.
                OutboundFrame::GetBattAndStorage
            }
            NodeAction::ExportKey { .. } => OutboundFrame::ExportPrivateKey,
            NodeAction::ImportKey { key, .. } => {
                let hex = key.trim();
                if hex.len() != 64 {
                    eprintln!(
                        "error: key must be exactly 64 hex characters ({} given)",
                        hex.len()
                    );
                    std::process::exit(1);
                }
                let mut bytes = [0u8; 32];
                for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
                    let s = std::str::from_utf8(chunk).unwrap_or("??");
                    bytes[i] = match u8::from_str_radix(s, 16) {
                        Ok(b) => b,
                        Err(_) => {
                            eprintln!("error: invalid hex character in key at position {}", i * 2);
                            std::process::exit(1);
                        }
                    };
                }
                OutboundFrame::ImportPrivateKey { key: bytes }
            }
            NodeAction::SetRadio { .. } => {
                // First frame sent in the event loop after the radio params frame.
                // Placeholder — the actual send is handled specially below.
                OutboundFrame::GetBattAndStorage
            }
            // Meshtastic commands are handled before this block — unreachable here.
            // Gated so the match stays exhaustive (no unreachable arm) in builds
            // without the meshtastic feature, where those variants don't exist.
            #[cfg(feature = "transport-meshtastic")]
            _ => unreachable!("Meshtastic commands should have been handled above"),
        };

        let serial_cfg = SerialConfig {
            port,
            baud_rate: baud,
            app_target_version: APP_TARGET_VER_V3,
            // Don't retry on CLI — if the port is unavailable, fail fast.
            reconnect_delay_initial: Duration::from_secs(60),
            reconnect_delay_max: Duration::from_secs(60),
        };

        let mut client = CompanionClient::connect_serial(serial_cfg);

        // ── Wait up to 15 s for the whole operation ───────────────────────
        let result = timeout(Duration::from_secs(15), async {
            let mut connected = false;
            // For set-radio: track how many Ok responses we've received
            // (one for SetRadioParams, one for SetRadioTxPower).
            let mut radio_ok_count: u8 = 0;

            while let Some(event) = client.recv().await {
                match event {
                    ClientEvent::Connected { self_info } => {
                        connected = true;
                        if let Some(ref info) = self_info {
                            let pk_hex: String =
                                info.pubkey.iter().map(|b| format!("{b:02x}")).collect();
                            eprintln!("Connected: {} ({})", info.node_name, pk_hex);
                        } else {
                            eprintln!("Connected (no SelfInfo)");
                        }

                        // For show-key we're done once we have SelfInfo.
                        if let NodeAction::ShowKey { .. } = &action {
                            match self_info {
                                Some(info) => {
                                    let hex: String =
                                        info.pubkey.iter().map(|b| format!("{b:02x}")).collect();
                                    println!("{hex}");
                                    return Ok::<(), String>(());
                                }
                                None => {
                                    return Err(
                                        "device did not return SelfInfo — public key unavailable"
                                            .to_owned(),
                                    );
                                }
                            }
                        }

                        // For set-radio, send SetRadioParams first.
                        if let NodeAction::SetRadio { .. } = &action {
                            if let Some(ref r) = resolved_radio {
                                let frame = OutboundFrame::SetRadioParams {
                                    frequency_hz: r.frequency_hz,
                                    bandwidth_hz: r.bandwidth_hz,
                                    spreading_factor: r.spreading_factor,
                                    coding_rate: r.coding_rate,
                                };
                                if let Err(e) = client.send(frame).await {
                                    return Err(format!("send SetRadioParams failed: {e}"));
                                }
                            }
                            continue;
                        }

                        // For export/import, send the command now.
                        if let Err(e) = client.send(cmd_to_send.clone()).await {
                            return Err(format!("send failed: {e}"));
                        }
                    }
                    ClientEvent::Frame(frame) => match frame {
                        InboundFrame::PrivateKey { ref key } => {
                            if let NodeAction::ExportKey { .. } = &action {
                                let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
                                println!("{hex}");
                                return Ok(());
                            }
                        }
                        InboundFrame::Ok => {
                            if let NodeAction::ImportKey { .. } = &action {
                                eprintln!("Key imported successfully.");
                                return Ok(());
                            }
                            if let NodeAction::SetRadio { .. } = &action {
                                radio_ok_count += 1;
                                if radio_ok_count == 1 {
                                    // SetRadioParams acknowledged — now send SetRadioTxPower.
                                    if let Some(ref r) = resolved_radio {
                                        let frame = OutboundFrame::SetRadioTxPower {
                                            power_dbm: r.tx_power_dbm as i8,
                                        };
                                        if let Err(e) = client.send(frame).await {
                                            return Err(format!(
                                                "send SetRadioTxPower failed: {e}"
                                            ));
                                        }
                                    }
                                } else {
                                    // Both commands acknowledged — done.
                                    eprintln!("Radio configured successfully.");
                                    return Ok(());
                                }
                            }
                        }
                        InboundFrame::Err { error_code } => {
                            return Err(format!("device returned error code {error_code}"));
                        }
                        _ => {} // ignore other frames
                    },
                    ClientEvent::Disconnected { will_retry: false } => {
                        if !connected {
                            return Err("connection failed".to_owned());
                        }
                        return Err("disconnected before operation completed".to_owned());
                    }
                    ClientEvent::Disconnected { will_retry: true } => {
                        if !connected {
                            return Err("connection failed".to_owned());
                        }
                    }
                }
            }
            Err("connection closed unexpectedly".to_owned())
        })
        .await;

        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
            Err(_) => {
                eprintln!("error: timed out waiting for device response (15s)");
                std::process::exit(1);
            }
        }
    }

    #[cfg(not(feature = "transport-mesh"))]
    {
        let _ = (config_path, action);
        eprintln!(
            "error: the `node` subcommand requires the `transport-mesh` feature. \
             Rebuild with --features transport-mesh."
        );
        std::process::exit(1);
    }
}

// ── Backup helpers ────────────────────────────────────────────────────────────

/// Delete backups that fall outside the daily/weekly retention window.
///
/// Sorted newest-first, we keep the first occurrence of each unique calendar
/// date up to `keep_daily` dates, and the first occurrence of each unique ISO
/// week up to `keep_weekly` weeks.  Everything else is deleted.
async fn prune_backups(
    host: &Arc<dyn bbs_plugin_api::Host>,
    backup_dir: &str,
    keep_daily: u32,
    keep_weekly: u32,
) {
    let mut backups = match host.admin_list_backups(backup_dir).await {
        Ok(b) => b,
        Err(e) => {
            warn!("backup pruning: list failed: {e}");
            return;
        }
    };

    // Newest first.
    backups.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    info!(
        dir = backup_dir,
        total = backups.len(),
        keep_daily,
        keep_weekly,
        "backup pruning: evaluating retention"
    );

    let mut daily_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut weekly_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut keep: std::collections::HashSet<String> = std::collections::HashSet::new();

    for rec in &backups {
        let date_key = rec.created_at.get(..10).unwrap_or("").to_owned();
        let week_key = iso_week_key(&rec.created_at);

        let new_day = !date_key.is_empty()
            && daily_seen.len() < keep_daily as usize
            && daily_seen.insert(date_key);
        let new_week = !week_key.is_empty()
            && weekly_seen.len() < keep_weekly as usize
            && weekly_seen.insert(week_key);

        if new_day || new_week {
            keep.insert(rec.filename.clone());
        }
    }

    let prune_count = backups
        .iter()
        .filter(|r| !keep.contains(&r.filename))
        .count();
    info!(
        keeping = keep.len(),
        pruning = prune_count,
        "backup pruning: retention decision"
    );

    for rec in &backups {
        if !keep.contains(&rec.filename) {
            match host.admin_delete_backup(backup_dir, &rec.filename).await {
                Ok(()) => info!(filename = %rec.filename, "pruned old backup"),
                Err(e) => warn!(filename = %rec.filename, "failed to prune backup: {e}"),
            }
        }
    }
}

/// Return `"YYYY-Www"` for an RFC 3339 timestamp string, or empty string on failure.
fn iso_week_key(rfc3339: &str) -> String {
    let date_str = rfc3339.get(..10).unwrap_or("");
    use time::macros::format_description;
    time::Date::parse(date_str, &format_description!("[year]-[month]-[day]"))
        .map(|d| {
            let (year, week, _) = d.to_iso_week_date();
            format!("{year}-W{week:02}")
        })
        .unwrap_or_default()
}

// ── Tracing init ──────────────────────────────────────────────────────────────

/// Initialise the global tracing subscriber from the resolved logging config.
///
/// Level precedence (highest wins):
/// 1. `RUST_LOG` env var (standard tracing-subscriber override, always honoured)
/// 2. `logging.level` (config file / `--log-level` flag, applied above)
/// 3. Compiled-in `INFO` default
///
/// Per-target overrides from `logging.targets` are added as env-filter
/// directives after the root directive.
///
/// Returns a type-erased closure that the web admin can call to change the
/// log level at runtime without a restart. Accepts a level string such as
/// `"DEBUG"` or `"INFO"`. Target-specific overrides from config are NOT
/// preserved after a runtime reload (the reload replaces the whole filter).
/// Tracing init that also installs the in-process error tracker layer.
///
/// Compiled only when the `admin-web` feature is active.  The concrete
/// `ErrorTrackerLayer` type is generic over any `Subscriber`, so it
/// composes cleanly with the reload and fmt layers.
#[cfg(feature = "admin-web")]
fn init_tracing_with_error_layer(
    cfg: &config::LoggingConfig,
    error_layer: bbs_web::error_tracker::ErrorTrackerLayer,
    log_layer: bbs_web::log_capture::LogCaptureLayer,
) -> LogReloadFn {
    let root_level: tracing::Level = cfg.level.into();

    let mut filter = EnvFilter::builder()
        .with_default_directive(root_level.into())
        .from_env_lossy();

    for (target, target_level) in &cfg.targets {
        let tl: tracing::Level = (*target_level).into();
        if let Ok(directive) = format!("{target}={}", tl.as_str()).parse() {
            filter = filter.add_directive(directive);
        }
    }

    let (filter_layer, reload_handle) = reload::Layer::new(filter);

    match cfg.format {
        config::LogFormat::Pretty => {
            tracing_subscriber::registry()
                .with(filter_layer)
                .with(error_layer)
                .with(log_layer)
                .with(tracing_subscriber::fmt::layer().pretty())
                .init();
        }
        config::LogFormat::Json => {
            tracing_subscriber::registry()
                .with(filter_layer)
                .with(error_layer)
                .with(log_layer)
                .with(tracing_subscriber::fmt::layer().json())
                .init();
        }
        config::LogFormat::Compact => {
            tracing_subscriber::registry()
                .with(filter_layer)
                .with(error_layer)
                .with(log_layer)
                .with(tracing_subscriber::fmt::layer().compact())
                .init();
        }
    }

    Arc::new(move |level: &str| {
        let new_filter =
            EnvFilter::try_new(level).map_err(|e| format!("invalid log filter: {e}"))?;
        reload_handle
            .reload(new_filter)
            .map_err(|e| format!("reload failed: {e}"))
    })
}

#[cfg(not(feature = "admin-web"))]
fn init_tracing(cfg: &config::LoggingConfig) -> LogReloadFn {
    let root_level: tracing::Level = cfg.level.into();

    let mut filter = EnvFilter::builder()
        .with_default_directive(root_level.into())
        .from_env_lossy();

    for (target, target_level) in &cfg.targets {
        let tl: tracing::Level = (*target_level).into();
        if let Ok(directive) = format!("{target}={}", tl.as_str()).parse() {
            filter = filter.add_directive(directive);
        }
    }

    let (filter_layer, reload_handle) = reload::Layer::new(filter);

    match cfg.format {
        config::LogFormat::Pretty => {
            tracing_subscriber::registry()
                .with(filter_layer)
                .with(tracing_subscriber::fmt::layer().pretty())
                .init();
        }
        config::LogFormat::Json => {
            tracing_subscriber::registry()
                .with(filter_layer)
                .with(tracing_subscriber::fmt::layer().json())
                .init();
        }
        config::LogFormat::Compact => {
            tracing_subscriber::registry()
                .with(filter_layer)
                .with(tracing_subscriber::fmt::layer().compact())
                .init();
        }
    }

    Arc::new(move |level: &str| {
        let new_filter =
            EnvFilter::try_new(level).map_err(|e| format!("invalid log filter: {e}"))?;
        reload_handle
            .reload(new_filter)
            .map_err(|e| format!("reload failed: {e}"))
    })
}
