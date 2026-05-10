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
        Some(Commands::User { action }) => cmd_user(config_path.as_deref(), action).await,
        Some(Commands::Room { action }) => cmd_room(config_path.as_deref(), action).await,
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

    // ── 6. Plugins ────────────────────────────────────────────────────────────
    //
    // Each plugin is init'd then start'd.  Errors at init abort startup;
    // errors at start are fatal.  Plugins are stopped in reverse order on
    // shutdown.  Only compiled-in plugins appear here (cargo features gate
    // what's available — see ADR-0004).

    #[cfg(feature = "transport-cli")]
    let cli_transport = init_cli_plugin(&cfg.plugins.cli, Arc::clone(&host)).await;

    #[cfg(feature = "transport-mesh")]
    let mesh_transport = init_mesh_plugin(&cfg.plugins.mesh, Arc::clone(&host)).await;

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
        init_web_plugin(&cfg.plugins.web, Arc::clone(&host), cfg_abs).await
    };

    // ── 7. Wait for shutdown signal ───────────────────────────────────────────
    info!("supply-drop-bbs ready — press Ctrl-C to stop");

    match tokio::signal::ctrl_c().await {
        Ok(()) => info!("Ctrl-C received — shutting down"),
        Err(e) => error!("error waiting for Ctrl-C: {e}"),
    }

    // ── 8. Stop plugins (reverse order) ──────────────────────────────────────
    #[cfg(feature = "admin-web")]
    if let Some(ref t) = web_plugin {
        use bbs_plugin_api::Plugin;
        if let Err(e) = t.stop().await {
            warn!("web plugin stop error: {e}");
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
