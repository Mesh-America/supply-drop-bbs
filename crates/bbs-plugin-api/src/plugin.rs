//! The `Plugin` trait and lifecycle.
//!
//! Every plugin in Supply Drop BBS implements `Plugin`. The base
//! trait covers the lifecycle (init, start, stop) and the static
//! metadata the supervisor needs to register the plugin.
//!
//! Capability traits ([`crate::TransportEngine`] and others) are
//! implemented in addition. A plugin that implements
//! `TransportEngine` is automatically a `Plugin` — the marker
//! lifecycle methods come from this trait.

use crate::error::PluginError;
use crate::host::Host;
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use std::sync::Arc;

/// The contract every Supply Drop BBS plugin compiles against.
///
/// ## Example skeleton
///
/// ```ignore
/// use bbs_plugin_api::{Plugin, Host, PluginError};
/// use async_trait::async_trait;
/// use serde::Deserialize;
/// use std::sync::Arc;
///
/// #[derive(Deserialize)]
/// pub struct MyConfig {
///     pub greeting: String,
/// }
///
/// pub struct MyPlugin {
///     config: MyConfig,
///     host: Arc<dyn Host>,
/// }
///
/// #[async_trait]
/// impl Plugin for MyPlugin {
///     type Config = MyConfig;
///
///     fn name(&self) -> &'static str { "my-plugin" }
///     fn version(&self) -> &'static str { env!("CARGO_PKG_VERSION") }
///
///     async fn init(config: Self::Config, host: Arc<dyn Host>)
///         -> Result<Self, PluginError>
///     {
///         Ok(Self { config, host })
///     }
///
///     async fn start(&self) -> Result<(), PluginError> { Ok(()) }
///     async fn stop(&self) -> Result<(), PluginError> { Ok(()) }
/// }
/// ```
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// The plugin's deserialised configuration type. The host
    /// reads `[plugins.<name>]` from the TOML config and
    /// deserialises it into this type at startup.
    type Config: DeserializeOwned + Send;

    /// Stable identifier. Lowercase ASCII, hyphens allowed, must
    /// be unique across all loaded plugins. Logged with every
    /// plugin event; used as the key for `[plugins.<name>]`
    /// config sections.
    fn name(&self) -> &'static str;

    /// Human-readable version. Reported via the health check and
    /// logged on startup. Conventionally `env!("CARGO_PKG_VERSION")`.
    fn version(&self) -> &'static str;

    /// Plugins this plugin requires to be loaded. Names must
    /// match other plugins' `name()`. The supervisor refuses to
    /// start if any declared dependency isn't loaded; cycles are
    /// also detected and rejected at startup.
    ///
    /// Most plugins return `&[]`. Plugins that declare
    /// dependencies should be conservative — every dependency
    /// reduces deployment flexibility.
    fn dependencies(&self) -> &'static [&'static str] {
        &[]
    }

    /// Construct the plugin from its config and a `Host` handle.
    /// Failure aborts startup.
    ///
    /// Long-running initialisation (DB migrations, cache warm,
    /// network handshakes) belongs here, **not** in `start`. The
    /// supervisor runs all `init`s in declared dependency order
    /// before any `start` runs.
    async fn init(config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError>
    where
        Self: Sized;

    /// Begin serving. Spawn worker tasks, open listeners.
    /// Returns when the plugin is ready to handle traffic. The
    /// supervisor doesn't announce service availability until all
    /// plugins' `start` methods have returned.
    async fn start(&self) -> Result<(), PluginError>;

    /// Cooperative shutdown. Drain in-flight work, close
    /// listeners. Must complete within the configured deadline
    /// (default 10s); the supervisor aborts the task and logs the
    /// breach if the deadline is exceeded.
    async fn stop(&self) -> Result<(), PluginError>;
}
