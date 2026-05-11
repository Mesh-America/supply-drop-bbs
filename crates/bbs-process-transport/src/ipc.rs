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
