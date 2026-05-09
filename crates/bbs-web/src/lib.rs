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
//! │  │  GET  /api/v1/sse/logs             (auth)       │    │
//! │  │  POST /api/v1/backups              (auth)       │    │
//! │  │  GET  /api/v1/backups              (auth)       │    │
//! │  │  GET  /api/v1/backups/:filename    (auth)       │    │
//! │  │  DELETE /api/v1/backups/:filename  (auth)       │    │
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

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

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

    /// Directory to store SQLite backup files created by the web admin.
    ///
    /// When `None`, the backup endpoints return 400 Bad Request.
    #[serde(default)]
    pub backup_dir: Option<String>,

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
            backup_dir: None,
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
const LOG_BUF_CAP: usize = 500;

// ── In-memory log ring buffer ─────────────────────────────────────────────────

/// A monotonically-sequenced ring buffer for recent log lines.
///
/// Each line gets a unique `seq` number. Clients send `?after=N` to receive
/// only lines with `seq >= N`, enabling efficient incremental polling.
struct LogBuffer {
    lines: std::collections::VecDeque<(u64, String)>,
    next_seq: u64,
}

impl LogBuffer {
    fn new() -> Self {
        Self {
            lines: std::collections::VecDeque::new(),
            next_seq: 0,
        }
    }

    fn push(&mut self, text: String) {
        self.lines.push_back((self.next_seq, text));
        self.next_seq += 1;
        while self.lines.len() > LOG_BUF_CAP {
            self.lines.pop_front();
        }
    }

    /// Return all lines with `seq >= after` and the next cursor value.
    fn since(&self, after: u64) -> (u64, Vec<String>) {
        let lines = self
            .lines
            .iter()
            .filter(|(seq, _)| *seq >= after)
            .map(|(_, text)| text.clone())
            .collect();
        (self.next_seq, lines)
    }
}

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

// ── Shared state ──────────────────────────────────────────────────────────────

struct AppState {
    host: Arc<dyn Host>,
    config: WebConfig,
    sessions: Mutex<HashMap<String, WebSession>>,
    started_at: Instant,
    log_tx: broadcast::Sender<String>,
    log_buf: std::sync::Arc<Mutex<LogBuffer>>,
}

impl AppState {
    fn new(host: Arc<dyn Host>, config: WebConfig) -> Self {
        let (log_tx, _) = broadcast::channel(LOG_CHANNEL_CAP);
        Self {
            host,
            config,
            sessions: Mutex::new(HashMap::new()),
            started_at: Instant::now(),
            log_tx,
            log_buf: std::sync::Arc::new(Mutex::new(LogBuffer::new())),
        }
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
        let mut events = self.state.host.events();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(event) => {
                        let line = format_domain_event(&event);
                        log_buf.lock().expect("log_buf poisoned").push(line.clone());
                        let _ = log_tx.send(line);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        let warn = format!("[warn] event stream lagged by {n}");
                        log_buf.lock().expect("log_buf poisoned").push(warn.clone());
                        let _ = log_tx.send(warn);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

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

// ── Router ────────────────────────────────────────────────────────────────────

fn build_router(state: Arc<AppState>) -> Router {
    let protected_api = Router::new()
        .route("/auth/whoami", get(api_whoami))
        .route("/auth/logout", post(api_logout))
        .route("/status", get(api_status))
        .route("/adverts", get(api_adverts))
        .route("/adverts/send", post(api_adverts_send))
        .route("/sessions", get(api_list_sessions))
        .route("/users", get(api_list_users))
        .route("/users/:username", patch(api_update_user))
        .route("/rooms", get(api_list_rooms).post(api_create_room))
        .route("/rooms/:id", delete(api_delete_room))
        .route("/rooms/:id/messages", get(api_list_messages))
        .route("/messages/:id", delete(api_delete_message))
        .route("/audit-log", get(api_audit_log))
        .route("/stats", get(api_stats))
        .route("/reports", get(api_reports))
        .route("/settings", get(api_settings))
        .route("/logs", get(api_logs))
        .route("/sse/logs", get(api_sse_logs))
        .route("/backups", get(api_list_backups).post(api_trigger_backup))
        .route(
            "/backups/:filename",
            get(api_download_backup).delete(api_delete_backup),
        )
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            auth_middleware,
        ));

    let public_api = Router::new().route("/auth/login", post(api_login));

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
}

async fn api_update_user(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<CurrentUser>,
    Path(username): Path<String>,
    Json(body): Json<UpdateUserBody>,
) -> Response {
    if body.status.is_none() && body.permission_level.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json_error(
                "at least one of status or permission_level is required",
            )),
        )
            .into_response();
    }
    // Only Sysop (level 100) may change permission levels.
    if body.permission_level.is_some() && caller.permission_level < 100 {
        return (
            StatusCode::FORBIDDEN,
            Json(json_error("sysop required to change permission level")),
        )
            .into_response();
    }
    match state
        .host
        .admin_update_user(&username, body.status, body.permission_level)
        .await
    {
        Ok(()) => {
            let actor_str = format!("web:{}", caller.username);
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
            Json(serde_json::json!({"ok": true})).into_response()
        }
        Err(HostError::NotFound(_)) => {
            (StatusCode::NOT_FOUND, Json(json_error("user not found"))).into_response()
        }
        Err(e) => server_error(&e.to_string()),
    }
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

async fn api_delete_room(
    State(state): State<Arc<AppState>>,
    Extension(caller): Extension<CurrentUser>,
    Path(id): Path<i64>,
) -> Response {
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
    match state
        .host
        .admin_list_messages(room_id, q.limit, q.after_id)
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

// ── Settings ──────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct SettingsResponse {
    backup_dir: Option<String>,
}

async fn api_settings(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    Json(SettingsResponse {
        backup_dir: state.config.backup_dir.clone(),
    })
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
    let (cursor, lines) = state.log_buf.lock().expect("log_buf poisoned").since(after);
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

// ── Backups ───────────────────────────────────────────────────────────────────

async fn api_trigger_backup(State(state): State<Arc<AppState>>) -> Response {
    use std::io::Write as _;
    use zip::{write::SimpleFileOptions, CompressionMethod};

    let dir = match &state.config.backup_dir {
        Some(d) => d.clone(),
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
    let dir = match &state.config.backup_dir {
        Some(d) => d.clone(),
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

    let dir = match &state.config.backup_dir {
        Some(d) => d.clone(),
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
    let dir = match &state.config.backup_dir {
        Some(d) => d.clone(),
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
