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

mod config;
mod setup;

use std::{path::PathBuf, sync::Arc};

use bbs_core::{BbsHost, Database};
use clap::{Parser, Subcommand};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

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
    /// installs systemd unit(s). \[TBD\]
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
        Some(Commands::User { action }) => cmd_user(config_path.as_deref(), action).await,
        Some(Commands::Room { action }) => cmd_room(config_path.as_deref(), action).await,
        #[cfg(feature = "transport-process")]
        Some(Commands::Plugin { action }) => cmd_plugin(config_path.as_deref(), action),
    }
}

// ── Subcommand handlers ───────────────────────────────────────────────────────

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
    init_tracing(&cfg.logging);

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
    let host: Arc<dyn bbs_plugin_api::Host> =
        Arc::new(BbsHost::with_location(db, cfg.location.as_coords()));
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
    let mesh_transport = init_mesh_plugin(&cfg.plugins.mesh, Arc::clone(&host)).await;

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
        ProcessPluginManager::new(cfg.plugins.process.clone(), Arc::clone(&host), config_path).await
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
        let wp = init_web_plugin(&cfg.plugins.web, Arc::clone(&host), cfg_abs).await;
        #[cfg(feature = "transport-process")]
        if let Some(ref plugin) = wp {
            let registry =
                Arc::clone(&process_registry) as Arc<dyn bbs_plugin_api::PluginRegistryApi>;
            plugin.set_plugin_registry(registry);
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
) -> Option<bbs_web::WebPlugin> {
    use bbs_plugin_api::Plugin;

    if !web_cfg.enabled {
        info!("web admin: disabled in config — skipping");
        return None;
    }

    // Inject the resolved absolute config path so backup zips always bundle
    // the correct file, even when running from a different working directory
    // (e.g. systemd services that start from /).
    let mut web_cfg = web_cfg.clone();
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

    if let Err(e) = plugin.start().await {
        error!("web admin start failed: {e}");
        std::process::exit(1);
    }

    Some(plugin)
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
    }
}

/// Manage process transport plugins by editing config.toml directly.
///
/// Changes take effect on the next BBS restart. Use the web admin UI
/// for live runtime management (start/stop/restart without restarting).
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

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading config: {e}");
            std::process::exit(1);
        }
    };

    let mut doc: toml_edit::DocumentMut = match raw.parse() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error parsing config: {e}");
            std::process::exit(1);
        }
    };

    // Read current plugins list.
    let mut plugins: Vec<ProcessPluginConfig> = {
        let cfg = config::load(config_path).unwrap_or_default();
        #[cfg(feature = "transport-process")]
        {
            cfg.plugins.process
        }
        #[cfg(not(feature = "transport-process"))]
        {
            vec![]
        }
    };

    match action {
        PluginAction::List => {
            if plugins.is_empty() {
                println!("No process plugins configured.");
                return;
            }
            println!("{:<20} {:<12} COMMAND", "NAME", "ENABLED");
            for p in &plugins {
                let args = if p.args.is_empty() {
                    String::new()
                } else {
                    format!(" {}", p.args.join(" "))
                };
                println!(
                    "{:<20} {:<12} {}{}",
                    p.name,
                    if p.enabled { "yes" } else { "no" },
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
            if plugins.iter().any(|p| p.name == name) {
                eprintln!("error: plugin '{name}' already exists");
                std::process::exit(1);
            }
            plugins.push(ProcessPluginConfig {
                name: name.clone(),
                command,
                args,
                enabled: !disabled,
                restart_on_crash: !no_restart,
                restart_delay_secs: restart_delay,
            });
            write_plugins(&mut doc, &plugins, &path);
            println!("Added plugin '{name}'. Restart the BBS to start it (or use the web UI).");
        }

        PluginAction::Remove { name } => {
            let before = plugins.len();
            plugins.retain(|p| p.name != name);
            if plugins.len() == before {
                eprintln!("error: plugin '{name}' not found");
                std::process::exit(1);
            }
            write_plugins(&mut doc, &plugins, &path);
            println!("Removed plugin '{name}'.");
        }

        PluginAction::Enable { name } => {
            let p = plugins.iter_mut().find(|p| p.name == name);
            match p {
                Some(p) => {
                    p.enabled = true;
                    write_plugins(&mut doc, &plugins, &path);
                    println!("Enabled '{name}'.");
                }
                None => {
                    eprintln!("error: plugin '{name}' not found");
                    std::process::exit(1);
                }
            }
        }

        PluginAction::Disable { name } => {
            let p = plugins.iter_mut().find(|p| p.name == name);
            match p {
                Some(p) => {
                    p.enabled = false;
                    write_plugins(&mut doc, &plugins, &path);
                    println!("Disabled '{name}'.");
                }
                None => {
                    eprintln!("error: plugin '{name}' not found");
                    std::process::exit(1);
                }
            }
        }
    }
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
    let cfg = match config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error loading config: {e}");
            std::process::exit(1);
        }
    };

    let db_path = cfg
        .database
        .path
        .as_ref()
        .expect("database.path set by resolve()");

    println!("Applying migrations to: {}", db_path.display());

    match Database::open(&db_path.to_string_lossy()).await {
        Ok(_db) => println!("Migrations applied successfully."),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

/// Trigger an immediate database backup and report the result.
async fn cmd_backup(cli: &Cli) {
    let cfg = match config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error loading config: {e}");
            std::process::exit(1);
        }
    };

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

    let db = match Database::open(&db_path.to_string_lossy()).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening database: {e}");
            std::process::exit(1);
        }
    };

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

async fn cmd_user(config_path: Option<&std::path::Path>, action: UserAction) {
    let cfg = match config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error loading config: {e}");
            std::process::exit(1);
        }
    };

    let db_path = cfg
        .database
        .path
        .as_ref()
        .expect("database.path set by resolve()");

    let db = match Database::open(&db_path.to_string_lossy()).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening database: {e}");
            std::process::exit(1);
        }
    };

    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(BbsHost::new(db));

    match action {
        UserAction::List { pending } => match host.admin_list_users(None, 500, 0).await {
            Ok(users) => {
                let users: Vec<_> = if pending {
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
                        if pending {
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
            match host.admin_update_user(&username, None, Some(10)).await {
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

            let level = if sysop { 100u8 } else { 10u8 };
            let label = if sysop { "sysop" } else { "user" };

            match host.admin_create_user(&username, &password, level).await {
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

        action => {
            let (username, new_level, label) = match action {
                UserAction::Promote { username } => (username, 100u8, "sysop"),
                UserAction::Demote { username } => (username, 10u8, "user"),
                UserAction::Create { .. } | UserAction::List { .. } | UserAction::Verify { .. } => {
                    unreachable!()
                }
            };

            match host
                .admin_update_user(&username, None, Some(new_level))
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

async fn cmd_room(config_path: Option<&std::path::Path>, action: RoomAction) {
    let cfg = match config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error loading config: {e}");
            std::process::exit(1);
        }
    };

    let db_path = cfg
        .database
        .path
        .as_ref()
        .expect("database.path set by resolve()");

    let db = match Database::open(&db_path.to_string_lossy()).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening database: {e}");
            std::process::exit(1);
        }
    };

    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(BbsHost::new(db));

    match action {
        RoomAction::Create { name, description } => {
            match host.admin_create_room(&name, description.as_deref()).await {
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
fn init_tracing(cfg: &config::LoggingConfig) {
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

    match cfg.format {
        config::LogFormat::Pretty => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .pretty()
                .init();
        }
        config::LogFormat::Json => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .json()
                .init();
        }
        config::LogFormat::Compact => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .compact()
                .init();
        }
    }
}
