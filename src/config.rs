//! Configuration loader for Supply Drop BBS.
//!
//! Implements the three-source overlay described in
//! [ADR-0008](../docs/adr/0008-toml-config-with-env-overrides.md):
//!
//! ```text
//! compiled-in defaults  (lowest priority)
//!        ↓
//! TOML config file
//!        ↓
//! environment variables (highest priority)
//! ```
//!
//! CLI flag overrides (`--log-level`, `--data-dir`) are applied by
//! `main` after [`load`] returns.
//!
//! # Usage
//!
//! ```no_run
//! use std::path::Path;
//!
//! let cfg = supply_drop_bbs::config::load(None).expect("config error");
//! println!("{}", cfg.bbs.name);
//! ```

#![allow(missing_docs)]

use std::{collections::HashMap, path::PathBuf};

use figment::{
    providers::{Env, Format, Serialized, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    FileNotFound(PathBuf),
    #[error("{0}")]
    Figment(#[from] figment::Error),
    #[error("invalid log level {0:?} — expected TRACE/DEBUG/INFO/WARN/ERROR")]
    BadLogLevel(String),
}

// ── Top-level Config ──────────────────────────────────────────────────────────

/// The complete configuration for a Supply Drop BBS instance.
///
/// Loaded via [`load`]. All fields have defaults; an empty config file
/// (or no file at all) is valid and produces a working configuration
/// with sensible values.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub bbs: BbsConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub backup: BackupConfig,
    #[serde(default)]
    pub location: LocationConfig,
    #[serde(default)]
    pub plugins: PluginsConfig,
}

impl Config {
    /// Fill in derived defaults that depend on `data_dir`.
    ///
    /// Called automatically by [`load`] after deserialization.
    /// If `database.path`, `logging.file`, or `backup.directory` are
    /// absent they are set to sub-paths of `bbs.data_dir`.
    pub fn resolve(mut self) -> Self {
        let data_dir = self
            .bbs
            .data_dir
            .get_or_insert_with(default_data_dir)
            .clone();

        if self.database.path.is_none() {
            self.database.path = Some(data_dir.join("bbs.sqlite"));
        }
        if self.logging.file.is_none() {
            self.logging.file = Some(data_dir.join("log/bbs.log"));
        }
        if self.backup.directory.is_none() {
            self.backup.directory = Some(data_dir.join("backups"));
        }
        // Resolve CLI socket path the same way.
        #[cfg(feature = "transport-cli")]
        if self.plugins.cli.socket.is_none() {
            self.plugins.cli.socket = Some(data_dir.join("cli.sock"));
        }

        self
    }
}

// ── [bbs] ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BbsConfig {
    /// Display name shown to users on connect.
    #[serde(default = "default_bbs_name")]
    pub name: String,

    /// Root directory for all BBS data (DB, logs, backups, socket).
    ///
    /// `None` here is resolved in [`Config::resolve`] to a
    /// platform-appropriate default.
    #[serde(default)]
    pub data_dir: Option<PathBuf>,

    /// Room a newly logged-in user lands in.
    #[serde(default = "default_starting_room")]
    pub starting_room: String,

    /// Banner shown on connect. `{name}` expands to `bbs.name`.
    #[serde(default = "default_welcome_msg")]
    pub welcome_msg: String,

    /// IANA timezone name for display timestamps. Storage is always UTC.
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

impl Default for BbsConfig {
    fn default() -> Self {
        Self {
            name: default_bbs_name(),
            data_dir: None,
            starting_room: default_starting_room(),
            welcome_msg: default_welcome_msg(),
            timezone: default_timezone(),
        }
    }
}

fn default_bbs_name() -> String {
    "Supply Drop BBS".to_owned()
}
fn default_starting_room() -> String {
    "Lobby".to_owned()
}
fn default_welcome_msg() -> String {
    "Welcome to {name}.".to_owned()
}
fn default_timezone() -> String {
    "UTC".to_owned()
}

// ── [location] ───────────────────────────────────────────────────────────────

/// GPS coordinates for this BBS node.
///
/// When set, the mesh transport sends `SetAdvertLatlon` to the radio on
/// connect so the node's location is broadcast in LoRa adverts.
/// Both fields must be present for the location to be applied; a partial
/// entry (only lat or only lon) is silently ignored.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocationConfig {
    /// Latitude in decimal degrees (e.g. `37.7749`).
    #[serde(default)]
    pub latitude: Option<f64>,

    /// Longitude in decimal degrees (e.g. `-122.4194`).
    #[serde(default)]
    pub longitude: Option<f64>,
}

impl LocationConfig {
    /// Return the coordinate pair if both fields are set, otherwise `None`.
    pub fn as_coords(&self) -> Option<(f64, f64)> {
        match (self.latitude, self.longitude) {
            (Some(lat), Some(lon)) => Some((lat, lon)),
            _ => None,
        }
    }
}

// ── [database] ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// SQLite file path. `None` is resolved to `<data_dir>/bbs.sqlite`.
    #[serde(default)]
    pub path: Option<PathBuf>,

    /// Read-only connection pool size. `None` = auto (`cpu_count + 2`).
    #[serde(default)]
    pub read_pool_size: Option<u32>,

    /// SQLite `busy_timeout` in milliseconds.
    #[serde(default = "default_busy_timeout_ms")]
    pub busy_timeout_ms: u64,

    /// SQLite `synchronous` pragma.
    #[serde(default)]
    pub synchronous: SynchronousMode,

    /// WAL pages between automatic checkpoints.
    #[serde(default = "default_wal_autocheckpoint")]
    pub wal_autocheckpoint: u32,

    /// Maximum WAL file size in bytes.
    #[serde(default = "default_journal_size_limit_bytes")]
    pub journal_size_limit_bytes: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: None,
            read_pool_size: None,
            busy_timeout_ms: default_busy_timeout_ms(),
            synchronous: SynchronousMode::default(),
            wal_autocheckpoint: default_wal_autocheckpoint(),
            journal_size_limit_bytes: default_journal_size_limit_bytes(),
        }
    }
}

fn default_busy_timeout_ms() -> u64 {
    5_000
}
fn default_wal_autocheckpoint() -> u32 {
    10_000
}
fn default_journal_size_limit_bytes() -> u64 {
    64 * 1024 * 1024 // 64 MB
}

/// SQLite `synchronous` pragma value.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SynchronousMode {
    #[default]
    Normal,
    Full,
    Off,
}

impl SynchronousMode {
    /// The string SQLite expects in the PRAGMA statement.
    #[allow(dead_code)]
    pub fn as_pragma_str(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Full => "FULL",
            Self::Off => "OFF",
        }
    }
}

// ── [logging] ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// Root log level.
    #[serde(default)]
    pub level: LogLevel,

    /// Log file path. `None` is resolved to `<data_dir>/log/bbs.log`.
    #[serde(default)]
    pub file: Option<PathBuf>,

    /// Rotation size per file in bytes.
    #[serde(default = "default_log_max_bytes")]
    pub max_bytes: u64,

    /// Number of rotated log files to retain.
    #[serde(default = "default_log_backup_count")]
    pub backup_count: u32,

    /// Log output format.
    #[serde(default)]
    pub format: LogFormat,

    /// Per-`tracing` target level overrides.
    ///
    /// Example in TOML:
    /// ```toml
    /// [logging.targets]
    /// "supply_drop_bbs::transport::mesh" = "DEBUG"
    /// "sqlx::query" = "WARN"
    /// ```
    #[serde(default)]
    pub targets: HashMap<String, LogLevel>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::default(),
            file: None,
            max_bytes: default_log_max_bytes(),
            backup_count: default_log_backup_count(),
            format: LogFormat::default(),
            targets: HashMap::new(),
        }
    }
}

fn default_log_max_bytes() -> u64 {
    10 * 1024 * 1024 // 10 MB
}
fn default_log_backup_count() -> u32 {
    5
}

/// Log verbosity level.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl From<LogLevel> for tracing::Level {
    fn from(l: LogLevel) -> Self {
        match l {
            LogLevel::Trace => tracing::Level::TRACE,
            LogLevel::Debug => tracing::Level::DEBUG,
            LogLevel::Info => tracing::Level::INFO,
            LogLevel::Warn => tracing::Level::WARN,
            LogLevel::Error => tracing::Level::ERROR,
        }
    }
}

impl std::str::FromStr for LogLevel {
    type Err = ConfigError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_uppercase().as_str() {
            "TRACE" => Ok(Self::Trace),
            "DEBUG" => Ok(Self::Debug),
            "INFO" => Ok(Self::Info),
            "WARN" | "WARNING" => Ok(Self::Warn),
            "ERROR" => Ok(Self::Error),
            _ => Err(ConfigError::BadLogLevel(s.to_owned())),
        }
    }
}

/// Log output format.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable, single-line per event.
    #[default]
    Compact,
    /// Human-readable with full context (spans, fields).
    Pretty,
    /// Structured JSON for log aggregators.
    Json,
}

// ── [security] ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    /// Argon2id memory cost in KiB (~19 MB; ~250ms on Pi 4).
    #[serde(default = "default_argon2_memory_kib")]
    pub argon2_memory_kib: u32,

    /// Argon2id time cost (iterations).
    #[serde(default = "default_argon2_iterations")]
    pub argon2_iterations: u32,

    /// Argon2id parallelism degree.
    #[serde(default = "default_argon2_parallelism")]
    pub argon2_parallelism: u32,

    /// Web session lifetime in seconds (default 12 hours).
    #[serde(default = "default_session_lifetime_web_secs")]
    pub session_lifetime_web_secs: u64,

    /// Mesh session lifetime in seconds (default 3 days).
    ///
    /// Mesh sessions persist longer because radio users disconnect
    /// frequently and re-auth over radio is expensive.
    #[serde(default = "default_session_lifetime_mesh_secs")]
    pub session_lifetime_mesh_secs: u64,

    /// Maximum failed login attempts per minute per source.
    #[serde(default = "default_login_rate_per_min")]
    pub login_rate_per_min: u32,

    /// Maximum commands per minute per session.
    #[serde(default = "default_command_rate_per_min")]
    pub command_rate_per_min: u32,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            argon2_memory_kib: default_argon2_memory_kib(),
            argon2_iterations: default_argon2_iterations(),
            argon2_parallelism: default_argon2_parallelism(),
            session_lifetime_web_secs: default_session_lifetime_web_secs(),
            session_lifetime_mesh_secs: default_session_lifetime_mesh_secs(),
            login_rate_per_min: default_login_rate_per_min(),
            command_rate_per_min: default_command_rate_per_min(),
        }
    }
}

fn default_argon2_memory_kib() -> u32 {
    19_456
}
fn default_argon2_iterations() -> u32 {
    2
}
fn default_argon2_parallelism() -> u32 {
    1
}
fn default_session_lifetime_web_secs() -> u64 {
    12 * 60 * 60 // 12 hours
}
fn default_session_lifetime_mesh_secs() -> u64 {
    3 * 24 * 60 * 60 // 3 days
}
fn default_login_rate_per_min() -> u32 {
    5
}
fn default_command_rate_per_min() -> u32 {
    60
}

// ── [backup] ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackupConfig {
    /// Whether to run automatic periodic backups.
    #[serde(default = "default_backup_enabled")]
    pub enabled: bool,

    /// Hours between automatic backups.
    #[serde(default = "default_backup_interval_hours")]
    pub interval_hours: u32,

    /// Backup directory. `None` is resolved to `<data_dir>/backups`.
    #[serde(default)]
    pub directory: Option<PathBuf>,

    /// Daily backups to retain.
    #[serde(default = "default_keep_daily")]
    pub keep_daily: u32,

    /// Weekly backups to retain.
    #[serde(default = "default_keep_weekly")]
    pub keep_weekly: u32,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            enabled: default_backup_enabled(),
            interval_hours: default_backup_interval_hours(),
            directory: None,
            keep_daily: default_keep_daily(),
            keep_weekly: default_keep_weekly(),
        }
    }
}

fn default_backup_enabled() -> bool {
    true
}
fn default_backup_interval_hours() -> u32 {
    6
}
fn default_keep_daily() -> u32 {
    7
}
fn default_keep_weekly() -> u32 {
    4
}

// ── [plugins] ─────────────────────────────────────────────────────────────────

/// Configuration for all compiled-in plugins.
///
/// Sections for plugins not compiled in are silently ignored
/// (no `deny_unknown_fields` here, by design).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PluginsConfig {
    #[cfg(feature = "transport-cli")]
    #[serde(default)]
    pub cli: bbs_cli::CliConfig,

    #[cfg(feature = "transport-mesh")]
    #[serde(default)]
    pub mesh: bbs_mesh::MeshConfig,

    #[cfg(feature = "transport-meshtastic")]
    #[serde(default)]
    pub meshtastic: bbs_meshtastic::MeshtasticConfig,

    #[cfg(feature = "admin-web")]
    #[serde(default)]
    pub web: bbs_web::WebConfig,

    /// Externally-spawned transport plugins (`[[plugins.process]]` blocks).
    #[cfg(feature = "transport-process")]
    #[serde(default)]
    pub process: Vec<bbs_plugin_api::ProcessPluginConfig>,
}

// ── plugins.d ─────────────────────────────────────────────────────────────────

/// Returns the `plugins.d` drop-in directory for the given config file.
///
/// Plugin installers drop `<name>.toml` files here instead of editing
/// `config.toml` directly. Supply Drop merges them at startup; the files
/// survive BBS upgrades and reconfiguration.
pub fn plugins_d_dir(config_path: &std::path::Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("plugins.d")
}

// ── File resolution ───────────────────────────────────────────────────────────

/// Locate the config file to use.
///
/// Priority:
/// 1. `explicit_path` (from `--config` CLI flag) — errors if not found
/// 2. `SUPPLY_DROP_CONFIG` env var
/// 3. `./config.toml`
/// 4. `/etc/supply-drop-bbs/config.toml`
/// 5. `~/.config/supply-drop-bbs/config.toml`
///
/// Returns `(path, was_explicit)`. `was_explicit = true` means the
/// caller specified the path; a missing explicit path is a hard error.
/// Return the config file path that would be used by `load()`.
///
/// Returns `None` when no config file can be located.
#[cfg(feature = "transport-process")]
pub fn resolve_config_path(explicit_path: Option<&std::path::Path>) -> Option<PathBuf> {
    resolve_file(explicit_path).0
}

fn resolve_file(explicit_path: Option<&std::path::Path>) -> (Option<PathBuf>, bool) {
    if let Some(p) = explicit_path {
        return (Some(p.to_owned()), true);
    }
    if let Ok(p) = std::env::var("SUPPLY_DROP_CONFIG") {
        if !p.is_empty() {
            return (Some(PathBuf::from(p)), false);
        }
    }
    let candidates = [
        PathBuf::from("config.toml"),
        PathBuf::from("/etc/supply-drop-bbs/config.toml"),
        dirs::config_dir()
            .map(|d| d.join("supply-drop-bbs/config.toml"))
            .unwrap_or_default(),
    ];
    for c in &candidates {
        if c.exists() {
            return (Some(c.clone()), false);
        }
    }
    (None, false)
}

/// Default `data_dir` when not explicitly configured.
///
/// Uses the XDG data home on Linux (`~/.local/share/supply-drop-bbs`).
/// System installs set `data_dir` explicitly via config or the setup wizard.
fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .map(|d| d.join("supply-drop-bbs"))
        .unwrap_or_else(|| PathBuf::from("/var/lib/supply-drop-bbs"))
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Load and resolve the effective configuration.
///
/// Sources are merged in priority order (lowest → highest):
/// 1. Compiled-in defaults
/// 2. TOML config file (see [`resolve_file`] for search order)
/// 3. `SUPPLY_DROP__*` environment variables
///
/// Returns an error if:
/// - An explicit config path was given but the file does not exist
/// - The TOML is malformed or contains type errors
/// - Any required field (those without defaults) is missing
#[allow(clippy::result_large_err)] // figment::Error is large; boxing it would add noise
pub fn load(explicit_path: Option<&std::path::Path>) -> Result<Config, ConfigError> {
    let (file_path, was_explicit) = resolve_file(explicit_path);

    if was_explicit {
        if let Some(ref p) = file_path {
            if !p.exists() {
                return Err(ConfigError::FileNotFound(p.clone()));
            }
        }
    }

    let mut figment = Figment::new().merge(Serialized::defaults(Config::default()));

    if let Some(ref path) = file_path {
        // `Toml::file` is lenient — silently no-ops if the file is missing.
        // We already checked existence for explicit paths above, so this
        // handles the case of a candidate path that disappeared between the
        // check and the open (acceptable; we'll just run on defaults).
        figment = figment.merge(Toml::file(path));
    }

    // Env vars: SUPPLY_DROP__SECTION__KEY=value
    // Double-underscore separates hierarchy levels.
    figment = figment.merge(Env::prefixed("SUPPLY_DROP__").split("__"));

    Ok(figment.extract::<Config>()?.resolve())
}
