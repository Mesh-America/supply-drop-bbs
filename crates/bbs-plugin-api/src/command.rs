//! Protocol-neutral command and response.
//!
//! These are the abstract messages that flow between transports
//! and the host. A transport translates its protocol's wire format
//! to a [`Command`] before calling [`Host::process_command`](crate::Host::process_command)
//! and translates the resulting [`Response`] back to its wire
//! format.
//!
//! Both enums are intentionally `#[non_exhaustive]` — they grow
//! with feature work, and matchers must always handle the catch-all.
//! The variants here are **placeholders** for the v1 happy paths;
//! their final shape is being designed alongside the command
//! processor in `bbs-core`. See
//! [docs/PROTOCOL.md](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/PROTOCOL.md)
//! for the full eventual surface.
//!
//! ## Why this lives in `bbs-plugin-api` and not `bbs-core`
//!
//! Plugins (specifically transports) need to construct `Command`s
//! and inspect `Response`s. If these types lived in `bbs-core`,
//! every plugin would have to depend on `bbs-core` and we'd lose
//! some of the contract-only purity of `bbs-plugin-api`. Keeping
//! them here means transports can compile against just the plugin
//! API — leaner dependency graph, clearer boundaries.

use crate::identity::Username;
use serde::{Deserialize, Serialize};

/// A protocol-neutral command from a session to the BBS.
///
/// Transports parse their wire format into one of these variants
/// before calling the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Command {
    /// Show available commands or help on a specific topic.
    Help {
        /// Optional topic. None = top-level help.
        topic: Option<String>,
    },

    /// Begin (or continue) the registration workflow.
    Register {
        /// Desired username.
        username: Username,
    },

    /// Begin (or continue) the login workflow.
    Login {
        /// The username being logged into.
        username: Username,
    },

    /// End the session.
    Logout,

    /// Print the current session's identity and state.
    Whoami,

    /// Free-text input for the current workflow step. Used during
    /// registration / login challenges where the previous response
    /// asked an open-ended question.
    WorkflowReply {
        /// The user's reply to the previous prompt.
        reply: String,
    },

    /// A command the host doesn't recognise. Captured here so
    /// reports and audit logs can record it; the host's response
    /// is typically the help topic.
    Unknown {
        /// The raw command string the user sent.
        raw: String,
    },

    // ── Room navigation ───────────────────────────────────────────────
    /// List all accessible rooms with unread-message markers. (K)
    ListRooms,

    /// Jump to the next room that has unread messages. (G)
    GoNextUnread,

    /// Change to a room by name or numeric ID. (C)
    ChangeRoom {
        /// Room name or numeric ID string supplied by the user.
        target: String,
    },

    /// Navigate directly to the Mail room. (M)
    GoMail,

    // ── Message reading ───────────────────────────────────────────────
    /// Read unread messages in the current room (from the last-read
    /// pointer). (N)
    ReadNew,

    /// Browse messages oldest-first, optionally starting after a
    /// given message ID. (F)
    ReadForward {
        /// Start cursor; `None` means from the beginning.
        after: Option<i64>,
    },

    /// Browse the most recent messages newest-first. (R)
    ReadReverse,

    /// Show one-line message summaries (ID, sender, snippet). (S)
    ScanMessages,

    // ── Message posting / deletion ────────────────────────────────────
    /// Begin composing a message for the current room (or Mail). (E)
    EnterMessage,

    /// Delete a message by its numeric ID. (D)
    DeleteMessage {
        /// The numeric message ID to delete.
        id: i64,
    },

    // ── Session control ───────────────────────────────────────────────
    /// Quit — log out gracefully. (Q)
    Quit,

    /// Cancel the current workflow without logging out. (cancel)
    Cancel,
}

/// A protocol-neutral response from the BBS to a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Response {
    /// Plain text the transport renders as-is.
    Text(String),

    /// A response that's also a prompt — the next user input is
    /// the workflow's continuation. Transports typically render
    /// the text and then read the next message as a
    /// `WorkflowReply` command.
    Prompt {
        /// Text to display to the user.
        text: String,
        /// Whether the user's reply should be hidden in the UI
        /// (passwords).
        hide_input: bool,
    },

    /// The session is now logged in. Transports may want to
    /// trigger UI changes (banner, prompt) on this.
    LoggedIn {
        /// The user the session bound to.
        user: Username,
    },

    /// The session is now logged out. Some transports tear down
    /// the connection on this.
    LoggedOut,

    /// An error response. The text is suitable for showing the
    /// user; structured details are not exposed at this level.
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_serde_roundtrip() {
        let cmds = [
            Command::Help { topic: None },
            Command::Help {
                topic: Some("rooms".to_owned()),
            },
            Command::Register {
                username: Username::new("alice").unwrap(),
            },
            Command::WorkflowReply {
                reply: "blue".to_owned(),
            },
            Command::Logout,
            Command::Unknown {
                raw: "asdf".to_owned(),
            },
        ];
        for c in cmds {
            let json = serde_json::to_string(&c).unwrap();
            let back: Command = serde_json::from_str(&json).unwrap();
            assert_eq!(c, back);
        }
    }

    #[test]
    fn response_serde_roundtrip() {
        let responses = [
            Response::Text("welcome".to_owned()),
            Response::Prompt {
                text: "password:".to_owned(),
                hide_input: true,
            },
            Response::LoggedIn {
                user: Username::new("alice").unwrap(),
            },
            Response::LoggedOut,
            Response::Error("nope".to_owned()),
        ];
        for r in responses {
            let json = serde_json::to_string(&r).unwrap();
            let back: Response = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back);
        }
    }
}
