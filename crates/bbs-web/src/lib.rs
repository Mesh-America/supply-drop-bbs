//! # bbs-web
//!
//! The web admin UI plugin for Supply Drop BBS. Serves an HTTP admin
//! interface for the sysop. Default OFF — requires the `admin-web`
//! cargo feature.
//!
//! See [ADR-0003] for why this is a plugin.
//!
//! [ADR-0003]: https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0003-web-ui-as-plugin.md
//!
//! ## Status
//!
//! Placeholder. Real implementation lands in subsequent commits.

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

/// Configuration for the web admin plugin.
///
/// Deserialized from `[plugins.web]` in the operator's TOML config.
/// Only valid when the binary is compiled with `--features admin-web`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    /// Whether to start the web listener. Set `false` to disable at
    /// runtime without recompiling.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Address to bind. Defaults to `127.0.0.1:8080`.
    ///
    /// **Do not bind to `0.0.0.0` without also setting
    /// `external_origin`** — CSRF protection requires knowing the
    /// public origin.
    #[serde(default = "default_bind")]
    pub bind: String,

    /// Public origin URL for CSRF and cookie `SameSite` policy.
    ///
    /// Required when `bind` is anything other than a loopback address.
    /// Example: `"https://admin.bbs.example.com"`.
    #[serde(default)]
    pub external_origin: Option<String>,

    /// Set the `Secure` flag on session cookies. Default `true`.
    /// Only set `false` for purely local development (no TLS).
    #[serde(default = "default_cookie_secure")]
    pub cookie_secure: bool,

    /// Expose Prometheus metrics at `GET /metrics`. Default `false`.
    #[serde(default)]
    pub prometheus: bool,

    /// Override the Content-Security-Policy header. Defaults to a
    /// strict policy that disallows inline scripts and external resources.
    #[serde(default)]
    pub csp: Option<String>,
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
    true
}
