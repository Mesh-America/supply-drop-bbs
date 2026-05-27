//! # bbs-web
//!
//! The web admin UI plugin for Supply Drop BBS. Serves an Axum HTTP server
//! with a Vue 3 SPA embedded via `rust-embed` and a JSON REST API.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │  WebPlugin (Plugin impl)                                │
//! │  ┌─────────────────────────────────────────────────┐    │
//! │  │  Axum router                                    │    │
//! │  │  POST /api/v1/auth/login                        │    │
//! │  │  GET  /api/v1/auth/whoami          (auth)       │    │
//! │  │  POST /api/v1/auth/logout          (auth)       │    │
//! │  │  GET  /api/v1/status               (auth)       │    │
//! │  │  GET  /api/v1/transports           (auth)       │    │
//! │  │  GET  /api/v1/native-plugins       (auth)       │    │
//! │  │  PATCH /api/v1/native-plugins/:name (auth)      │    │
//! │  │  GET  /api/v1/adverts              (auth)       │    │
//! │  │  POST /api/v1/adverts/send         (auth)       │    │
//! │  │  GET  /api/v1/sessions             (auth)       │    │
//! │  │  GET  /api/v1/users                (auth)       │    │
//! │  │  PATCH /api/v1/users/:username     (auth)       │    │
//! │  │  GET  /api/v1/rooms                (auth)       │    │
//! │  │  POST /api/v1/rooms                (auth)       │    │
//! │  │  DELETE /api/v1/rooms/:id          (auth)       │    │
//! │  │  GET  /api/v1/rooms/:id/messages   (auth)       │    │
//! │  │  DELETE /api/v1/messages/:id       (auth)       │    │
//! │  │  GET  /api/v1/audit-log            (auth)       │    │
//! │  │  GET  /api/v1/stats                (auth)       │    │
//! │  │  GET  /api/v1/settings             (auth)       │    │
//! │  │  GET  /api/v1/config               (auth)       │    │
//! │  │  PATCH /api/v1/config              (auth)       │    │
//! │  │  GET  /api/v1/access-policy        (auth)       │    │
//! │  │  PATCH /api/v1/access-policy       (auth)       │    │
//! │  │  GET  /api/v1/radio-config         (auth)       │    │
//! │  │  PATCH /api/v1/radio-config        (auth)       │    │
//! │  │  GET  /api/v1/errors               (auth)       │    │
//! │  │  GET  /api/v1/metrics              (auth)       │    │
//! │  │  GET  /api/v1/sse/logs             (auth)       │    │
//! │  │  GET  /api/v1/sse/errors           (auth)       │    │
//! │  │  GET  /api/v1/sse/rss-alert        (auth)       │    │
//! │  │  POST /api/v1/backups              (auth)       │    │
//! │  │  GET  /api/v1/backups              (auth)       │    │
//! │  │  GET  /api/v1/backups/:filename    (auth)       │    │
//! │  │  DELETE /api/v1/backups/:filename  (auth)       │    │
//! │  │  GET  /api/v1/plugins              (auth)       │    │
//! │  │  POST /api/v1/plugins              (auth)       │    │
//! │  │  DELETE /api/v1/plugins/:name      (auth)       │    │
//! │  │  PATCH /api/v1/plugins/:name       (auth)       │    │
//! │  │  POST /api/v1/plugins/:name/restart (auth)      │    │
//! │  │  GET  /api/v1/plugins/:name/logs   (auth)       │    │
//! │  │  GET  /*               → rust-embed SPA         │    │
//! │  └─────────────────────────────────────────────────┘    │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Auth
//!
//! BBS users with Aide+ permission (level ≥ 50) can log in to the web admin.
//! Sessions are in-memory UUIDs stored in an HttpOnly cookie.
//!
//! [ADR-0003]: https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0003-web-ui-as-plugin.md

#![allow(missing_docs)]

pub mod error_tracker;
pub mod log_capture;
pub mod metrics;
pub mod rss_monitor;

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use error_tracker::{ErrorEntry, ErrorStore};
use rss_monitor::RssAlert;

use async_trait::async_trait;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Extension, Json, Router};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::CookieJar;
use bbs_plugin_api::admin::AdminBackupRecord;
use bbs_plugin_api::error::{HostError, PluginError};
use bbs_plugin_api::event::{DomainEvent, MessageRecipient};
use bbs_plugin_api::host::Host;
use bbs_plugin_api::plugin::Plugin;
use bbs_plugin_api::registry::{PluginRegistryApi, ProcessPluginConfig, RegistryError};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;
use tracing::{info, warn};
use uuid::Uuid;

// ── Static assets (embedded at compile time) ──────────────────────────────────

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct StaticFiles;

// ── WebConfig ─────────────────────────────────────────────────────────────────

/// Configuration for the web admin plugin.
///
/// Deserialized from `[plugins.web]` in the operator's TOML config.
/// Only valid when the binary is compiled with `--features admin-web`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebConfig {
    /// Whether to start the web listener.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Address to bind. Defaults to `127.0.0.1:8080`.
    #[serde(default = "default_bind")]
    pub bind: String,

    /// Public origin URL for CSRF and `SameSite` cookie policy.
    ///
    /// Required when `bind` is not a loopback address.
    #[serde(default)]
    pub external_origin: Option<String>,

    /// Set the `Secure` flag on session cookies. Disable only for
    /// local development without TLS.
    #[serde(default = "default_cookie_secure")]
    pub cookie_secure: bool,

    /// Expose Prometheus metrics at `GET /metrics`. Default `false`.
    #[serde(default)]
    pub prometheus: bool,

    /// Override the Content-Security-Policy header.
    #[serde(default)]
    pub csp: Option<String>,

    /// Path to the main config file to include in each backup snapshot.
    ///
    /// Defaults to `config.toml` in the current working directory.
    /// Set to an empty string to disable config backup.
    #[serde(default = "default_config_path")]
    pub config_path: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            bind: default_bind(),
            external_origin: None,
            cookie_secure: default_cookie_secure(),
            prometheus: false,
            csp: None,
            config_path: default_config_path(),
        }
    }
}

fn default_enabled() -> bool {
    true
}
fn default_bind() -> String {
    "127.0.0.1:8080".to_owned()
}
fn default_cookie_secure() -> bool {
    false
}
fn default_config_path() -> Option<String> {
    Some("config.toml".to_owned())
}

// ── Web session store ─────────────────────────────────────────────────────────

const SESSION_COOKIE: &str = "bbs_web_session";
const SESSION_TTL_SECS: u64 = 12 * 60 * 60; // 12 h
const LOG_CHANNEL_CAP: usize = 256;

use log_capture::LogBuffer;

#[derive(Debug)]
struct WebSession {
    username: String,
    permission_level: u8,
    expires_at: Instant,
}

/// Identity injected into request extensions by `auth_middleware`.
#[derive(Debug, Clone)]
struct CurrentUser {
    username: String,
    permission_level: u8,
}

// ── Transport flags ───────────────────────────────────────────────────────────

/// Which built-in transports are compiled in and/or currently enabled.
///
/// Injected by `main.rs` via [`WebPlugin::set_active_transports`] before
/// `start()` is called. Exposed at `GET /api/v1/transports` so the web UI
/// can conditionally show transport-specific pages, and at
/// `GET /api/v1/native-plugins` for the plugin management panel.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct TransportFlags {
    /// MeshCore radio transport enabled.
    pub meshcore: bool,
    /// Meshtastic radio transport enabled.
    pub meshtastic: bool,
    /// Whether MeshCore was compiled into this binary.
    pub compiled_mesh: bool,
    /// Whether Meshtastic was compiled into this binary.
    pub compiled_meshtastic: bool,
    /// Whether the CLI (Unix socket) transport was compiled into this binary.
    pub compiled_cli: bool,
}

// ── Shared state ──────────────────────────────────────────────────────────────

type LogReloadFn = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

struct AppState {
    host: Arc<dyn Host>,
    config: WebConfig,
    /// Directory where backup files are stored.
    ///
    /// Sourced from `[backup] directory` in the operator config and injected
    /// by the host binary after plugin init via [`WebPlugin::set_backup_dir`].
    /// When `None` the backup endpoints return 503.
    backup_dir: std::sync::Mutex<Option<String>>,
    sessions: Mutex<HashMap<String, WebSession>>,
    started_at: Instant,
    log_tx: broadcast::Sender<String>,
    /// Domain-event-only ring buffer (BBS sessions, auth, messages).
    /// Used as fallback when no application-level log buffer is injected.
    log_buf: std::sync::Arc<Mutex<LogBuffer>>,
    /// Application-level log buffer shared with the LogCaptureLayer in main.rs.
    /// When set, combines tracing events + BBS domain events; preferred by api_logs.
    ext_log_buf: std::sync::Mutex<Option<Arc<Mutex<LogBuffer>>>>,
    plugin_registry: std::sync::Mutex<Option<Arc<dyn PluginRegistryApi>>>,
    active_transports: std::sync::Mutex<TransportFlags>,
    pending_restart: AtomicBool,
    log_reload: std::sync::Mutex<Option<LogReloadFn>>,
    error_store: std::sync::Mutex<Option<Arc<Mutex<ErrorStore>>>>,
    error_tx: std::sync::Mutex<Option<broadcast::Sender<ErrorEntry>>>,
    rss_alert_tx: std::sync::Mutex<Option<broadcast::Sender<RssAlert>>>,
}

impl AppState {
    fn new(host: Arc<dyn Host>, config: WebConfig) -> Self {
        let (log_tx, _) = broadcast::channel(LOG_CHANNEL_CAP);
        Self {
            host,
            config,
            backup_dir: std::sync::Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            started_at: Instant::now(),
            log_tx,
            log_buf: std::sync::Arc::new(Mutex::new(LogBuffer::new())),
            ext_log_buf: std::sync::Mutex::new(None),
            plugin_registry: std::sync::Mutex::new(None),
            active_transports: std::sync::Mutex::new(TransportFlags::default()),
            pending_restart: AtomicBool::new(false),
            log_reload: std::sync::Mutex::new(None),
            error_store: std::sync::Mutex::new(None),
            error_tx: std::sync::Mutex::new(None),
            rss_alert_tx: std::sync::Mutex::new(None),
        }
    }

    /// Return the configured backup directory, if any.
    fn backup_dir(&self) -> Option<String> {
        self.backup_dir.lock().expect("backup_dir poisoned").clone()
    }

    fn create_session(&self, username: String, permission_level: u8) -> String {
        let token = Uuid::new_v4().to_string();
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        sessions.insert(
            token.clone(),
            WebSession {
                username,
                permission_level,
                expires_at: Instant::now() + std::time::Duration::from_secs(SESSION_TTL_SECS),
            },
        );
        token
    }

    fn validate_session(&self, token: &str) -> Option<CurrentUser> {
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        match sessions.get(token) {
            Some(s) if s.expires_at > Instant::now() => Some(CurrentUser {
                username: s.username.clone(),
                permission_level: s.permission_level,
            }),
            _ => {
                sessions.remove(token);
                None
            }
        }
    }

    fn remove_session(&self, token: &str) {
        self.sessions
            .lock()
            .expect("sessions poisoned")
            .remove(token);
    }
}

// ── WebPlugin ─────────────────────────────────────────────────────────────────

/// The web admin plugin.
pub struct WebPlugin {
    state: Arc<AppState>,
    listener_slot: Mutex<Option<TcpListener>>,
    shutdown_tx: watch::Sender<bool>,
}

#[async_trait]
impl Plugin for WebPlugin {
    type Config = WebConfig;

    fn name(&self) -> &'static str {
        "web"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    async fn init(config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError> {
        if !config.enabled {
            return Ok(Self {
                state: Arc::new(AppState::new(host, config)),
                listener_slot: Mutex::new(None),
                shutdown_tx: watch::channel(false).0,
            });
        }

        let addr: SocketAddr = config.bind.parse().map_err(|e| {
            PluginError::InvalidConfig(format!(
                "web.bind {:?} is not a valid address: {e}",
                config.bind
            ))
        })?;

        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| PluginError::StartFailed(format!("web: could not bind {addr}: {e}")))?;

        info!(addr = %addr, "web admin: listener bound");

        let (shutdown_tx, _) = watch::channel(false);
        Ok(Self {
            state: Arc::new(AppState::new(host, config)),
            listener_slot: Mutex::new(Some(listener)),
            shutdown_tx,
        })
    }

    async fn start(&self) -> Result<(), PluginError> {
        if !self.state.config.enabled {
            info!("web admin: disabled in config — skipping");
            return Ok(());
        }

        let listener = self
            .listener_slot
            .lock()
            .expect("listener_slot poisoned")
            .take()
            .ok_or_else(|| PluginError::StartFailed("web admin already started".into()))?;

        // Spawn domain-event → SSE + ring-buffer log bridge.
        let log_tx = self.state.log_tx.clone();
        let log_buf = std::sync::Arc::clone(&self.state.log_buf);
        let ext_log_buf = self
            .state
            .ext_log_buf
            .lock()
            .expect("ext_log_buf poisoned")
            .clone();
        let mut events = self.state.host.events();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let line = format_domain_event(&event);
                        log_buf.lock().expect("log_buf poisoned").push(line.clone());
                        if let Some(ref buf) = ext_log_buf {
                            buf.lock().expect("ext_log_buf poisoned").push(line.clone());
                        }
                        let _ = log_tx.send(line);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        let warn = format!("[warn] event stream lagged by {n}");
                        log_buf.lock().expect("log_buf poisoned").push(warn.clone());
                        if let Some(ref buf) = ext_log_buf {
                            buf.lock().expect("ext_log_buf poisoned").push(warn.clone());
                        }
                        let _ = log_tx.send(warn);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Start RSS trend monitor and store its broadcast sender in shared state.
        let rss_tx = rss_monitor::start();
        *self
            .state
            .rss_alert_tx
            .lock()
            .expect("rss_alert_tx poisoned") = Some(rss_tx);

        let state = Arc::clone(&self.state);
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        let app = build_router(state);

        tokio::spawn(async move {
            let serve = axum::serve(listener, app);
            tokio::select! {
                result = serve => {
                    if let Err(e) = result {
                        warn!("web admin server error: {e}");
                    }
                }
                _ = shutdown_rx.changed() => {
                    info!("web admin: shutdown signal received");
                }
            }
        });

        info!(
            bind = %self.state.config.bind,
            "web admin started — open http://{}/", self.state.config.bind
        );
        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        let _ = self.shutdown_tx.send(true);
        info!("web admin stop requested");
        Ok(())
    }
}

impl WebPlugin {
    /// Inject the process plugin registry so the web API can manage plugins.
    ///
    /// Must be called before `start()`.  Safe to call with `None` to disable
    /// plugin management endpoints (they return 501 in that case).
    pub fn set_plugin_registry(&self, registry: Arc<dyn PluginRegistryApi>) {
        *self
            .state
            .plugin_registry
            .lock()
            .expect("plugin_registry poisoned") = Some(registry);
    }

    /// Inject which built-in transports are active.
    ///
    /// Must be called before `start()`. Exposed at `GET /api/v1/transports`
    /// so the SPA can conditionally show transport-specific nav items.
    pub fn set_active_transports(&self, flags: TransportFlags) {
        *self
            .state
            .active_transports
            .lock()
            .expect("active_transports poisoned") = flags;
    }

    /// Inject the tracing-subscriber reload handle so the web API can change
    /// the log level at runtime without a restart.
    ///
    /// Must be called before `start()`.
    pub fn set_log_reload(&self, reload: LogReloadFn) {
        *self.state.log_reload.lock().expect("log_reload poisoned") = Some(reload);
    }

    /// Inject the application-level log buffer shared with the `LogCaptureLayer`.
    ///
    /// When set, `GET /api/v1/logs` returns tracing-level events (INFO/WARN/ERROR)
    /// from all crates in addition to BBS domain events.  Must be called before
    /// `start()`.
    pub fn set_log_buffer(&self, buf: Arc<Mutex<LogBuffer>>) {
        *self.state.ext_log_buf.lock().expect("ext_log_buf poisoned") = Some(buf);
    }

    /// Inject the error tracker store and broadcast sender.
    ///
    /// Must be called before `start()`.  Enables the `GET /api/v1/errors`
    /// endpoint and the `GET /api/v1/sse/errors` stream.
    pub fn set_error_store(
        &self,
        store: Arc<Mutex<ErrorStore>>,
        tx: broadcast::Sender<ErrorEntry>,
    ) {
        *self.state.error_store.lock().expect("error_store poisoned") = Some(store);
        *self.state.error_tx.lock().expect("error_tx poisoned") = Some(tx);
    }

    /// Set the directory where backup files are stored.
    ///
    /// Always sourced from `[backup] directory` in the operator config —
    /// there is no separate `[plugins.web] backup_dir` setting.  Must be
    /// called before `start()`.
    pub fn set_backup_dir(&self, dir: Option<String>) {
        *self.state.backup_dir.lock().expect("backup_dir poisoned") = dir;
    }
}

// ── Router ────────────────────────────────────────────────────────────────────

fn build_router(state: Arc<AppState>) -> Router {
    let protected_api = Router::new()
        .route("/auth/whoami", get(api_whoami))
        .route("/auth/logout", post(api_logout))
        .route("/status", get(api_status))
        .route("/transports", get(api_active_transports))
        .route("/native-plugins", get(api_list_native_plugins))
        .route("/native-plugins/:name", patch(api_update_native_plugin))
        .route("/adverts", get(api_adverts))
        .route("/adverts/send", post(api_adverts_send))
        .route("/sessions", get(api_list_sessions))
        .route("/sessions/:id", delete(api_kill_session))
        .route("/users", get(api_list_users))
        .route("/users/:username", patch(api_update_user))
        .route("/rooms", get(api_list_rooms).post(api_create_room))
        .route("/rooms/:id", patch(api_update_room).delete(api_delete_room))
        .route("/rooms/:id/messages", get(api_list_messages))
        .route("/messages/search", get(api_search_messages))
        .route("/messages/:id", delete(api_delete_message))
        .route("/audit-log", get(api_audit_log))
        .route("/stats", get(api_stats))
        .route("/reports", get(api_reports))
        .route("/settings", get(api_settings))
        .route("/config", get(api_get_config).patch(api_patch_config))
        .route(
            "/access-policy",
            get(api_get_access_policy).patch(api_patch_access_policy),
        )
        .route(
            "/radio-config",
            get(api_get_radio_config).patch(api_patch_radio_config),
        )
        .route("/node-identity", get(api_get_node_identity))
        .route("/node-identity/export-key", post(api_export_node_key))
        .route("/node-identity/import-key", post(api_import_node_key))
        .route("/restart", post(api_restart))
        .route("/logs", get(api_logs))
        .route("/sse/logs", get(api_sse_logs))
        .route("/sse/events", get(api_sse_events))
        .route("/sse/errors", get(api_sse_errors))
        .route("/sse/rss-alert", get(api_sse_rss_alert))
        .route("/errors", get(api_errors))
        .route("/metrics", get(api_metrics))
        .route("/backups", get(api_list_backups).post(api_trigger_backup))
        .route(
            "/backups/:filename",
            get(api_download_backup).delete(api_delete_backup),
        )
        .route("/plugins", get(api_list_plugins).post(api_add_plugin))
        .route(
            "/plugins/:name",
            delete(api_remove_plugin).patch(api_update_plugin),
        )
        .route("/plugins/:name/restart", post(api_restart_plugin))
        .route("/plugins/:name/logs", get(api_plugin_logs))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    let public_api = Router::new()
        .route("/auth/login", post(api_login))
        .route("/health", get(api_health));

    Router::new()
        .nest("/api/v1", protected_api)
        .nest("/api/v1", public_api)
        .fallback(spa_handler)
        .with_state(state)
}

// ── Auth middleware ───────────────────────────────────────────────────────────

async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    mut req: Request,
    next: Next,
) -> Response {
    let token = jar
        .get(SESSION_COOKIE)
        .map(|c| c.value().to_owned())
        .unwrap_or_default();

    match state.validate_session(&token) {
        Some(user) => {
            req.extensions_mut().insert(user);
            next.run(req).await
        }
        None => (
            StatusCode::UNAUTHORIZED,
            Json(json_error("not authenticated")),
        )
            .into_response(),
    }
}

// ── Auth handlers ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct WhoamiResponse {
    username: String,
    is_sysop: bool,
    permission_level: u8,
}

async fn api_whoami(Extension(user): Extension<CurrentUser>) -> impl IntoResponse {
    Json(WhoamiResponse {
        is_sysop: user.permission_level >= 100,
        permission_level: user.permission_level,
        username: user.username,
    })
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    ok: bool,
    username: String,
    permission_level: u8,
}

async fn api_login(
    State(state): State<Arc<AppState>>,
    jar: CookieJar,
    Json(body): Json<LoginRequest>,
) -> Response {
    let level = match state
        .host
        .admin_verify_credentials(&body.username, &body.password)
        .await
    {
        Ok(l) => l,
        Err(HostError::NotFound(_) | HostError::PermissionDenied { .. }) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json_error("invalid credentials")),
            )
                .into_response();
        }
        Err(e) => {
            warn!("login error: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json_error("login failed")),
            )
                .into_response();
        }
    };

    let level_u8 = level as u8;
    let token = state.create_session(body.username.clone(), level_u8);
    let mut cookie = Cookie::new(SESSION_COOKIE, token);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Strict);
    cookie.set_path("/");
    if state.config.cookie_secure {
        cookie.set_secure(true);
    }

    (
        jar.add(cookie),
        Json(LoginResponse {
            ok: true,
            username: body.username,
            permission_level: level_u8,
        }),
    )
        .into_response()
}

async fn api_logout(State(state): State<Arc<AppState>>, jar: CookieJar) -> Response {
    if let Some(c) = jar.get(SESSION_COOKIE) {
        state.remove_session(c.value());
    }
    let removal = Cookie::build((SESSION_COOKIE, "")).path("/").build();
    (jar.remove(removal), Json(serde_json::json!({"ok": true}))).into_response()
}

// ── Status ────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct StatusResponse {
    version: &'static str,
    uptime_secs: u64,
}

async fn api_status(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION"),
        uptime_secs: state.started_at.elapsed().as_secs(),
    })
}

async fn api_active_transports(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let flags = *state
        .active_transports
        .lock()
        .expect("active_transports poisoned");
    Json(flags)
}

// ── Native plugins ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct NativePluginInfo {
    name: &'static str,
    label: &'static str,
    compiled_in: bool,
    enabled: bool,
    connection_type: Option<String>,
}

#[derive(Serialize)]
struct NativePluginsResponse {
    plugins: Vec<NativePluginInfo>,
    pending_restart: bool,
}

#[derive(Deserialize)]
struct UpdateNativePluginBody {
    enabled: Option<bool>,
}

async fn api_list_native_plugins(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }

    let pending_restart = state.pending_restart.load(Ordering::Relaxed);
    let flags = *state
        .active_transports
        .lock()
        .expect("active_transports poisoned");
    let config_val = state
        .config
        .config_path
        .as_deref()
        .filter(|p| !p.is_empty())
        .and_then(|p| read_config_toml(p).ok());

    let plugins = vec![
        NativePluginInfo {
            name: "mesh",
            label: "MeshCore",
            compiled_in: flags.compiled_mesh,
            enabled: config_val
                .as_ref()
                .and_then(|v| toml_plugin_bool(v, "mesh", "enabled"))
                .unwrap_or(true),
            connection_type: config_val
                .as_ref()
                .and_then(|v| toml_plugin_str(v, "mesh", "connection_type")),
        },
        NativePluginInfo {
            name: "meshtastic",
            label: "Meshtastic",
            compiled_in: flags.compiled_meshtastic,
            enabled: config_val
                .as_ref()
                .and_then(|v| toml_plugin_bool(v, "meshtastic", "enabled"))
                .unwrap_or(false),
            connection_type: config_val
                .as_ref()
                .and_then(|v| toml_plugin_str(v, "meshtastic", "connection_type")),
        },
        NativePluginInfo {
            name: "cli",
            label: "CLI (Unix socket)",
            compiled_in: flags.compiled_cli,
            enabled: config_val
                .as_ref()
                .and_then(|v| toml_plugin_bool(v, "cli", "enabled"))
                .unwrap_or(true),
            connection_type: None,
        },
    ];

    Json(NativePluginsResponse {
        plugins,
        pending_restart,
    })
    .into_response()
}

async fn api_update_native_plugin(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
    Path(name): Path<String>,
    Json(body): Json<UpdateNativePluginBody>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }

    let Some(enabled) = body.enabled else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json_error("enabled field is required")),
        )
            .into_response();
    };

    let flags = *state
        .active_transports
        .lock()
        .expect("active_transports poisoned");
    let compiled_in = match name.as_str() {
        "mesh" => flags.compiled_mesh,
        "meshtastic" => flags.compiled_meshtastic,
        "cli" => flags.compiled_cli,
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json_error("unknown native plugin")),
            )
                .into_response()
        }
    };

    if !compiled_in {
        return (
            StatusCode::BAD_REQUEST,
            Json(json_error("plugin not compiled into this binary")),
        )
            .into_response();
    }

    let path = match &state.config.config_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json_error(
                    "config_path not set in [plugins.web] — cannot write config",
                )),
            )
                .into_response()
        }
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json_error(&format!("could not read config file: {e}"))),
            )
                .into_response()
        }
    };
    let mut doc = match raw.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json_error(&format!("could not parse config file: {e}"))),
            )
                .into_response()
        }
    };

    doc["plugins"][name.as_str()]["enabled"] = toml_edit::value(enabled);

    if let Err(e) = std::fs::write(&path, doc.to_string()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json_error(&format!("could not write config file: {e}"))),
        )
            .into_response();
    }

    state.pending_restart.store(true, Ordering::Relaxed);

    let action = if enabled {
        "enable_native_plugin"
    } else {
        "disable_native_plugin"
    };
    let _ = state
        .host
        .admin_write_audit(&format!("web:{}", user.username), action, Some(&name), None)
        .await;

    Json(serde_json::json!({ "ok": true, "pending_restart": true })).into_response()
}

// ── Adverts ───────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct AdvertResponse {
    ts: i64,
    pubkey: String,
    name: String,
    #[serde(rename = "type")]
    adv_type: u8,
    type_name: String,
    lat: f64,
    lon: f64,
}

async fn api_adverts(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let records = state.host.advert_bus().list();
    let out: Vec<AdvertResponse> = records
        .into_iter()
        .map(|r| AdvertResponse {
            ts: r.last_seen_secs,
            pubkey: r.pubkey_hex,
            name: r.name,
            adv_type: r.adv_type,
            type_name: adv_type_name(r.adv_type).to_owned(),
            lat: r.lat,
            lon: r.lon,
        })
        .collect();
    Json(out)
}

fn adv_type_name(t: u8) -> &'static str {
    match t {
        1 => "chat",
        2 => "room",
        3 => "repeater",
        4 => "sensor",
        _ => "unknown",
    }
}

#[derive(Deserialize)]
struct SendAdvertRequest {
    #[serde(default = "default_flood")]
    flood: bool,
}
fn default_flood() -> bool {
    true
}

#[derive(Serialize)]
struct SendAdvertResponse {
    ok: bool,
    flood: bool,
    sent_at: i64,
}

async fn api_adverts_send(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SendAdvertRequest>,
) -> Response {
    let bus = state.host.advert_bus();
    let queued = bus.request_send(body.flood);

    if !queued {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json_error("mesh transport not running")),
        )
            .into_response();
    }

    let sent_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);

    Json(SendAdvertResponse {
        ok: true,
        flood: body.flood,
        sent_at,
    })
    .into_response()
}

// ── Sessions ──────────────────────────────────────────────────────────────────

async fn api_list_sessions(State(state): State<Arc<AppState>>) -> Response {
    match state.host.admin_list_sessions().await {
        Ok(s) => Json(s).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

async fn api_kill_session(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<CurrentUser>,
    Path(id): Path<u64>,
) -> Response {
    if caller.permission_level < 100 {
        return (
            StatusCode::FORBIDDEN,
            Json(json_error("sysop required to kill sessions")),
        )
            .into_response();
    }
    match state.host.admin_kill_session(id).await {
        Ok(true) => {
            let actor_str = format!("web:{}", caller.username);
            if let Err(e) = state
                .host
                .admin_write_audit(&actor_str, "kill_session", Some(&format!("{id}")), None)
                .await
            {
                warn!("audit write failed: {e}");
            }
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, Json(json_error("session not found"))).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

// ── Users ─────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ListUsersQuery {
    status: Option<u8>,
    #[serde(default = "default_page_size")]
    limit: u32,
    #[serde(default)]
    offset: u32,
}

fn default_page_size() -> u32 {
    100
}

async fn api_list_users(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListUsersQuery>,
) -> Response {
    match state
        .host
        .admin_list_users(q.status, q.limit, q.offset)
        .await
    {
        Ok(u) => Json(u).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

#[derive(Deserialize)]
struct UpdateUserBody {
    status: Option<u8>,
    permission_level: Option<u8>,
    password: Option<String>,
}

async fn api_update_user(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<CurrentUser>,
    Path(username): Path<String>,
    Json(body): Json<UpdateUserBody>,
) -> Response {
    if body.status.is_none() && body.permission_level.is_none() && body.password.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json_error(
                "at least one of status, permission_level, or password is required",
            )),
        )
            .into_response();
    }
    // Only Sysop (level 100) may change permission levels or reset passwords.
    if body.permission_level.is_some() && caller.permission_level < 100 {
        return (
            StatusCode::FORBIDDEN,
            Json(json_error("sysop required to change permission level")),
        )
            .into_response();
    }
    if body.password.is_some() && caller.permission_level < 100 {
        return (
            StatusCode::FORBIDDEN,
            Json(json_error("sysop required to reset a password")),
        )
            .into_response();
    }
    if let Some(ref pw) = body.password {
        if pw.len() < 6 {
            return (
                StatusCode::BAD_REQUEST,
                Json(json_error("password must be at least 6 characters")),
            )
                .into_response();
        }
    }

    let actor_str = format!("web:{}", caller.username);

    if body.status.is_some() || body.permission_level.is_some() {
        match state
            .host
            .admin_update_user(&username, body.status, body.permission_level)
            .await
        {
            Ok(()) => {
                if let Some(s) = body.status {
                    let action = if s == 1 { "ban" } else { "unban" };
                    if let Err(e) = state
                        .host
                        .admin_write_audit(&actor_str, action, Some(username.as_str()), None)
                        .await
                    {
                        warn!("audit write failed: {e}");
                    }
                }
                if let Some(lvl) = body.permission_level {
                    let detail = format!("level -> {lvl}");
                    if let Err(e) = state
                        .host
                        .admin_write_audit(
                            &actor_str,
                            "set_permission",
                            Some(username.as_str()),
                            Some(&detail),
                        )
                        .await
                    {
                        warn!("audit write failed: {e}");
                    }
                }
            }
            Err(HostError::NotFound(_)) => {
                return (StatusCode::NOT_FOUND, Json(json_error("user not found"))).into_response();
            }
            Err(e) => return server_error(&e.to_string()),
        }
    }

    if let Some(ref pw) = body.password {
        match state.host.admin_set_password(&username, pw).await {
            Ok(()) => {
                if let Err(e) = state
                    .host
                    .admin_write_audit(&actor_str, "reset_password", Some(username.as_str()), None)
                    .await
                {
                    warn!("audit write failed: {e}");
                }
            }
            Err(HostError::NotFound(_)) => {
                return (StatusCode::NOT_FOUND, Json(json_error("user not found"))).into_response();
            }
            Err(e) => return server_error(&e.to_string()),
        }
    }

    Json(serde_json::json!({"ok": true})).into_response()
}

// ── Rooms ─────────────────────────────────────────────────────────────────────

async fn api_list_rooms(State(state): State<Arc<AppState>>) -> Response {
    match state.host.admin_list_rooms().await {
        Ok(r) => Json(r).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

#[derive(Deserialize)]
struct CreateRoomBody {
    name: String,
    description: Option<String>,
}

async fn api_create_room(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<CurrentUser>,
    Json(body): Json<CreateRoomBody>,
) -> Response {
    if caller.permission_level < 100 {
        return (
            StatusCode::FORBIDDEN,
            Json(json_error("sysop required to create rooms")),
        )
            .into_response();
    }
    let name = body.name.trim();
    if name.is_empty() || name.len() > 64 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json_error("room name must be 1–64 characters")),
        )
            .into_response();
    }
    if body.description.as_deref().map(str::len).unwrap_or(0) > 512 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json_error("description max 512 characters")),
        )
            .into_response();
    }
    match state
        .host
        .admin_create_room(name, body.description.as_deref())
        .await
    {
        Ok(room) => {
            let actor_str = format!("web:{}", caller.username);
            if let Err(e) = state
                .host
                .admin_write_audit(&actor_str, "create_room", Some(name), None)
                .await
            {
                warn!("audit write failed: {e}");
            }
            (StatusCode::CREATED, Json(room)).into_response()
        }
        Err(e) => server_error(&e.to_string()),
    }
}

#[derive(Deserialize)]
struct UpdateRoomBody {
    description: Option<serde_json::Value>, // null = clear, string = set, absent = leave
    read_only: Option<bool>,
    min_permission_level: Option<u8>,
}

async fn api_update_room(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<CurrentUser>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateRoomBody>,
) -> Response {
    if caller.permission_level < 100 {
        return (
            StatusCode::FORBIDDEN,
            Json(json_error("sysop required to edit rooms")),
        )
            .into_response();
    }

    // Mail (2), Aides (3), and Sysop (4) have fixed permissions and read-only settings.
    if (2..=4).contains(&id) && (body.read_only.is_some() || body.min_permission_level.is_some()) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json_error(
                "permission level and read-only are locked for this room",
            )),
        )
            .into_response();
    }

    // Convert JSON Value for description: absent key → None (leave), null → Some(None) (clear),
    // string → Some(Some(s)) (set).
    let description: Option<Option<String>> = match body.description {
        None => None,
        Some(serde_json::Value::Null) => Some(None),
        Some(serde_json::Value::String(s)) => Some(Some(s)),
        Some(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json_error("description must be a string or null")),
            )
                .into_response()
        }
    };

    match state
        .host
        .admin_update_room(id, description, body.read_only, body.min_permission_level)
        .await
    {
        Ok(room) => {
            let actor_str = format!("web:{}", caller.username);
            if let Err(e) = state
                .host
                .admin_write_audit(&actor_str, "edit_room", Some(&room.name), None)
                .await
            {
                warn!("audit write failed: {e}");
            }
            Json(room).into_response()
        }
        Err(HostError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, Json(json_error("room not found"))).into_response()
        }
        Err(e) => server_error(&e.to_string()),
    }
}

async fn api_delete_room(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<CurrentUser>,
    Path(id): Path<i64>,
) -> Response {
    if caller.permission_level < 100 {
        return (
            StatusCode::FORBIDDEN,
            Json(json_error("sysop required to delete rooms")),
        )
            .into_response();
    }
    match state.host.admin_delete_room(id).await {
        Ok(true) => {
            let actor_str = format!("web:{}", caller.username);
            if let Err(e) = state
                .host
                .admin_write_audit(&actor_str, "delete_room", Some(&format!("id={id}")), None)
                .await
            {
                warn!("audit write failed: {e}");
            }
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(json_error("room not found or protected")),
        )
            .into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

// ── Messages ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ListMessagesQuery {
    #[serde(default = "default_page_size")]
    limit: u32,
    after_id: Option<i64>,
}

async fn api_list_messages(
    State(state): State<Arc<AppState>>,
    Path(room_id): Path<i64>,
    Query(q): Query<ListMessagesQuery>,
) -> Response {
    // Room 2 is the Mail room — DMs are private and must never be exposed here.
    if room_id == 2 {
        return (
            StatusCode::FORBIDDEN,
            Json(json_error("mail room messages are private")),
        )
            .into_response();
    }
    match state
        .host
        .admin_list_messages(room_id, q.limit, q.after_id)
        .await
    {
        Ok(m) => Json(m).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

#[derive(Deserialize)]
struct SearchMessagesQuery {
    sender: Option<String>,
    q: Option<String>,
    #[serde(default = "default_page_size")]
    limit: u32,
}

async fn api_search_messages(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchMessagesQuery>,
) -> Response {
    match state
        .host
        .admin_search_messages(params.sender.as_deref(), params.q.as_deref(), params.limit)
        .await
    {
        Ok(m) => Json(m).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

async fn api_delete_message(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<CurrentUser>,
    Path(id): Path<i64>,
) -> Response {
    match state.host.admin_delete_message(id).await {
        Ok(true) => {
            let actor_str = format!("web:{}", caller.username);
            if let Err(e) = state
                .host
                .admin_write_audit(&actor_str, "delete_message", Some(&format!("#{id}")), None)
                .await
            {
                warn!("audit write failed: {e}");
            }
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, Json(json_error("message not found"))).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

// ── Audit log ─────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct AuditLogQuery {
    #[serde(default = "default_page_size")]
    limit: u32,
    #[serde(default)]
    offset: u32,
    action: Option<String>,
}

async fn api_audit_log(
    State(state): State<Arc<AppState>>,
    Query(q): Query<AuditLogQuery>,
) -> Response {
    match state
        .host
        .admin_audit_log(q.limit, q.offset, q.action.as_deref())
        .await
    {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

// ── Stats ─────────────────────────────────────────────────────────────────────

async fn api_stats(State(state): State<Arc<AppState>>) -> Response {
    match state.host.admin_stats().await {
        Ok(s) => Json(s).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

async fn api_reports(State(state): State<Arc<AppState>>) -> Response {
    match state.host.admin_reports().await {
        Ok(r) => Json(r).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

// ── Error report ─────────────────────────────────────────────────────────────

async fn api_errors(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let store_guard = state.error_store.lock().expect("error_store poisoned");
    let entries = match store_guard.as_ref() {
        Some(store) => store
            .lock()
            .expect("error_store inner poisoned")
            .list_sorted(),
        None => vec![],
    };
    Json(entries)
}

async fn api_sse_errors(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx_opt = state
        .error_tx
        .lock()
        .expect("error_tx poisoned")
        .as_ref()
        .map(|tx| tx.subscribe());

    let stream: Box<dyn tokio_stream::Stream<Item = Result<Event, Infallible>> + Send + Unpin> =
        match rx_opt {
            Some(rx) => Box::new(BroadcastStream::new(rx).filter_map(|res| {
                match res {
                    Ok(entry) => serde_json::to_string(&entry)
                        .ok()
                        .map(|json| Ok(Event::default().event("error_alert").data(json))),
                    Err(_) => None,
                }
            })),
            None => Box::new(tokio_stream::empty()),
        };

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

async fn api_sse_rss_alert(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx_opt = state
        .rss_alert_tx
        .lock()
        .expect("rss_alert_tx poisoned")
        .as_ref()
        .map(|tx| tx.subscribe());

    let stream: Box<dyn tokio_stream::Stream<Item = Result<Event, Infallible>> + Send + Unpin> =
        match rx_opt {
            Some(rx) => Box::new(BroadcastStream::new(rx).filter_map(|res| {
                match res {
                    Ok(alert) => serde_json::to_string(&alert)
                        .ok()
                        .map(|json| Ok(Event::default().event("rss_alert").data(json))),
                    Err(_) => None,
                }
            })),
            None => Box::new(tokio_stream::empty()),
        };

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

// ── System metrics ───────────────────────────────────────────────────────────

async fn api_metrics() -> Response {
    match tokio::task::spawn_blocking(metrics::collect).await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => server_error(&format!("metrics collection panicked: {e}")),
    }
}

// ── Settings ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SettingsResponse {
    backup_dir: Option<String>,
}

async fn api_settings(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(SettingsResponse {
        backup_dir: state.backup_dir(),
    })
}

// ── Config read / write ───────────────────────────────────────────────────────

/// One entry in the `presets` list returned by `GET /api/v1/radio-config`.
#[derive(Debug, Clone, Serialize)]
struct RadioPresetDetail {
    name: &'static str,
    frequency_hz: u64,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate: u8,
    tx_power_dbm: i32,
}

/// Response body for `GET /api/v1/radio-config`.
#[derive(Debug, Serialize)]
struct RadioConfigResponse {
    preset: Option<String>,
    frequency_hz: Option<u64>,
    bandwidth_hz: Option<u32>,
    spreading_factor: Option<u8>,
    coding_rate: Option<u8>,
    tx_power_dbm: Option<i32>,
    /// MeshCore connection type: `"serial"`, `"tcp"`, or `"hat"`.
    connection_type: Option<String>,
    /// Serial port path (only set when `connection_type` is `"serial"`).
    serial_port: Option<String>,
    /// Full preset details for populating the UI dropdown and auto-filling fields.
    presets: Vec<RadioPresetDetail>,
}

/// Patch body for `PATCH /api/v1/radio-config`.
/// `None` means "leave unchanged"; for optional fields, JSON `null` clears the value.
#[derive(Debug, Deserialize)]
struct RadioConfigPatch {
    /// Named preset. JSON null clears it; a string sets it.
    preset: Option<serde_json::Value>,
    /// Carrier frequency in Hz. JSON null clears it.
    frequency_hz: Option<serde_json::Value>,
    /// Channel bandwidth in Hz. JSON null clears it.
    bandwidth_hz: Option<serde_json::Value>,
    /// LoRa spreading factor (7–12). JSON null clears it.
    spreading_factor: Option<serde_json::Value>,
    /// Coding rate denominator (5–8). JSON null clears it.
    coding_rate: Option<serde_json::Value>,
    /// TX power in dBm. JSON null clears it.
    tx_power_dbm: Option<serde_json::Value>,
}

/// Editable subset of the BBS configuration, returned by GET /api/v1/config.
///
/// Only fields that are safe to change via the web UI are included.
/// All fields are `Option` so the frontend can distinguish "not set in file"
/// from "explicitly set to the default value".
#[derive(Debug, Default, Serialize, Deserialize)]
struct ConfigResponse {
    config_file: Option<String>,
    /// Whether the config file is writable by this process.
    writable: bool,
    /// Server's system timezone (best-effort; TZ env → /etc/timezone → UTC).
    server_timezone: String,
    bbs_name: Option<String>,
    bbs_starting_room: Option<String>,
    bbs_welcome_msg: Option<String>,
    bbs_timezone: Option<String>,
    location_latitude: Option<f64>,
    location_longitude: Option<f64>,
    backup_enabled: Option<bool>,
    backup_interval_hours: Option<u32>,
    backup_keep_daily: Option<u32>,
    backup_keep_weekly: Option<u32>,
    security_session_web_secs: Option<u64>,
    security_session_mesh_secs: Option<u64>,
    security_login_rate_per_min: Option<u32>,
    security_command_rate_per_min: Option<u32>,
    logging_level: Option<String>,
}

/// Fields accepted by PATCH /api/v1/config.
/// `None` means "leave this field unchanged".
#[derive(Debug, Deserialize)]
struct ConfigPatch {
    bbs_name: Option<String>,
    bbs_starting_room: Option<String>,
    bbs_welcome_msg: Option<String>,
    bbs_timezone: Option<String>,
    location_latitude: Option<serde_json::Value>, // null clears, number sets
    location_longitude: Option<serde_json::Value>,
    backup_enabled: Option<bool>,
    backup_interval_hours: Option<u32>,
    backup_keep_daily: Option<u32>,
    backup_keep_weekly: Option<u32>,
    security_session_web_secs: Option<u64>,
    security_session_mesh_secs: Option<u64>,
    security_login_rate_per_min: Option<u32>,
    security_command_rate_per_min: Option<u32>,
    logging_level: Option<String>,
}

async fn api_health() -> Response {
    Json(serde_json::json!({ "status": "ok" })).into_response()
}

async fn api_restart(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }

    if std::env::var("INVOCATION_ID").is_err() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json_error(
                "not running under systemd — restart manually: sudo systemctl restart supply-drop-bbs",
            )),
        )
            .into_response();
    }

    let _ = state
        .host
        .admin_write_audit(
            &format!("web:{}", user.username),
            "service_restart",
            None,
            None,
        )
        .await;

    // Spawn restart after a short delay so the 202 response can leave first.
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let _ = std::process::Command::new("sudo")
            .args(["systemctl", "restart", "supply-drop-bbs"])
            .spawn();
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "message": "restart initiated" })),
    )
        .into_response()
}

fn system_timezone() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        if !tz.is_empty() {
            return tz;
        }
    }
    if let Ok(content) = std::fs::read_to_string("/etc/timezone") {
        let tz = content.trim().to_owned();
        if !tz.is_empty() {
            return tz;
        }
    }
    "UTC".to_owned()
}

/// Preset names mirrored from `src/mesh_presets.rs`.
/// Duplicated here because `bbs-web` does not depend on the main binary.
/// Full preset data mirrored from `src/mesh_presets.rs`.
/// Duplicated here because `bbs-web` does not depend on the main binary.
const RADIO_PRESETS: &[RadioPresetDetail] = &[
    RadioPresetDetail {
        name: "Australia",
        frequency_hz: 915_800_000,
        bandwidth_hz: 250_000,
        spreading_factor: 10,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "Australia (Narrow)",
        frequency_hz: 916_575_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "Australia SA, WA, QLD",
        frequency_hz: 923_125_000,
        bandwidth_hz: 62_500,
        spreading_factor: 8,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "Czech Republic",
        frequency_hz: 869_432_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RadioPresetDetail {
        name: "EU 433MHz",
        frequency_hz: 433_650_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "EU/UK (Long Range)",
        frequency_hz: 869_525_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RadioPresetDetail {
        name: "EU/UK (Medium Range)",
        frequency_hz: 869_525_000,
        bandwidth_hz: 250_000,
        spreading_factor: 10,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RadioPresetDetail {
        name: "EU/UK (Narrow)",
        frequency_hz: 869_618_000,
        bandwidth_hz: 62_500,
        spreading_factor: 8,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RadioPresetDetail {
        name: "New Zealand",
        frequency_hz: 917_375_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "New Zealand (Narrow)",
        frequency_hz: 917_375_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "Portugal 433",
        frequency_hz: 433_375_000,
        bandwidth_hz: 62_500,
        spreading_factor: 9,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "Portugal 869",
        frequency_hz: 869_618_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RadioPresetDetail {
        name: "Switzerland",
        frequency_hz: 869_618_000,
        bandwidth_hz: 62_500,
        spreading_factor: 8,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RadioPresetDetail {
        name: "USA Arizona",
        frequency_hz: 908_205_000,
        bandwidth_hz: 62_500,
        spreading_factor: 10,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "USA/Canada",
        frequency_hz: 910_525_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "Vietnam",
        frequency_hz: 920_250_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "Off-Grid 433",
        frequency_hz: 433_000_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 8,
        tx_power_dbm: 20,
    },
    RadioPresetDetail {
        name: "Off-Grid 869",
        frequency_hz: 869_000_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 8,
        tx_power_dbm: 14,
    },
    RadioPresetDetail {
        name: "Off-Grid 918",
        frequency_hz: 918_000_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 8,
        tx_power_dbm: 20,
    },
];

fn read_config_toml(path: &str) -> Result<toml::Value, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("could not read config file: {e}"))?;
    raw.parse::<toml::Value>()
        .map_err(|e| format!("could not parse config file: {e}"))
}

fn toml_str_field(val: &toml::Value, section: &str, key: &str) -> Option<String> {
    val.get(section)?.get(key)?.as_str().map(str::to_owned)
}

fn toml_bool_field(val: &toml::Value, section: &str, key: &str) -> Option<bool> {
    val.get(section)?.get(key)?.as_bool()
}

fn toml_u32_field(val: &toml::Value, section: &str, key: &str) -> Option<u32> {
    val.get(section)?.get(key)?.as_integer().map(|i| i as u32)
}

fn toml_u64_field(val: &toml::Value, section: &str, key: &str) -> Option<u64> {
    val.get(section)?.get(key)?.as_integer().map(|i| i as u64)
}

fn toml_f64_field(val: &toml::Value, section: &str, key: &str) -> Option<f64> {
    val.get(section)?.get(key)?.as_float()
}

fn toml_radio_str(val: &toml::Value, key: &str) -> Option<String> {
    val.get("plugins")?
        .get("mesh")?
        .get("radio")?
        .get(key)?
        .as_str()
        .map(str::to_owned)
}

fn toml_radio_u64(val: &toml::Value, key: &str) -> Option<u64> {
    val.get("plugins")?
        .get("mesh")?
        .get("radio")?
        .get(key)?
        .as_integer()
        .map(|i| i as u64)
}

fn toml_radio_u32(val: &toml::Value, key: &str) -> Option<u32> {
    val.get("plugins")?
        .get("mesh")?
        .get("radio")?
        .get(key)?
        .as_integer()
        .map(|i| i as u32)
}

fn toml_radio_i32(val: &toml::Value, key: &str) -> Option<i32> {
    val.get("plugins")?
        .get("mesh")?
        .get("radio")?
        .get(key)?
        .as_integer()
        .map(|i| i as i32)
}

/// Set a scalar key in `[plugins.mesh.radio]`, creating the table path as needed.
fn doc_set_radio_field(doc: &mut toml_edit::DocumentMut, key: &str, val: toml_edit::Value) {
    // Ensure [plugins] exists as a table
    if doc.get("plugins").is_none() {
        doc["plugins"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    let plugins = doc["plugins"].as_table_mut().unwrap();
    if plugins.get("mesh").is_none() {
        plugins.insert("mesh", toml_edit::Item::Table(toml_edit::Table::new()));
    }
    let mesh = plugins.get_mut("mesh").unwrap().as_table_mut().unwrap();
    if mesh.get("radio").is_none() {
        mesh.insert("radio", toml_edit::Item::Table(toml_edit::Table::new()));
    }
    let radio = mesh.get_mut("radio").unwrap().as_table_mut().unwrap();
    radio.insert(key, toml_edit::Item::Value(val));
}

/// Remove a key from `[plugins.mesh.radio]` if the table path exists.
fn doc_remove_radio_field(doc: &mut toml_edit::DocumentMut, key: &str) {
    if let Some(plugins) = doc.get_mut("plugins") {
        if let Some(mesh) = plugins.as_table_mut().and_then(|t| t.get_mut("mesh")) {
            if let Some(radio) = mesh.as_table_mut().and_then(|t| t.get_mut("radio")) {
                if let Some(t) = radio.as_table_mut() {
                    t.remove(key);
                }
            }
        }
    }
}

/// Remove a key from a section in a [`toml_edit::DocumentMut`], if it exists.
fn doc_remove_key(doc: &mut toml_edit::DocumentMut, section: &str, key: &str) {
    if let Some(item) = doc.get_mut(section) {
        if let Some(t) = item.as_table_mut() {
            t.remove(key);
        }
    }
}

/// Read a bool from `[plugins.<plugin>].<key>`.
fn toml_plugin_bool(val: &toml::Value, plugin: &str, key: &str) -> Option<bool> {
    val.get("plugins")?.get(plugin)?.get(key)?.as_bool()
}

/// Read a string from `[plugins.<plugin>].<key>`.
fn toml_plugin_str(val: &toml::Value, plugin: &str, key: &str) -> Option<String> {
    val.get("plugins")?
        .get(plugin)?
        .get(key)?
        .as_str()
        .map(str::to_owned)
}

async fn api_get_config(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }

    let path = match &state.config.config_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json_error(
                    "config_path not set in [plugins.web] — cannot read config",
                )),
            )
                .into_response()
        }
    };

    let val = match read_config_toml(&path) {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json_error(&e))).into_response(),
    };

    let writable = std::fs::OpenOptions::new().write(true).open(&path).is_ok();

    let resp = ConfigResponse {
        config_file: Some(path),
        writable,
        server_timezone: system_timezone(),
        bbs_name: toml_str_field(&val, "bbs", "name"),
        bbs_starting_room: toml_str_field(&val, "bbs", "starting_room"),
        bbs_welcome_msg: toml_str_field(&val, "bbs", "welcome_msg"),
        bbs_timezone: toml_str_field(&val, "bbs", "timezone"),
        location_latitude: toml_f64_field(&val, "location", "latitude"),
        location_longitude: toml_f64_field(&val, "location", "longitude"),
        backup_enabled: toml_bool_field(&val, "backup", "enabled"),
        backup_interval_hours: toml_u32_field(&val, "backup", "interval_hours"),
        backup_keep_daily: toml_u32_field(&val, "backup", "keep_daily"),
        backup_keep_weekly: toml_u32_field(&val, "backup", "keep_weekly"),
        security_session_web_secs: toml_u64_field(&val, "security", "session_lifetime_web_secs"),
        security_session_mesh_secs: toml_u64_field(&val, "security", "session_lifetime_mesh_secs"),
        security_login_rate_per_min: toml_u32_field(&val, "security", "login_rate_per_min"),
        security_command_rate_per_min: toml_u32_field(&val, "security", "command_rate_per_min"),
        logging_level: toml_str_field(&val, "logging", "level"),
    };

    Json(resp).into_response()
}

async fn api_patch_config(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
    Json(patch): Json<ConfigPatch>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }

    let path = match &state.config.config_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json_error(
                    "config_path not set in [plugins.web] — cannot write config",
                )),
            )
                .into_response()
        }
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json_error(&format!("could not read config file: {e}"))),
            )
                .into_response()
        }
    };
    let mut doc = match raw.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json_error(&format!("could not parse config file: {e}"))),
            )
                .into_response()
        }
    };

    // Validate logging level before mutating anything.
    if let Some(ref level) = patch.logging_level {
        match level.to_ascii_uppercase().as_str() {
            "TRACE" | "DEBUG" | "INFO" | "WARN" | "ERROR" => {}
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json_error(
                        "logging_level must be one of TRACE, DEBUG, INFO, WARN, ERROR",
                    )),
                )
                    .into_response();
            }
        }
    }

    // Apply patches — only touch keys explicitly present in the request.
    if let Some(v) = patch.bbs_name {
        doc["bbs"]["name"] = toml_edit::value(v);
    }
    if let Some(v) = patch.bbs_starting_room {
        doc["bbs"]["starting_room"] = toml_edit::value(v);
    }
    if let Some(v) = patch.bbs_welcome_msg {
        doc["bbs"]["welcome_msg"] = toml_edit::value(v);
    }
    if let Some(v) = patch.bbs_timezone {
        doc["bbs"]["timezone"] = toml_edit::value(v);
    }
    // Latitude/longitude: JSON null removes the key; a number sets it.
    let location_touched = patch.location_latitude.is_some() || patch.location_longitude.is_some();
    if let Some(v) = patch.location_latitude {
        if v.is_null() {
            doc_remove_key(&mut doc, "location", "latitude");
        } else if let Some(f) = v.as_f64() {
            doc["location"]["latitude"] = toml_edit::value(f);
        }
    }
    if let Some(v) = patch.location_longitude {
        if v.is_null() {
            doc_remove_key(&mut doc, "location", "longitude");
        } else if let Some(f) = v.as_f64() {
            doc["location"]["longitude"] = toml_edit::value(f);
        }
    }
    if let Some(v) = patch.backup_enabled {
        doc["backup"]["enabled"] = toml_edit::value(v);
    }
    if let Some(v) = patch.backup_interval_hours {
        doc["backup"]["interval_hours"] = toml_edit::value(v as i64);
    }
    if let Some(v) = patch.backup_keep_daily {
        doc["backup"]["keep_daily"] = toml_edit::value(v as i64);
    }
    if let Some(v) = patch.backup_keep_weekly {
        doc["backup"]["keep_weekly"] = toml_edit::value(v as i64);
    }
    if let Some(v) = patch.security_session_web_secs {
        doc["security"]["session_lifetime_web_secs"] = toml_edit::value(v as i64);
    }
    if let Some(v) = patch.security_session_mesh_secs {
        doc["security"]["session_lifetime_mesh_secs"] = toml_edit::value(v as i64);
    }
    if let Some(v) = patch.security_login_rate_per_min {
        doc["security"]["login_rate_per_min"] = toml_edit::value(v as i64);
    }
    if let Some(v) = patch.security_command_rate_per_min {
        doc["security"]["command_rate_per_min"] = toml_edit::value(v as i64);
    }
    let logging_level_changed = patch.logging_level.is_some();
    if let Some(v) = patch.logging_level {
        doc["logging"]["level"] = toml_edit::value(v.to_ascii_uppercase());
    }

    if let Err(e) = std::fs::write(&path, doc.to_string()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json_error(&format!("could not write config file: {e}"))),
        )
            .into_response();
    }

    // Apply log level change immediately without a restart.
    if logging_level_changed {
        if let Some(level) = doc
            .get("logging")
            .and_then(|s| s.get("level"))
            .and_then(|v| v.as_str())
        {
            if let Some(reload) = state
                .log_reload
                .lock()
                .expect("log_reload poisoned")
                .as_ref()
            {
                if let Err(e) = reload(level) {
                    warn!("log level reload failed: {e}");
                } else {
                    info!(level, "log level changed at runtime");
                }
            }
        }
    }

    // Update in-memory GPS location so the mesh transport picks it up on next
    // reconnect without a restart.
    if location_touched {
        let new_location = match (
            doc.get("location")
                .and_then(|s| s.get("latitude"))
                .and_then(|v| v.as_float()),
            doc.get("location")
                .and_then(|s| s.get("longitude"))
                .and_then(|v| v.as_float()),
        ) {
            (Some(lat), Some(lon)) => Some((lat, lon)),
            _ => None,
        };
        state.host.set_node_location(new_location);
    }

    // Audit log — best-effort.
    let _ = state
        .host
        .admin_write_audit(
            &format!("web:{}", user.username),
            "config_change",
            None,
            None,
        )
        .await;

    Json(serde_json::json!({
        "ok": true,
        "message": "Config saved. Log level takes effect immediately; other changes require a restart."
    }))
    .into_response()
}

// ── Access policy ─────────────────────────────────────────────────────────────

/// Response body for `GET /api/v1/access-policy`.
#[derive(Debug, Serialize)]
struct AccessPolicyResponse {
    require_verify: bool,
    guest_room: Option<String>,
    guest_room_id: Option<i64>,
}

/// Patch body for `PATCH /api/v1/access-policy`.
/// Only fields explicitly included in the request are changed.
#[derive(Debug, Deserialize)]
struct AccessPolicyPatch {
    /// When present, sets `require_verify`.
    require_verify: Option<bool>,
    /// When present, sets the guest-room name.
    /// Send `null` (JSON null) to disable the guest room.
    guest_room: Option<serde_json::Value>,
}

async fn api_get_access_policy(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }
    match state.host.admin_get_access_policy().await {
        Ok(p) => Json(AccessPolicyResponse {
            require_verify: p.require_verify,
            guest_room: p.guest_room,
            guest_room_id: p.guest_room_id,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json_error(&format!("{e}"))),
        )
            .into_response(),
    }
}

async fn api_patch_access_policy(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
    Json(patch): Json<AccessPolicyPatch>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }

    if let Some(rv) = patch.require_verify {
        if let Err(e) = state.host.admin_set_require_verify(rv).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json_error(&format!("set require_verify: {e}"))),
            )
                .into_response();
        }
    }

    if let Some(gr) = patch.guest_room {
        let name: Option<String> = if gr.is_null() {
            None
        } else if let Some(s) = gr.as_str() {
            Some(s.to_owned())
        } else {
            return (
                StatusCode::BAD_REQUEST,
                Json(json_error("guest_room must be a string or null")),
            )
                .into_response();
        };
        if let Err(e) = state.host.admin_set_guest_room(name).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json_error(&format!("set guest_room: {e}"))),
            )
                .into_response();
        }
    }

    // Audit — best-effort.
    let _ = state
        .host
        .admin_write_audit(
            &format!("web:{}", user.username),
            "access_policy_change",
            None,
            None,
        )
        .await;

    // Return the updated policy so the frontend can refresh without a second GET.
    match state.host.admin_get_access_policy().await {
        Ok(p) => Json(AccessPolicyResponse {
            require_verify: p.require_verify,
            guest_room: p.guest_room,
            guest_room_id: p.guest_room_id,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json_error(&format!("{e}"))),
        )
            .into_response(),
    }
}

// ── Radio config ──────────────────────────────────────────────────────────────

async fn api_get_radio_config(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }

    let path = match &state.config.config_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json_error(
                    "config_path not set in [plugins.web] — cannot read config",
                )),
            )
                .into_response()
        }
    };

    let val = match read_config_toml(&path) {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json_error(&e))).into_response(),
    };

    let connection_type = toml_plugin_str(&val, "mesh", "connection_type");
    let serial_port = toml_plugin_str(&val, "mesh", "serial_port");

    Json(RadioConfigResponse {
        preset: toml_radio_str(&val, "preset"),
        frequency_hz: toml_radio_u64(&val, "frequency_hz"),
        bandwidth_hz: toml_radio_u32(&val, "bandwidth_hz"),
        spreading_factor: toml_radio_u32(&val, "spreading_factor").map(|v| v as u8),
        coding_rate: toml_radio_u32(&val, "coding_rate").map(|v| v as u8),
        tx_power_dbm: toml_radio_i32(&val, "tx_power_dbm"),
        connection_type,
        serial_port,
        presets: RADIO_PRESETS.to_vec(),
    })
    .into_response()
}

async fn api_patch_radio_config(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
    Json(patch): Json<RadioConfigPatch>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }

    let path = match &state.config.config_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            return (
                StatusCode::NOT_FOUND,
                Json(json_error(
                    "config_path not set in [plugins.web] — cannot write config",
                )),
            )
                .into_response()
        }
    };

    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json_error(&format!("could not read config file: {e}"))),
            )
                .into_response()
        }
    };
    let mut doc = match raw.parse::<toml_edit::DocumentMut>() {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json_error(&format!("could not parse config file: {e}"))),
            )
                .into_response()
        }
    };

    // Apply patches — null clears the key; a value sets it.
    if let Some(v) = patch.preset {
        if v.is_null() {
            doc_remove_radio_field(&mut doc, "preset");
        } else if let Some(s) = v.as_str() {
            doc_set_radio_field(&mut doc, "preset", toml_edit::Value::from(s));
        }
    }
    if let Some(v) = patch.frequency_hz {
        if v.is_null() {
            doc_remove_radio_field(&mut doc, "frequency_hz");
        } else if let Some(n) = v.as_u64() {
            doc_set_radio_field(&mut doc, "frequency_hz", toml_edit::Value::from(n as i64));
        }
    }
    if let Some(v) = patch.bandwidth_hz {
        if v.is_null() {
            doc_remove_radio_field(&mut doc, "bandwidth_hz");
        } else if let Some(n) = v.as_u64() {
            doc_set_radio_field(&mut doc, "bandwidth_hz", toml_edit::Value::from(n as i64));
        }
    }
    if let Some(v) = patch.spreading_factor {
        if v.is_null() {
            doc_remove_radio_field(&mut doc, "spreading_factor");
        } else if let Some(n) = v.as_u64() {
            doc_set_radio_field(
                &mut doc,
                "spreading_factor",
                toml_edit::Value::from(n as i64),
            );
        }
    }
    if let Some(v) = patch.coding_rate {
        if v.is_null() {
            doc_remove_radio_field(&mut doc, "coding_rate");
        } else if let Some(n) = v.as_u64() {
            doc_set_radio_field(&mut doc, "coding_rate", toml_edit::Value::from(n as i64));
        }
    }
    if let Some(v) = patch.tx_power_dbm {
        if v.is_null() {
            doc_remove_radio_field(&mut doc, "tx_power_dbm");
        } else if let Some(n) = v.as_i64() {
            doc_set_radio_field(&mut doc, "tx_power_dbm", toml_edit::Value::from(n));
        }
    }

    if let Err(e) = std::fs::write(&path, doc.to_string()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json_error(&format!("could not write config file: {e}"))),
        )
            .into_response();
    }

    // Audit log — best-effort.
    let _ = state
        .host
        .admin_write_audit(
            &format!("web:{}", user.username),
            "radio_config_change",
            None,
            None,
        )
        .await;

    // Return updated config.
    let val = match read_config_toml(&path) {
        Ok(v) => v,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json_error(&e))).into_response(),
    };
    Json(RadioConfigResponse {
        preset: toml_radio_str(&val, "preset"),
        frequency_hz: toml_radio_u64(&val, "frequency_hz"),
        bandwidth_hz: toml_radio_u32(&val, "bandwidth_hz"),
        spreading_factor: toml_radio_u32(&val, "spreading_factor").map(|v| v as u8),
        coding_rate: toml_radio_u32(&val, "coding_rate").map(|v| v as u8),
        tx_power_dbm: toml_radio_i32(&val, "tx_power_dbm"),
        connection_type: toml_plugin_str(&val, "mesh", "connection_type"),
        serial_port: toml_plugin_str(&val, "mesh", "serial_port"),
        presets: RADIO_PRESETS.to_vec(),
    })
    .into_response()
}

// ── Node identity ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct NodeIdentityResponse {
    /// Current node public key hex (64 chars), or null if not yet connected.
    pubkey: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImportKeyBody {
    /// 64-char hex private key.
    key: String,
}

async fn api_get_node_identity(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }
    Json(NodeIdentityResponse {
        pubkey: state.host.node_pubkey(),
    })
    .into_response()
}

async fn api_export_node_key(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }
    match state.host.admin_export_node_key().await {
        Ok(hex) => Json(serde_json::json!({ "key": hex })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json_error(&format!("{e}"))),
        )
            .into_response(),
    }
}

async fn api_import_node_key(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<ImportKeyBody>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }
    match state.host.admin_import_node_key(body.key).await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json_error(&format!("{e}")))).into_response(),
    }
}

// ── HTTP log poll ─────────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct LogsQuery {
    after: Option<u64>,
}

#[derive(serde::Serialize)]
struct LogsResponse {
    /// Next cursor value to send as `?after=N` on the following request.
    cursor: u64,
    lines: Vec<String>,
}

async fn api_logs(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LogsQuery>,
) -> impl IntoResponse {
    let after = q.after.unwrap_or(0);
    // Prefer the application log buffer (tracing events + BBS events) when
    // available; fall back to the BBS-only domain event buffer.
    let (cursor, lines) =
        if let Some(ref buf) = *state.ext_log_buf.lock().expect("ext_log_buf poisoned") {
            buf.lock().expect("ext_log_buf inner poisoned").since(after)
        } else {
            state.log_buf.lock().expect("log_buf poisoned").since(after)
        };
    Json(LogsResponse { cursor, lines })
}

// ── SSE log stream ────────────────────────────────────────────────────────────

async fn api_sse_logs(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.log_tx.subscribe();

    // Prepend a one-shot "[system] connected" event so the client can
    // immediately verify the stream is delivering data (not just keeping
    // the connection alive with empty comments).
    let connect_msg = Ok(Event::default().data("[system] log stream connected"));
    let init = tokio_stream::once(connect_msg);

    let live = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(line) => Some(Ok(Event::default().data(line))),
        Err(_lagged) => None,
    });

    Sse::new(tokio_stream::StreamExt::chain(init, live))
        .keep_alive(axum::response::sse::KeepAlive::default())
}

// ── SSE domain events ─────────────────────────────────────────────────────────

async fn api_sse_events(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.host.events();

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let kind = match &event {
                DomainEvent::UserCreated { .. } => Some("user_created"),
                DomainEvent::UserValidated { .. } => Some("user_validated"),
                _ => None,
            };
            kind.map(|k| Ok(Event::default().event(k).data("{}")))
        }
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default())
}

// ── Backups ───────────────────────────────────────────────────────────────────

async fn api_trigger_backup(State(state): State<Arc<AppState>>) -> Response {
    use std::io::Write as _;
    use zip::{write::SimpleFileOptions, CompressionMethod};

    let dir = match state.backup_dir() {
        Some(d) => d,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json_error("backup_dir not configured")),
            )
                .into_response()
        }
    };

    // Step 1: VACUUM INTO a temporary .db file.
    let record = match state.host.admin_trigger_backup(&dir).await {
        Ok(r) => r,
        Err(e) => return server_error(&e.to_string()),
    };

    // Step 2: Bundle the .db (and config if available) into a single .zip.
    let db_path = std::path::Path::new(&dir).join(&record.filename);
    let zip_name = record.filename.trim_end_matches(".db").to_owned() + ".zip";
    let zip_path = std::path::Path::new(&dir).join(&zip_name);
    let config_path_opt = state.config.config_path.clone();
    let db_entry_name = record.filename.clone();

    let zip_result = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        let file = std::fs::File::create(&zip_path)?;
        let mut zip = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        // Add database.
        zip.start_file(&db_entry_name, opts)?;
        zip.write_all(&std::fs::read(&db_path)?)?;

        // Add config (best-effort — log a warning if the path doesn't exist).
        if let Some(ref cfg) = config_path_opt {
            if !cfg.is_empty() {
                match std::fs::read(cfg) {
                    Ok(bytes) => {
                        zip.start_file("config.toml", opts)?;
                        zip.write_all(&bytes)?;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "backup: could not include config file '{}': {} \
                             — set config_path in [plugins.web] to the full \
                             path of your config.toml",
                            cfg,
                            e
                        );
                    }
                }
            }
        }

        zip.finish()?;

        // Remove the raw .db now that it is inside the zip.
        let _ = std::fs::remove_file(&db_path);

        Ok(std::fs::metadata(&zip_path)?.len())
    })
    .await;

    match zip_result {
        Ok(Ok(zip_size)) => {
            let zip_record = AdminBackupRecord {
                filename: zip_name,
                size_bytes: zip_size,
                created_at: record.created_at,
                config_filename: None,
                config_size_bytes: None,
            };
            (StatusCode::CREATED, Json(zip_record)).into_response()
        }
        Ok(Err(e)) => server_error(&e.to_string()),
        Err(e) => server_error(&e.to_string()),
    }
}

async fn api_list_backups(State(state): State<Arc<AppState>>) -> Response {
    let dir = match state.backup_dir() {
        Some(d) => d,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json_error("backup_dir not configured")),
            )
                .into_response()
        }
    };
    match state.host.admin_list_backups(&dir).await {
        Ok(records) => Json(records).into_response(),
        Err(e) => server_error(&e.to_string()),
    }
}

async fn api_download_backup(
    State(state): State<Arc<AppState>>,
    Path(filename): Path<String>,
) -> Response {
    // Path traversal protection.
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json_error("invalid filename")),
        )
            .into_response();
    }

    let dir = match state.backup_dir() {
        Some(d) => d,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json_error("backup_dir not configured")),
            )
                .into_response()
        }
    };

    let path = std::path::Path::new(&dir).join(&filename);
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{filename}\""),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (StatusCode::NOT_FOUND, Json(json_error("file not found"))).into_response()
        }
        Err(e) => server_error(&e.to_string()),
    }
}

async fn api_delete_backup(
    State(state): State<Arc<AppState>>,
    Path(filename): Path<String>,
) -> Response {
    let dir = match state.backup_dir() {
        Some(d) => d,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json_error("backup_dir not configured")),
            )
                .into_response()
        }
    };

    match state.host.admin_delete_backup(&dir, &filename).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(HostError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, Json(json_error("not found"))).into_response()
        }
        Err(e) => server_error(&e.to_string()),
    }
}

// ── Domain event formatting ───────────────────────────────────────────────────

fn format_domain_event(event: &DomainEvent) -> String {
    match event {
        DomainEvent::SessionCreated { session, transport } => {
            format!("[session] #{} created via {transport}", session.as_u64())
        }
        DomainEvent::SessionAuthenticated { session, user } => {
            format!("[auth] #{} authenticated as {user}", session.as_u64())
        }
        DomainEvent::SessionEnded { session, reason } => {
            format!("[session] #{} ended: {reason}", session.as_u64())
        }
        DomainEvent::MessagePosted {
            sender,
            recipient,
            message_id,
        } => {
            let dest = match recipient {
                MessageRecipient::Room(r) => format!("#{r}"),
                MessageRecipient::Direct(u) => format!("@{u}"),
                _ => "?".to_owned(),
            };
            format!("[msg] #{message_id} from {sender} to {dest}")
        }
        DomainEvent::UserCreated { user } => format!("[user] {user} registered"),
        DomainEvent::UserValidated { user } => format!("[user] {user} validated"),
        DomainEvent::CommandExecuted {
            session,
            command,
            user,
        } => {
            let who = user
                .as_ref()
                .map(|u| u.as_str().to_owned())
                .unwrap_or_else(|| format!("#{}", session.as_u64()));
            format!("[cmd] {who} → {command}")
        }
        _ => format!("[event] {event:?}"),
    }
}

// ── SPA fallback ──────────────────────────────────────────────────────────────

async fn spa_handler(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(asset) = StaticFiles::get(path) {
        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();
        return ([(header::CONTENT_TYPE, mime)], asset.data).into_response();
    }

    match StaticFiles::get("index.html") {
        Some(index) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            index.data,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            "web admin not built — run `npm run build` in crates/bbs-web/web/",
        )
            .into_response(),
    }
}

// ── Error helpers ─────────────────────────────────────────────────────────────

fn json_error(msg: &str) -> serde_json::Value {
    serde_json::json!({ "error": { "message": msg } })
}

fn server_error(internal_msg: &str) -> Response {
    warn!("admin API internal error: {internal_msg}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json_error("internal server error")),
    )
        .into_response()
}

// ── Plugin registry helpers ───────────────────────────────────────────────────

fn registry_unavailable() -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json_error("process plugin registry not available")),
    )
        .into_response()
}

fn registry_err(e: RegistryError) -> Response {
    let status = match &e {
        RegistryError::NotFound(_) => StatusCode::NOT_FOUND,
        RegistryError::AlreadyExists(_) => StatusCode::CONFLICT,
        RegistryError::NotRunning(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(json_error(&e.to_string()))).into_response()
}

// ── Plugin handlers ───────────────────────────────────────────────────────────

async fn api_list_plugins(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }
    let registry = { state.plugin_registry.lock().expect("poisoned").clone() };
    let Some(registry) = registry else {
        return registry_unavailable();
    };
    Json(registry.list_plugins().await).into_response()
}

#[derive(Deserialize)]
struct AddPluginBody {
    name: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_true")]
    restart_on_crash: bool,
    #[serde(default = "default_restart_delay")]
    restart_delay_secs: u64,
}

fn default_true() -> bool {
    true
}
fn default_restart_delay() -> u64 {
    5
}

async fn api_add_plugin(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
    Json(body): Json<AddPluginBody>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }
    let registry = { state.plugin_registry.lock().expect("poisoned").clone() };
    let Some(registry) = registry else {
        return registry_unavailable();
    };
    let cfg = ProcessPluginConfig {
        name: body.name,
        command: body.command,
        args: body.args,
        enabled: body.enabled,
        restart_on_crash: body.restart_on_crash,
        restart_delay_secs: body.restart_delay_secs,
    };
    match registry.add_plugin(cfg).await {
        Ok(()) => {
            let _ = state
                .host
                .admin_write_audit(
                    &format!("web:{}", user.username),
                    "add_plugin",
                    Some(
                        &registry
                            .list_plugins()
                            .await
                            .last()
                            .map(|p| p.name.clone())
                            .unwrap_or_default(),
                    ),
                    None,
                )
                .await;
            (StatusCode::CREATED, Json(serde_json::json!({ "ok": true }))).into_response()
        }
        Err(e) => registry_err(e),
    }
}

async fn api_remove_plugin(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
    Path(name): Path<String>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }
    let registry = { state.plugin_registry.lock().expect("poisoned").clone() };
    let Some(registry) = registry else {
        return registry_unavailable();
    };
    match registry.remove_plugin(&name).await {
        Ok(()) => {
            let _ = state
                .host
                .admin_write_audit(
                    &format!("web:{}", user.username),
                    "remove_plugin",
                    Some(&name),
                    None,
                )
                .await;
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(e) => registry_err(e),
    }
}

#[derive(Deserialize)]
struct UpdatePluginBody {
    enabled: Option<bool>,
}

async fn api_update_plugin(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
    Path(name): Path<String>,
    Json(body): Json<UpdatePluginBody>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }
    let registry = { state.plugin_registry.lock().expect("poisoned").clone() };
    let Some(registry) = registry else {
        return registry_unavailable();
    };
    if let Some(enabled) = body.enabled {
        if let Err(e) = registry.set_enabled(&name, enabled).await {
            return registry_err(e);
        }
        let _ = state
            .host
            .admin_write_audit(
                &format!("web:{}", user.username),
                if enabled {
                    "enable_plugin"
                } else {
                    "disable_plugin"
                },
                Some(&name),
                None,
            )
            .await;
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

async fn api_restart_plugin(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
    Path(name): Path<String>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }
    let registry = { state.plugin_registry.lock().expect("poisoned").clone() };
    let Some(registry) = registry else {
        return registry_unavailable();
    };
    match registry.restart_plugin(&name).await {
        Ok(()) => {
            let _ = state
                .host
                .admin_write_audit(
                    &format!("web:{}", user.username),
                    "restart_plugin",
                    Some(&name),
                    None,
                )
                .await;
            Json(serde_json::json!({ "ok": true })).into_response()
        }
        Err(e) => registry_err(e),
    }
}

#[derive(Deserialize)]
struct PluginLogsQuery {
    #[serde(default = "default_log_lines")]
    lines: usize,
}

fn default_log_lines() -> usize {
    50
}

async fn api_plugin_logs(
    State(state): State<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
    Path(name): Path<String>,
    Query(q): Query<PluginLogsQuery>,
) -> Response {
    if user.permission_level < 100 {
        return (StatusCode::FORBIDDEN, Json(json_error("sysop required"))).into_response();
    }
    let registry = { state.plugin_registry.lock().expect("poisoned").clone() };
    let Some(registry) = registry else {
        return registry_unavailable();
    };
    match registry.get_logs(&name, q.lines.min(500)).await {
        Ok(lines) => Json(serde_json::json!({ "lines": lines })).into_response(),
        Err(e) => registry_err(e),
    }
}
