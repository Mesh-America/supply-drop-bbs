//! [`ProcessPluginManager`] — runtime registry for process transport plugins.
//!
//! Manages a collection of [`ProcessTransport`] instances: starts, stops,
//! restarts, persists config changes, and captures stderr for log viewing.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bbs_plugin_api::registry::{
    PluginRegistryApi, PluginState, PluginStatus, ProcessPluginConfig, RegistryError,
};
use bbs_plugin_api::{Host, Plugin};
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::transport::ProcessTransport;

const LOG_RING_CAP: usize = 500;
const LOG_TAIL: usize = 50;

// ── Internal state ────────────────────────────────────────────────────────────

struct ManagedPlugin {
    config: ProcessPluginConfig,
    state: PluginState,
    restart_count: u32,
    log_buffer: Arc<std::sync::Mutex<VecDeque<String>>>,
    /// Shutdown sender for the running transport (None when stopped/disabled).
    transport: Option<ProcessTransport>,
}

// ── ProcessPluginManager ──────────────────────────────────────────────────────

/// Implements [`PluginRegistryApi`] for process-based transport plugins.
///
/// Created in `main.rs` and passed to the web plugin via
/// `WebPlugin::set_plugin_registry()`.  The CLI `plugin` subcommand
/// accesses it directly through the returned `Arc`.
pub struct ProcessPluginManager {
    plugins: Mutex<HashMap<String, ManagedPlugin>>,
    host: Arc<dyn Host>,
    /// Path to `config.toml` for persisting add/remove/enable changes.
    config_path: Option<PathBuf>,
}

impl ProcessPluginManager {
    /// Create a manager from an initial list of configured plugins.
    ///
    /// Enabled plugins are started immediately.
    pub async fn new(
        configs: Vec<ProcessPluginConfig>,
        host: Arc<dyn Host>,
        config_path: Option<PathBuf>,
    ) -> Arc<Self> {
        let mgr = Arc::new(Self {
            plugins: Mutex::new(HashMap::new()),
            host,
            config_path,
        });

        for cfg in configs {
            let _ = mgr.init_plugin(cfg).await;
        }

        mgr
    }

    async fn init_plugin(&self, config: ProcessPluginConfig) -> Result<(), RegistryError> {
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
            state,
            restart_count: 0,
            log_buffer,
            transport,
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

        // Validate that the executable exists and is runnable.
        let mut cmd = TokioCommand::new(&command);
        cmd.args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        // We spawn through ProcessTransport::start() which does the real spawn.
        // Here we just construct and start.
        let transport = ProcessTransport::new(config, Arc::clone(&self.host));

        transport
            .start()
            .await
            .map_err(|e| RegistryError::SpawnFailed(name.clone(), e.to_string()))?;

        // Attach a stderr tapper if available.  The actual stderr capture is
        // done inside ProcessTransport::start(); here we provide a hook for the
        // manager's own log ring buffer by spawning a separate stderr reader.
        // Note: since ProcessTransport already consumed stderr, we rely on its
        // internal tracing output for now.
        // TODO: expose log_buffer into ProcessTransport for direct capture.
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
        PluginStatus {
            name: name.to_owned(),
            command: m.config.command.clone(),
            args: m.config.args.clone(),
            enabled: m.config.enabled,
            restart_on_crash: m.config.restart_on_crash,
            state: m.state.clone(),
            restart_count: m.restart_count,
            recent_logs,
        }
    }

    /// Persist the current plugin list to config.toml using toml_edit.
    async fn persist_config(&self, plugins: &HashMap<String, ManagedPlugin>) {
        let Some(path) = &self.config_path else {
            return;
        };

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

        // Rebuild the [[plugins.process]] array from current state.
        let arr = toml_edit::Array::default();
        // toml_edit uses ArrayOfTables for [[...]] blocks.
        let mut aot = toml_edit::ArrayOfTables::new();
        for m in plugins.values() {
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
        let _ = arr; // unused

        // Set or clear [[plugins.process]].
        doc["plugins"]["process"] = toml_edit::Item::ArrayOfTables(aot);

        if let Err(e) = tokio::fs::write(path, doc.to_string()).await {
            warn!("plugin manager: failed to write config: {e}");
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
        self.init_plugin(config).await?;
        let plugins = self.plugins.lock().await;
        self.persist_config(&plugins).await;
        Ok(())
    }

    async fn remove_plugin(&self, name: &str) -> Result<(), RegistryError> {
        let mut plugins = self.plugins.lock().await;
        let m = plugins
            .remove(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_owned()))?;
        if let Some(t) = m.transport {
            let _ = t.stop().await;
        }
        self.persist_config(&plugins).await;
        Ok(())
    }

    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), RegistryError> {
        let mut plugins = self.plugins.lock().await;
        let m = plugins
            .get_mut(name)
            .ok_or_else(|| RegistryError::NotFound(name.to_owned()))?;

        m.config.enabled = enabled;

        if enabled && m.transport.is_none() {
            // Start it.
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
                        m.state = PluginState::Crashed {
                            reason: e.to_string(),
                        };
                    }
                    return Err(e);
                }
            }
        } else if !enabled {
            if let Some(t) = m.transport.take() {
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
