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
//! │  │  GET  /api/v1/auth/whoami                       │    │
//! │  │  POST /api/v1/auth/login                        │    │
//! │  │  POST /api/v1/auth/logout                       │    │
//! │  │  GET  /api/v1/status                            │    │
//! │  │  GET  /api/v1/adverts   → host.advert_bus()     │    │
//! │  │  POST /api/v1/adverts/send                      │    │
//! │  │  GET  /*               → rust-embed SPA         │    │
//! │  └─────────────────────────────────────────────────┘    │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Auth
//!
//! The web admin uses its own session system independent of BBS user sessions.
//! A single admin account is configured via `[plugins.web] admin_password`.
//! Sessions are in-memory UUIDs stored in an HttpOnly cookie.
//!
//! [ADR-0003]: https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0003-web-ui-as-plugin.md

#![allow(missing_docs)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::CookieJar;
use bbs_plugin_api::error::PluginError;
use bbs_plugin_api::host::Host;
use bbs_plugin_api::plugin::Plugin;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::watch;
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
#[serde(deny_unknown_fields)]
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

    /// Admin password for the web UI. **Change this before deploying.**
    ///
    /// A warning is logged at startup if this is still the default.
    #[serde(default = "default_admin_password")]
    pub admin_password: String,
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
            admin_password: default_admin_password(),
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
fn default_admin_password() -> String {
    "changeme".to_owned()
}

// ── Web session store ─────────────────────────────────────────────────────────

const SESSION_COOKIE: &str = "bbs_web_session";
const SESSION_TTL_SECS: u64 = 12 * 60 * 60; // 12 h

#[derive(Debug)]
struct WebSession {
    expires_at: Instant,
}

// ── Shared state ──────────────────────────────────────────────────────────────

struct AppState {
    host: Arc<dyn Host>,
    config: WebConfig,
    sessions: Mutex<HashMap<String, WebSession>>,
    started_at: Instant,
}

impl AppState {
    fn new(host: Arc<dyn Host>, config: WebConfig) -> Self {
        Self {
            host,
            config,
            sessions: Mutex::new(HashMap::new()),
            started_at: Instant::now(),
        }
    }

    fn create_session(&self) -> String {
        let token = Uuid::new_v4().to_string();
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        sessions.insert(
            token.clone(),
            WebSession {
                expires_at: Instant::now() + std::time::Duration::from_secs(SESSION_TTL_SECS),
            },
        );
        token
    }

    fn validate_session(&self, token: &str) -> bool {
        let mut sessions = self.sessions.lock().expect("sessions poisoned");
        match sessions.get(token) {
            Some(s) if s.expires_at > Instant::now() => true,
            _ => {
                sessions.remove(token);
                false
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

        if config.admin_password == "changeme" {
            warn!(
                "web admin password is still the default 'changeme'. \
                 Set [plugins.web] admin_password in your config."
            );
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
    req: Request,
    next: Next,
) -> Response {
    let token = jar
        .get(SESSION_COOKIE)
        .map(|c| c.value().to_owned())
        .unwrap_or_default();

    if state.validate_session(&token) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json_error("not authenticated")),
        )
            .into_response()
    }
}

// ── API handlers ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct WhoamiResponse {
    username: &'static str,
    is_sysop: bool,
    permission_level: u8,
}

async fn api_whoami() -> impl IntoResponse {
    Json(WhoamiResponse {
        username: "admin",
        is_sysop: true,
        permission_level: 4,
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
    let valid = body.username == "admin" && body.password == state.config.admin_password;
    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json_error("invalid credentials")),
        )
            .into_response();
    }

    let token = state.create_session();
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
            username: "admin".into(),
            permission_level: 4,
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

// ── SPA fallback ──────────────────────────────────────────────────────────────

async fn spa_handler(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try exact asset first, then fall through to index.html.
    if let Some(asset) = StaticFiles::get(path) {
        let mime = mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string();
        return ([(header::CONTENT_TYPE, mime)], asset.data).into_response();
    }

    // SPA catch-all: serve index.html for any unknown path.
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
