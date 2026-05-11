//! Wire protocol between Supply Drop and a process transport plugin.
//!
//! Both directions carry **newline-delimited JSON** (one object per line).
//! The `t` field is the message type discriminator.
//!
//! ## Plugin → Host (plugin's stdout)
//!
//! | `t`       | Fields                        | Meaning                              |
//! |-----------|-------------------------------|--------------------------------------|
//! | `ready`   | `payload_limit?`              | Plugin initialised, ready to accept  |
//! | `open`    | `id`                          | New connection established           |
//! | `recv`    | `id`, `line`                  | User sent a line of text             |
//! | `close`   | `id`                          | Connection closed by remote end      |
//!
//! ## Host → Plugin (plugin's stdin)
//!
//! | `t`        | Fields                       | Meaning                              |
//! |------------|------------------------------|--------------------------------------|
//! | `send`     | `id`, `text`, `hide_input?`  | Deliver text to user                 |
//! | `kick`     | `id`                         | Force-close a connection             |
//! | `shutdown` | —                            | Graceful exit request                |

use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    // ── PluginMsg round-trips ─────────────────────────────────────────────────

    #[test]
    fn plugin_msg_ready_with_limit() {
        let msg = PluginMsg::Ready { payload_limit: 156 };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.contains(r#""t":"ready""#),
            "discriminator missing: {json}"
        );
        assert!(json.contains("156"), "payload_limit missing: {json}");
        let back: PluginMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, PluginMsg::Ready { payload_limit: 156 }));
    }

    #[test]
    fn plugin_msg_ready_default_limit_when_field_absent() {
        // Omitting payload_limit should deserialise to 0 (the #[serde(default)]).
        let json = r#"{"t":"ready"}"#;
        let msg: PluginMsg = serde_json::from_str(json).unwrap();
        assert!(matches!(msg, PluginMsg::Ready { payload_limit: 0 }));
    }

    #[test]
    fn plugin_msg_open_roundtrip() {
        let msg = PluginMsg::Open {
            id: "tcp:127.0.0.1:4242:1".to_owned(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""t":"open""#));
        let back: PluginMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, PluginMsg::Open { ref id } if id == "tcp:127.0.0.1:4242:1"));
    }

    #[test]
    fn plugin_msg_recv_roundtrip() {
        let msg = PluginMsg::Recv {
            id: "conn-42".to_owned(),
            line: "login alice".to_owned(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""t":"recv""#));
        let back: PluginMsg = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(&back, PluginMsg::Recv { id, line } if id == "conn-42" && line == "login alice")
        );
    }

    #[test]
    fn plugin_msg_close_roundtrip() {
        let msg = PluginMsg::Close {
            id: "c99".to_owned(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""t":"close""#));
        let back: PluginMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(&back, PluginMsg::Close { id } if id == "c99"));
    }

    // ── HostMsg round-trips ───────────────────────────────────────────────────

    #[test]
    fn host_msg_send_roundtrip() {
        let msg = HostMsg::Send {
            id: "c1".to_owned(),
            text: "Welcome to the BBS.".to_owned(),
            hide_input: false,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""t":"send""#));
        let back: HostMsg = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(&back, HostMsg::Send { id, text, hide_input: false }
                if id == "c1" && text == "Welcome to the BBS.")
        );
    }

    #[test]
    fn host_msg_send_hide_input_roundtrip() {
        let msg = HostMsg::Send {
            id: "c2".to_owned(),
            text: "Password: ".to_owned(),
            hide_input: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        // hide_input must be present and true when set.
        assert!(json.contains("true"), "hide_input:true missing: {json}");
        let back: HostMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            &back,
            HostMsg::Send {
                hide_input: true,
                ..
            }
        ));
    }

    #[test]
    fn host_msg_send_hide_input_defaults_false_when_absent() {
        // Omitting hide_input should deserialise to false.
        let json = r#"{"t":"send","id":"c1","text":"hi"}"#;
        let msg: HostMsg = serde_json::from_str(json).unwrap();
        assert!(matches!(
            msg,
            HostMsg::Send {
                hide_input: false,
                ..
            }
        ));
    }

    #[test]
    fn host_msg_kick_roundtrip() {
        let msg = HostMsg::Kick {
            id: "bad-conn".to_owned(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""t":"kick""#));
        let back: HostMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(&back, HostMsg::Kick { id } if id == "bad-conn"));
    }

    #[test]
    fn host_msg_shutdown_roundtrip() {
        let msg = HostMsg::Shutdown;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""t":"shutdown""#), "unexpected: {json}");
        let back: HostMsg = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, HostMsg::Shutdown));
    }

    #[test]
    fn unknown_t_field_returns_error() {
        let json = r#"{"t":"teleport","id":"x"}"#;
        assert!(
            serde_json::from_str::<PluginMsg>(json).is_err(),
            "expected error for unknown message type"
        );
    }
}

/// A message from the plugin process to Supply Drop (read from the plugin's stdout).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum PluginMsg {
    /// The plugin has initialised and is ready to accept connections.
    Ready {
        /// Maximum bytes per response text frame.
        /// `0` means no limit (CLI-style transports).
        /// Supply Drop truncates `MultiText` frames that exceed this limit.
        #[serde(default)]
        payload_limit: usize,
    },

    /// A new user connection has been established.
    Open {
        /// Plugin-assigned connection identifier. Must be unique within
        /// this plugin instance for the lifetime of the connection.
        /// Can be anything useful to the plugin (socket addr, node key, etc.).
        id: String,
    },

    /// A line of text was received from a user on an open connection.
    Recv {
        /// Connection ID (matches a preceding `Open` message).
        id: String,
        /// Raw text the user sent. Supply Drop handles command parsing.
        line: String,
    },

    /// A connection was closed by the remote end.
    Close {
        /// Connection ID.
        id: String,
    },
}

/// A message from Supply Drop to the plugin process (written to the plugin's stdin).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostMsg {
    /// Deliver a text frame to a user.
    Send {
        /// Connection ID.
        id: String,
        /// Text to display to the user.
        text: String,
        /// When `true`, the user's next input should be visually masked
        /// (password entry). Transports that don't support input masking
        /// may ignore this.
        #[serde(default)]
        hide_input: bool,
    },

    /// Force-close a connection.
    Kick {
        /// Connection ID.
        id: String,
    },

    /// Graceful shutdown request. The plugin should stop accepting new
    /// connections, close all existing ones, flush output, and exit.
    Shutdown,
}
