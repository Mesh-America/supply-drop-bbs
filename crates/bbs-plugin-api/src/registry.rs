//! Runtime registry API for externally-spawned transport plugins.
//!
//! Defines the types and [`PluginRegistryApi`] trait that let the web admin
//! and CLI manage process-based transport plugins at runtime — without a full
//! BBS restart.
//!
//! The concrete implementation lives in `bbs-process-transport`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Config ────────────────────────────────────────────────────────────────────

/// Configuration for a single externally-spawned transport plugin.
///
/// Each entry maps to one `[[plugins.process]]` block in `config.toml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProcessPluginConfig {
    /// Unique stable identifier. Used in API paths, log prefixes, and as
    /// the transport name in the session registry. Must be unique across all
    /// process plugins.
    pub name: String,

    /// Path to the executable to spawn.
    pub command: String,

    /// Arguments to pass to the executable.
    #[serde(default)]
    pub args: Vec<String>,

    /// Whether to start this plugin at BBS startup.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Restart the plugin process if it exits unexpectedly.
    #[serde(default = "default_true")]
    pub restart_on_crash: bool,

    /// Seconds to wait between crash restarts (exponential backoff up to 60s).
    #[serde(default = "default_restart_delay")]
    pub restart_delay_secs: u64,
}

fn default_true() -> bool {
    true
}
fn default_restart_delay() -> u64 {
    5
}

// ── Status ────────────────────────────────────────────────────────────────────

/// Runtime state of a managed process plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PluginState {
    /// The process is running and accepting connections.
    Running,
    /// Configured but not started (not yet launched or cleanly stopped).
    Stopped,
    /// The process exited unexpectedly.
    Crashed {
        /// Human-readable exit reason (exit code, signal, etc.).
        reason: String,
    },
    /// Disabled in config — will not be started automatically.
    Disabled,
}

/// Runtime status of a managed plugin, as returned by the registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginStatus {
    /// Plugin name from config.
    pub name: String,
    /// Executable path.
    pub command: String,
    /// CLI args.
    pub args: Vec<String>,
    /// Whether enabled in config.
    pub enabled: bool,
    /// Whether to restart on crash.
    pub restart_on_crash: bool,
    /// Current runtime state.
    #[serde(flatten)]
    pub state: PluginState,
    /// Times the process has restarted since BBS start.
    pub restart_count: u32,
    /// Most recent stderr lines (up to the last 50).
    pub recent_logs: Vec<String>,
    /// Version string reported by the plugin in its `ready` message, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

// ── Errors ────────────────────────────────────────────────────────────────────

/// Errors returned by [`PluginRegistryApi`] operations.
#[derive(Debug, Error)]
pub enum RegistryError {
    /// No plugin with the given name is configured.
    #[error("plugin '{0}' not found")]
    NotFound(String),

    /// A plugin with the given name already exists.
    #[error("plugin '{0}' already exists")]
    AlreadyExists(String),

    /// The operation requires the plugin to be running, but it is not.
    #[error("plugin '{0}' is not running")]
    NotRunning(String),

    /// The plugin process could not be started.
    #[error("failed to spawn plugin '{0}': {1}")]
    SpawnFailed(String, String),

    /// Config file could not be written (IO or TOML error).
    #[error("config write failed: {0}")]
    ConfigWrite(String),

    /// Catch-all for unexpected errors.
    #[error("{0}")]
    Other(String),
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Runtime management API for externally-spawned transport plugins.
///
/// The `ProcessPluginManager` in `bbs-process-transport` implements this.
/// The web admin and CLI use it through `Arc<dyn PluginRegistryApi>`.
#[async_trait]
pub trait PluginRegistryApi: Send + Sync + 'static {
    /// List all configured plugins and their current runtime status.
    async fn list_plugins(&self) -> Vec<PluginStatus>;

    /// Add a new plugin, start it immediately (if enabled), and persist to config.
    async fn add_plugin(&self, config: ProcessPluginConfig) -> Result<(), RegistryError>;

    /// Remove a plugin, stop it if running, and remove from config.
    async fn remove_plugin(&self, name: &str) -> Result<(), RegistryError>;

    /// Enable or disable a plugin and persist to config.
    ///
    /// Enabling a stopped plugin starts it immediately.
    /// Disabling a running plugin stops it.
    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), RegistryError>;

    /// Restart a plugin (stop then re-spawn the process).
    async fn restart_plugin(&self, name: &str) -> Result<(), RegistryError>;

    /// Return the last `n` lines from the plugin's stderr capture.
    async fn get_logs(&self, name: &str, n: usize) -> Result<Vec<String>, RegistryError>;
}
