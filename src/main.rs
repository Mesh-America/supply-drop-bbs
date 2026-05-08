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
    /// installs systemd unit(s). [TBD]
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
        Some(Commands::Migrate) => cmd_migrate(&cli),
        Some(Commands::Backup) => cmd_backup(&cli),
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
    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(BbsHost::new(db));
    info!("host initialised");

    // ── 6. Plugins ────────────────────────────────────────────────────────────
    //
    // Each plugin is init'd then start'd.  Errors at init abort startup;
    // errors at start are fatal.  Plugins are stopped in reverse order on
    // shutdown.  Only compiled-in plugins appear here (cargo features gate
    // what's available — see ADR-0004).

    #[cfg(feature = "transport-mesh")]
    let mesh_transport = init_mesh_plugin(&cfg.plugins.mesh, Arc::clone(&host)).await;

    // ── 7. Wait for shutdown signal ───────────────────────────────────────────
    info!("supply-drop-bbs ready — press Ctrl-C to stop");

    match tokio::signal::ctrl_c().await {
        Ok(()) => info!("Ctrl-C received — shutting down"),
        Err(e) => error!("error waiting for Ctrl-C: {e}"),
    }

    // ── 8. Stop plugins (reverse order) ──────────────────────────────────────
    #[cfg(feature = "transport-mesh")]
    if let Some(ref t) = mesh_transport {
        use bbs_plugin_api::Plugin;
        if let Err(e) = t.stop().await {
            warn!("mesh transport stop error: {e}");
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

fn cmd_migrate(_cli: &Cli) {
    // TODO: open database, run sqlx::migrate!().
    eprintln!("error: migrate not yet implemented.");
    std::process::exit(1);
}

fn cmd_backup(_cli: &Cli) {
    // TODO: open database, trigger VACUUM INTO backup.
    eprintln!("error: backup not yet implemented.");
    std::process::exit(1);
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
