//! Supply Drop BBS — entry point.
//!
//! Parses the command line, loads config, and dispatches to the
//! appropriate subcommand.  The host supervisor (the component that
//! wires the database to plugins and runs the event loop) is wired in
//! a subsequent commit once the config loader is stable.
//!
//! Architecture: see `docs/ARCHITECTURE.md`.

#![allow(missing_docs)]

mod config;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

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
    #[arg(long, global = true, value_name = "PATH", env = "SUPPLY_DROP__BBS__DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Override the log level (TRACE/DEBUG/INFO/WARN/ERROR).
    ///
    /// When this flag is used the effective level is announced in the
    /// first log line (ADR-0009: no silent stomps).
    #[arg(long, global = true, value_name = "LEVEL", env = "SUPPLY_DROP__LOGGING__LEVEL")]
    log_level: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the BBS (default when no subcommand is given). [TBD]
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

    match cli.command {
        None | Some(Commands::Run) => cmd_run(&cli),
        Some(Commands::Setup) => cmd_setup(),
        Some(Commands::Config { action }) => cmd_config(cli.config.as_deref(), action),
        Some(Commands::Migrate) => cmd_migrate(&cli),
        Some(Commands::Backup) => cmd_backup(&cli),
    }
}

// ── Subcommand handlers ───────────────────────────────────────────────────────

fn cmd_run(_cli: &Cli) {
    // TODO: initialise tracing, open database, construct Host, spin up plugins.
    eprintln!(
        "error: the supervisor is not yet implemented.\n\
         Run `supply-drop-bbs config check` to validate your config,\n\
         or `supply-drop-bbs config show` to inspect effective values."
    );
    std::process::exit(1);
}

fn cmd_setup() {
    // TODO: interactive setup wizard.
    eprintln!("error: setup wizard not yet implemented.");
    std::process::exit(1);
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
            // Config loaded and resolved successfully.
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
