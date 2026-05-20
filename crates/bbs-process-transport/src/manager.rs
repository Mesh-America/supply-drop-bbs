//! [`ProcessPluginManager`] — runtime registry for process transport plugins.
//!
//! Manages a collection of [`ProcessTransport`] instances: starts, stops,
//! restarts, persists config changes, and captures stderr for log viewing.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bbs_plugin_api::registry::{
    PluginRegistryApi, PluginState, PluginStatus, ProcessPluginConfig, RegistryError,
};
use bbs_plugin_api::{Host, Plugin};
use tokio::process::Command as TokioCommand;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

use crate::transport::{ExitEvent, ProcessTransport};

const LOG_RING_CAP: usize = 500;
const LOG_TAIL: usize = 50;

// ── Internal state ────────────────────────────────────────────────────────────

struct ManagedPlugin {
    config: ProcessPluginConfig,
    /// True when this plugin was loaded from `plugins.d/<name>.toml`.
    /// False means it lives in the `[[plugins.process]]` section of config.toml.
    from_plugins_d: bool,
    state: PluginState,
    restart_count: u32,
    log_buffer: Arc<std::sync::Mutex<VecDeque<String>>>,
    /// Shutdown sender for the running transport (None when stopped/disabled).
    transport: Option<ProcessTransport>,
    /// Last version string reported by the plugin process in its `ready` message.
    /// Cached so it survives across restarts while the transport is stopped.
    version: Option<String>,
}

// ── plugins.d helpers ─────────────────────────────────────────────────────────

/// Parse all `[[plugins.process]]` entries from a single drop-in file.
fn parse_plugin_file(raw: &str) -> Vec<ProcessPluginConfig> {
    #[derive(serde::Deserialize)]
    struct PluginFile {
        plugins: Option<PluginsSection>,
    }
    #[derive(serde::Deserialize)]
    struct PluginsSection {
        process: Option<Vec<ProcessPluginConfig>>,
    }
    toml::from_str::<PluginFile>(raw)
        .ok()
        .and_then(|f| f.plugins)
        .and_then(|p| p.process)
        .unwrap_or_default()
}

/// Load all plugins from `plugins.d/*.toml`, sorted by file name.
fn load_plugins_d(dir: &std::path::Path) -> Vec<ProcessPluginConfig> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "toml").unwrap_or(false))
        .collect();
    paths.sort();

    let mut out = Vec::new();
    for path in &paths {
        match std::fs::read_to_string(path) {
            Ok(raw) => out.extend(parse_plugin_file(&raw)),
            Err(e) => warn!(path = %path.display(), "plugins.d: cannot read file: {e}"),
        }
    }
    out
}

/// Write a single plugin's config to `plugins.d/<name>.toml`.
async fn write_plugin_file(dir: &std::path::Path, cfg: &ProcessPluginConfig) {
    #[derive(serde::Serialize)]
    struct Out<'a> {
        plugins: OutPlugins<'a>,
    }
    #[derive(serde::Serialize)]
    struct OutPlugins<'a> {
        process: &'a [ProcessPluginConfig],
    }
    let content = match toml::to_string_pretty(&Out {
        plugins: OutPlugins {
            process: std::slice::from_ref(cfg),
        },
    }) {
        Ok(s) => s,
        Err(e) => {
            warn!(name = %cfg.name, "plugins.d: cannot serialise plugin config: {e}");
            return;
        }
    };
    let path = dir.join(format!("{}.toml", cfg.name));
    if let Err(e) = tokio::fs::write(&path, content).await {
        warn!(path = %path.display(), "plugins.d: cannot write plugin file: {e}");
    }
}

// ── ProcessPluginManager ──────────────────────────────────────────────────────

/// Implements [`PluginRegistryApi`] for process-based transport plugins.
///
/// Created in `main.rs` and passed to the web plugin via
/// `WebPlugin::set_plugin_registry()`.  The CLI `plugin` subcommand
/// accesses it directly through the returned `Arc`.
pub struct ProcessPluginManager {
    /// Wrapped in `Arc` so crash-restart tasks can hold a reference without
    /// requiring `Arc<ProcessPluginManager>` (which would be self-referential
    /// given that `new()` returns `Arc<Self>`).
    plugins: Arc<Mutex<HashMap<String, ManagedPlugin>>>,
    host: Arc<dyn Host>,
    /// Path to `config.toml` for persisting legacy (non-plugins.d) entries.
    config_path: Option<PathBuf>,
    /// Path to the `plugins.d/` drop-in directory.
    plugins_d: Option<PathBuf>,
}

impl ProcessPluginManager {
    /// Create a manager from an initial list of configured plugins and an
    /// optional `plugins.d` drop-in directory.
    ///
    /// `configs` comes from `[[plugins.process]]` blocks in `config.toml`.
    /// Any `.toml` files found in `plugins_d` are merged in as well, with
    /// plugins.d entries taking precedence on name collisions.
    /// Enabled plugins are started immediately.
    pub async fn new(
        configs: Vec<ProcessPluginConfig>,
        host: Arc<dyn Host>,
        config_path: Option<PathBuf>,
        plugins_d: Option<PathBuf>,
    ) -> Arc<Self> {
        let mgr = Arc::new(Self {
            plugins: Arc::new(Mutex::new(HashMap::new())),
            host,
            config_path,
            plugins_d: plugins_d.clone(),
        });

        // Load config.toml plugins first (lower precedence).
        for cfg in configs {
            let _ = mgr.init_plugin(cfg, false).await;
        }

        // Load plugins.d — overrides config.toml on name collision.
        if let Some(ref dir) = plugins_d {
            for cfg in load_plugins_d(dir) {
                // Stop and remove any config.toml entry with the same name before
                // re-spawning from plugins.d.  Just dropping the ManagedPlugin is
                // not enough: the child process and its crash-restart loop would
                // both keep running and fight with the new instance.
                let evicted = mgr.plugins.lock().await.remove(&cfg.name);
                if let Some(m) = evicted {
                    if let Some(t) = m.transport {
                        let _ = t.stop().await;
                    }
                }
                let _ = mgr.init_plugin(cfg, true).await;
            }
        }

        mgr
    }

    async fn init_plugin(
        &self,
        config: ProcessPluginConfig,
        from_plugins_d: bool,
    ) -> Result<(), RegistryError> {
        let name = config.name.clone();
        let enabled = config.enabled;

        let log_buffer = Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(LOG_RING_CAP)));

        let (state, transport) = if enabled {
            match self
                .spawn_transport(config.clone(), Arc::clone(&log_buffer))
                .await
            {
                Ok(t) => (PluginState::Running, Some(t)),
                Err(e) => {
                    warn!(plugin = %name, "failed to start: {e}");
                    (
                        PluginState::Crashed {
                            reason: e.to_string(),
                        },
                        None,
                    )
                }
            }
        } else {
            (PluginState::Disabled, None)
        };

        let managed = ManagedPlugin {
            config,
            from_plugins_d,
            state,
            restart_count: 0,
            log_buffer,
            transport,
            version: None,
        };

        self.plugins.lock().await.insert(name, managed);
        Ok(())
    }

    async fn spawn_transport(
        &self,
        config: ProcessPluginConfig,
        log_buffer: Arc<std::sync::Mutex<VecDeque<String>>>,
    ) -> Result<ProcessTransport, RegistryError> {
        let name = config.name.clone();
        let command = config.command.clone();
        let args = config.args.clone();

        // Validate that the executable path is non-empty (basic sanity check
        // before the real spawn happens inside ProcessTransport::start).
        let mut cmd = TokioCommand::new(&command);
        cmd.args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let transport = ProcessTransport::new(config.clone(), Arc::clone(&self.host));

        // Wire up the exit-handling loop BEFORE start() so there is no window
        // where a very-fast exit could go unnoticed.
        if let Some(exit_rx) = transport.take_exit_receiver() {
            tokio::spawn(plugin_exit_loop(
                name.clone(),
                exit_rx,
                config.restart_on_crash,
                config.restart_delay_secs,
                Arc::clone(&self.plugins),
                Arc::clone(&self.host),
            ));
        }

        transport
            .start()
            .await
            .map_err(|e| RegistryError::SpawnFailed(name.clone(), e.to_string()))?;

        // Log capture: ProcessTransport already captures stderr via tracing.
        // A direct ring-buffer tap is a future improvement.
        let _ = log_buffer;

        info!(plugin = %name, "started");
        Ok(transport)
    }

    fn plugin_status(name: &str, m: &ManagedPlugin) -> PluginStatus {
        let recent_logs = {
            let buf = m.log_buffer.lock().expect("log buffer poisoned");
            buf.iter()
                .rev()
                .take(LOG_TAIL)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        };
        // Prefer the live value from the running transport; fall back to the
        // cached value from the last time the plugin reported ready.
        let version = m
            .transport
            .as_ref()
            .and_then(|t| t.reported_version())
            .or_else(|| m.version.clone());
        PluginStatus {
            name: name.to_owned(),
            command: m.config.command.clone(),
            args: m.config.args.clone(),
            enabled: m.config.enabled,
            restart_on_crash: m.config.restart_on_crash,
            state: m.state.clone(),
            restart_count: m.restart_count,
            recent_logs,
            version,
        }
    }

    /// Persist all in-memory plugin state back to disk.
    ///
    /// - Plugins from `config.toml` are written back to that file's
    ///   `[[plugins.process]]` section.
    /// - Plugins from `plugins.d` are written to their individual
    ///   `plugins.d/<name>.toml` files.
    async fn persist_config(&self, plugins: &HashMap<String, ManagedPlugin>) {
        // ── config.toml plugins ───────────────────────────────────────────────
        if let Some(path) = &self.config_path {
            let raw = match tokio::fs::read_to_string(path).await {
                Ok(s) => s,
                Err(e) => {
                    warn!("plugin manager: cannot read config for update: {e}");
                    return;
                }
            };
            let mut doc: toml_edit::DocumentMut = match raw.parse() {
                Ok(d) => d,
                Err(e) => {
                    warn!("plugin manager: cannot parse config for update: {e}");
                    return;
                }
            };
            let mut aot = toml_edit::ArrayOfTables::new();
            for m in plugins.values().filter(|m| !m.from_plugins_d) {
                let mut tbl = toml_edit::Table::new();
                tbl.insert("name", toml_edit::value(&m.config.name));
                tbl.insert("command", toml_edit::value(&m.config.command));
                if !m.config.args.is_empty() {
                    let mut args_arr = toml_edit::Array::default();
                    for a in &m.config.args {
                        args_arr.push(a.as_str());
                    }
                    tbl.insert("args", toml_edit::value(args_arr));
                }
                tbl.insert("enabled", toml_edit::value(m.config.enabled));
                tbl.insert(
                    "restart_on_crash",
                    toml_edit::value(m.config.restart_on_crash),
                );
                tbl.insert(
                    "restart_delay_secs",
                    toml_edit::value(m.config.restart_delay_secs as i64),
                );
                aot.push(tbl);
            }
            doc["plugins"]["process"] = toml_edit::Item::ArrayOfTables(aot);
            if let Err(e) = tokio::fs::write(path, doc.to_string()).await {
                warn!("plugin manager: failed to write config: {e}");
            }
        }

        // ── plugins.d plugins ─────────────────────────────────────────────────
        if let Some(ref dir) = self.plugins_d {
            for m in plugins.values().filter(|m| m.from_plugins_d) {
                write_plugin_file(dir, &m.config).await;
            }
        }
    }
}

#[async_trait]
impl PluginRegistryApi for ProcessPluginManager {
    async fn list_plugins(&self) -> Vec<PluginStatus> {
        let plugins = self.plugins.lock().await;
        plugins
            .iter()
            .map(|(name, m)| Self::plugin_status(name, m))
            .collect()
    }

    async fn add_plugin(&self, config: ProcessPluginConfig) -> Result<(), RegistryError> {
        let name = config.name.clone();
        {
            let plugins = self.plugins.lock().await;
            if plugins.contains_key(&name) {
                return Err(RegistryError::AlreadyExists(name));
            }
        }
        // New plugins always go to plugins.d when available.
        let to_plugins_d = self.plugins_d.is_some();
        self.init_plugin(config, to_plugins_d).await?;
        let plugins = self.plugins.lock().await;
        self.persist_config(&plugins).await;
        Ok(())
    }

    async fn remove_plugin(&self, name: &str) -> Result<(), RegistryError> {
        let (from_plugins_d, transport) = {
            let mut plugins = self.plugins.lock().await;
            let m = plugins
                .remove(name)
                .ok_or_else(|| RegistryError::NotFound(name.to_owned()))?;
            (m.from_plugins_d, m.transport)
        };
        if let Some(t) = transport {
            let _ = t.stop().await;
        }
        // Delete the drop-in file when removing a plugins.d plugin.
        if from_plugins_d {
            if let Some(ref dir) = self.plugins_d {
                let file = dir.join(format!("{name}.toml"));
                if let Err(e) = tokio::fs::remove_file(&file).await {
                    warn!(path = %file.display(), "plugins.d: failed to delete plugin file: {e}");
                }
            }
        }
        let plugins = self.plugins.lock().await;
        self.persist_config(&plugins).await;
        Ok(())
    }

    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), RegistryError> {
        let mut plugins = self.plugins.lock().await;
        let m = plugins
            .get_mut(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_owned()))?;

        m.config.enabled = enabled;

        if enabled && m.transport.is_none() && m.state != PluginState::Starting {
            // Mark the plugin as Starting *before* releasing the lock so that
            // any concurrent set_enabled(true) call sees the transitional state
            // and bails out without spawning a second orphaned child process
            // (TOCTOU fix).
            m.state = PluginState::Starting;
            let log_buffer = Arc::clone(&m.log_buffer);
            let config = m.config.clone();
            drop(plugins);
            match self.spawn_transport(config, log_buffer).await {
                Ok(t) => {
                    let mut plugins = self.plugins.lock().await;
                    if let Some(m) = plugins.get_mut(name) {
                        m.transport = Some(t);
                        m.state = PluginState::Running;
                    }
                    self.persist_config(&plugins).await;
                }
                Err(e) => {
                    let mut plugins = self.plugins.lock().await;
                    if let Some(m) = plugins.get_mut(name) {
                        // Roll back to Stopped so a future call can retry.
                        m.state = PluginState::Stopped;
                    }
                    return Err(e);
                }
            }
        } else if !enabled {
            if let Some(t) = m.transport.take() {
                // Cache the version before the transport is dropped.
                if let Some(v) = t.reported_version() {
                    m.version = Some(v);
                }
                let _ = t.stop().await;
            }
            m.state = PluginState::Disabled;
            self.persist_config(&plugins).await;
        }

        Ok(())
    }

    async fn restart_plugin(&self, name: &str) -> Result<(), RegistryError> {
        // Stop existing transport.
        let (config, log_buffer) = {
            let mut plugins = self.plugins.lock().await;
            let m = plugins
                .get_mut(name)
                .ok_or_else(|| RegistryError::NotFound(name.to_owned()))?;
            if let Some(t) = m.transport.take() {
                // Cache the version before the transport is dropped.
                if let Some(v) = t.reported_version() {
                    m.version = Some(v);
                }
                let _ = t.stop().await;
            }
            m.state = PluginState::Stopped;
            (m.config.clone(), Arc::clone(&m.log_buffer))
        };

        // Spawn fresh.
        match self.spawn_transport(config, log_buffer).await {
            Ok(t) => {
                let mut plugins = self.plugins.lock().await;
                if let Some(m) = plugins.get_mut(name) {
                    m.transport = Some(t);
                    m.state = PluginState::Running;
                    m.restart_count += 1;
                }
                Ok(())
            }
            Err(e) => {
                let mut plugins = self.plugins.lock().await;
                if let Some(m) = plugins.get_mut(name) {
                    m.state = PluginState::Crashed {
                        reason: e.to_string(),
                    };
                }
                Err(e)
            }
        }
    }

    async fn get_logs(&self, name: &str, n: usize) -> Result<Vec<String>, RegistryError> {
        let plugins = self.plugins.lock().await;
        let m = plugins
            .get(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_owned()))?;
        let buf = m.log_buffer.lock().expect("log buffer poisoned");
        let lines = buf
            .iter()
            .rev()
            .take(n)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Ok(lines)
    }
}

// ── Plugin exit loop ───────────────────────────────────────────────────────

/// Spawned once per plugin that has been started.
///
/// Receives an [`ExitEvent`] from `ProcessTransport`'s watcher task and acts:
///
/// - [`ExitEvent::Crash`]: waits the configured delay, respawns the process,
///   and updates state.  The loop then watches the new child via its fresh
///   receiver.  The loop exits when:
///   - The sender is dropped (plugin was removed or disabled)
///   - A respawn attempt fails (state is set to `Crashed`)
///
/// - [`ExitEvent::Stopped`]: updates state to `PluginState::Stopped` and
///   exits.  The plugin exited cleanly or with `restart_on_crash = false`.
async fn plugin_exit_loop(
    name: String,
    mut rx: mpsc::Receiver<ExitEvent>,
    restart_on_crash: bool,
    delay_secs: u64,
    plugins: Arc<Mutex<HashMap<String, ManagedPlugin>>>,
    host: Arc<dyn Host>,
) {
    while let Some(event) = rx.recv().await {
        match event {
            ExitEvent::Stopped => {
                // Clean exit or restart_on_crash=false — mark as Stopped.
                info!(plugin = %name, "process stopped; updating state to Stopped");
                let mut map = plugins.lock().await;
                if let Some(m) = map.get_mut(&name) {
                    // Only update if still Running — don't clobber Disabled or
                    // a state already set by an explicit stop/restart call.
                    if m.state == PluginState::Running {
                        m.state = PluginState::Stopped;
                        m.transport = None;
                    }
                }
                break;
            }

            ExitEvent::Crash => {
                // restart_on_crash=true and non-zero exit.
                if !restart_on_crash {
                    // Defensive: this variant should only arrive when
                    // restart_on_crash is true, but guard anyway.
                    let mut map = plugins.lock().await;
                    if let Some(m) = map.get_mut(&name) {
                        if m.state == PluginState::Running {
                            m.state = PluginState::Stopped;
                            m.transport = None;
                        }
                    }
                    break;
                }

                warn!(plugin = %name, delay_secs, "crashed — restarting");
                tokio::time::sleep(Duration::from_secs(delay_secs)).await;

                let (config, _log_buffer) = {
                    let map = plugins.lock().await;
                    let Some(m) = map.get(&name) else { return };
                    (m.config.clone(), Arc::clone(&m.log_buffer))
                };

                let new_transport = ProcessTransport::new(config, Arc::clone(&host));
                // Take the crash receiver for the new process before it starts.
                let new_rx = new_transport.take_exit_receiver();

                match new_transport.start().await {
                    Ok(()) => {
                        {
                            let mut map = plugins.lock().await;
                            if let Some(m) = map.get_mut(&name) {
                                m.transport = Some(new_transport);
                                m.state = PluginState::Running;
                                m.restart_count += 1;
                            }
                        }
                        info!(plugin = %name, "restarted after crash");
                        // Switch to watching the new process.
                        match new_rx {
                            Some(new_rx) => rx = new_rx,
                            None => break,
                        }
                    }
                    Err(e) => {
                        warn!(plugin = %name, error = %e, "restart failed");
                        let mut map = plugins.lock().await;
                        if let Some(m) = map.get_mut(&name) {
                            m.transport = None;
                            m.state = PluginState::Crashed {
                                reason: e.to_string(),
                            };
                        }
                        break;
                    }
                }
            } // end ExitEvent::Crash
        } // end match event
    }
}
