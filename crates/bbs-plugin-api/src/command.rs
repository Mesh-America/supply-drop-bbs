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

// ── Parsing helpers (private) ─────────────────────────────────────────────────

fn split_first_word(s: &str) -> (&str, Option<&str>) {
    match s.find(|c: char| c.is_ascii_whitespace()) {
        None => (s, None),
        Some(i) => {
            let rest = s[i..].trim_start();
            (&s[..i], if rest.is_empty() { None } else { Some(rest) })
        }
    }
}

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

    /// Toggle ignore on the current room. (I)
    IgnoreRoom,

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

    /// Fast-forward past all unread messages in the current room. (.FF)
    FastForward,

    // ── Message posting / deletion ────────────────────────────────────
    /// Compose a message. (E)
    ///
    /// Both paths require a lone `.` to confirm before the message is posted:
    ///
    /// - `body` is `Some`: the inline text is staged as a draft
    ///   (`AwaitingConfirmation`) and the user must reply with `.` to send.
    ///   This makes sends idempotent on lossy links — if "Message posted." is
    ///   not received the user sends `.` again without creating a duplicate.
    /// - `body` is `None`: the host prompts for the body, then transitions to
    ///   `AwaitingConfirmation` where `.` finalises the post.
    ///
    /// For Mail DMs the format is `E @recipient message text`; for the
    /// current room it is `E message text`.
    EnterMessage {
        /// Optional inline body (and, for Mail, optional `@recipient`).
        body: Option<String>,
    },

    /// Delete a message by its numeric ID. (D)
    DeleteMessage {
        /// The numeric message ID to delete.
        id: i64,
    },

    // ── Session control ───────────────────────────────────────────────
    /// Quit — log out gracefully. (Q)
    Quit,

    /// Cancel the current workflow without logging out. (CANCEL / STOP)
    Cancel,

    // ── Moderation / account ──────────────────────────────────────────
    /// List all currently logged-in sessions. (W)
    WhoIsOnline,

    /// List users whose accounts are awaiting aide validation. (PENDING)
    ListPending,

    /// Promote an unvalidated user to full User tier (Aide+). (V)
    ValidateUser {
        /// The username of the account to validate.
        username: Username,
    },

    /// Block or unblock another user — hides their messages from the caller. (B)
    ///
    /// `force = Some(true)` → force-block, `Some(false)` → force-unblock,
    /// `None` → toggle.  Prefix the username with `+` to force-block or `-`
    /// to force-unblock from the mesh transport.
    BlockUser {
        /// The username to block or unblock.
        target: Username,
        /// Explicit direction, or `None` for toggle.
        force: Option<bool>,
    },

    /// Ban a user account, preventing further login (Aide+). (BAN)
    BanUser {
        /// The username of the account to ban.
        username: Username,
    },

    /// Lift a ban on a previously banned user (Sysop+). (UNBAN)
    UnbanUser {
        /// The username of the account to unban.
        username: Username,
    },

    /// Begin editing the caller's own display name. (PROFILE)
    EditProfile,

    /// Begin the change-password workflow. (PASSWD)
    ///
    /// The workflow asks for the current password (to verify identity),
    /// then the new password twice.  Requires the session to be logged in
    /// at User level or above.
    ChangePassword,

    // ── Room / user management ────────────────────────────────────────
    /// Create a new room with the given name (Sysop+). (.C)
    CreateRoom {
        /// Short room name — must pass `Room::validate_name`.
        name: String,
    },

    /// Delete a room by name (Sysop+). (.DR)
    DeleteRoom {
        /// Name of the room to delete.
        name: String,
    },

    /// Edit the current room's properties (Aide+). (.ER)
    EditRoom,

    /// Edit a user's properties (Aide+). (.EU)
    EditUser {
        /// The username of the account to edit.
        username: Username,
    },

    /// List user accounts (Aide+). (USERS)
    ///
    /// Optional filter: "active" (default), "banned", or "all" (Sysop+).
    ListUsers {
        /// Status filter string from the user. None = active only.
        filter: Option<String>,
    },

    /// Search user accounts by username substring (Aide+). (SEARCH)
    SearchUsers {
        /// Substring to match against usernames (case-insensitive).
        query: String,
    },

    /// Show details for a specific user account (Aide+). (WHOIS)
    UserInfo {
        /// The username to look up.
        username: Username,
    },

    /// Soft-delete a user account (Sysop+). (.DU)
    DeleteUser {
        /// The username of the account to delete.
        username: Username,
    },

    /// Reset another user's password (Sysop+). (`.PW <username>`)
    ///
    /// Starts a two-prompt workflow: new password, then confirm.
    /// The caller does not need to know the target's current password.
    SetUserPassword {
        /// The account whose password will be reset.
        username: Username,
    },

    // ── Access policy (Sysop only) ────────────────────────────────────
    /// Enable open access — disable the verification requirement (Sysop+).
    ///
    /// Takes effect immediately in-memory and is persisted to `config.toml`.
    /// Keyword: `OPENACCESS`
    OpenAccess,

    /// Restore the verification requirement (Sysop+).
    ///
    /// Takes effect immediately in-memory and is persisted to `config.toml`.
    /// Keyword: `CLOSEACCESS`
    CloseAccess,

    /// Set or clear the guest room (Sysop+).
    ///
    /// `name = Some("RoomName")` enables the guest room (created if needed).
    /// `name = None` disables the feature.
    ///
    /// Takes effect immediately in-memory and is persisted to `config.toml`.
    /// Keyword: `GUESTROOM <name>` or `GUESTROOM OFF`
    SetGuestRoom {
        /// Room name, or `None` to disable.
        name: Option<String>,
    },
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

    /// Multiple text frames to be delivered as separate messages.
    ///
    /// Transports with per-message size limits (e.g. LoRa radio) send each
    /// element as an independent frame. Transports without size constraints
    /// (e.g. CLI) may join the parts with newlines.
    MultiText(Vec<String>),
}

// ── Command::parse ────────────────────────────────────────────────────────────

impl Command {
    /// Parse a raw text line from a transport connection into a [`Command`].
    ///
    /// When `awaiting_reply` is `true` (the previous [`Response`] was a
    /// [`Response::Prompt`]), the entire line becomes a [`Command::WorkflowReply`]
    /// regardless of content — this lets the host handle password entry, message
    /// bodies, and other free-form workflow steps without each transport
    /// re-implementing that state machine.
    ///
    /// This is the canonical parser shared by all transports that forward raw
    /// text lines (CLI, process plugins).  Transports with their own wire
    /// syntax (e.g. MeshCore frames) do their own mapping.
    pub fn parse(line: &str, awaiting_reply: bool) -> Self {
        let text = line.trim();

        if awaiting_reply {
            return Command::WorkflowReply {
                reply: text.to_owned(),
            };
        }

        if text.is_empty() {
            return Command::Unknown { raw: String::new() };
        }

        let (word, rest) = split_first_word(text);
        let keyword = word.to_ascii_lowercase();

        match keyword.as_str() {
            // ── Auth ─────────────────────────────────────────────────────────
            "h" | "help" | "?" => Command::Help {
                topic: rest.map(str::to_owned),
            },
            "register" => match rest.and_then(|s| Username::new(s).ok()) {
                Some(u) => Command::Register { username: u },
                None => Command::Help {
                    topic: Some("register".to_owned()),
                },
            },
            "login" => match rest.and_then(|s| Username::new(s).ok()) {
                Some(u) => Command::Login { username: u },
                None => Command::Help {
                    topic: Some("login".to_owned()),
                },
            },
            "logout" | "q" | "quit" => Command::Quit,

            // ── Room navigation ───────────────────────────────────────────────
            "k" => Command::ListRooms,
            "g" => Command::GoNextUnread,
            "c" => Command::ChangeRoom {
                target: rest.unwrap_or("").to_owned(),
            },
            "m" => Command::GoMail,
            "i" => Command::IgnoreRoom,

            // ── Message reading ───────────────────────────────────────────────
            "n" => Command::ReadNew,
            "f" => Command::ReadForward {
                after: rest.and_then(|s| s.parse::<i64>().ok()),
            },
            "r" => Command::ReadReverse,
            "s" => match rest {
                Some(q) if !q.is_empty() => Command::SearchUsers {
                    query: q.to_owned(),
                },
                _ => Command::ScanMessages,
            },
            ".ff" => Command::FastForward,

            // ── Message posting / deletion ────────────────────────────────────
            "e" => Command::EnterMessage {
                body: rest.filter(|s| !s.is_empty()).map(str::to_owned),
            },
            "d" => match rest.and_then(|s| s.parse::<i64>().ok()) {
                Some(id) => Command::DeleteMessage { id },
                None => Command::Unknown {
                    raw: text.to_owned(),
                },
            },

            // ── Moderation / account ──────────────────────────────────────────
            "w" => Command::WhoIsOnline,
            "pending" => Command::ListPending,
            "v" => match rest.and_then(|s| Username::new(s).ok()) {
                Some(username) => Command::ValidateUser { username },
                None => Command::Unknown {
                    raw: text.to_owned(),
                },
            },
            "b" => {
                let raw_arg = rest.unwrap_or("").trim();
                let (force, name) = if let Some(s) = raw_arg.strip_prefix('+') {
                    (Some(true), s.trim())
                } else if let Some(s) = raw_arg.strip_prefix('-') {
                    (Some(false), s.trim())
                } else {
                    (None, raw_arg)
                };
                match Username::new(name) {
                    Ok(target) => Command::BlockUser { target, force },
                    Err(_) => Command::Unknown {
                        raw: text.to_owned(),
                    },
                }
            }
            "ban" => match rest.and_then(|s| Username::new(s).ok()) {
                Some(username) => Command::BanUser { username },
                None => Command::Unknown {
                    raw: text.to_owned(),
                },
            },
            "unban" => match rest.and_then(|s| Username::new(s).ok()) {
                Some(username) => Command::UnbanUser { username },
                None => Command::Unknown {
                    raw: text.to_owned(),
                },
            },
            "u" | "users" => Command::ListUsers {
                filter: rest.map(str::to_owned),
            },
            "whois" => match rest.and_then(|s| Username::new(s).ok()) {
                Some(username) => Command::UserInfo { username },
                None => Command::Unknown {
                    raw: text.to_owned(),
                },
            },
            "profile" => Command::EditProfile,
            "passwd" => Command::ChangePassword,

            // ── Room / user management ────────────────────────────────────────
            ".c" => match rest {
                Some(name) if !name.is_empty() => Command::CreateRoom {
                    name: name.to_owned(),
                },
                _ => Command::Unknown {
                    raw: text.to_owned(),
                },
            },
            ".dr" => match rest {
                Some(name) if !name.is_empty() => Command::DeleteRoom {
                    name: name.to_owned(),
                },
                _ => Command::Unknown {
                    raw: text.to_owned(),
                },
            },
            ".er" => Command::EditRoom,
            ".eu" => match rest.and_then(|s| Username::new(s).ok()) {
                Some(username) => Command::EditUser { username },
                None => Command::Unknown {
                    raw: text.to_owned(),
                },
            },
            ".du" => match rest.and_then(|s| Username::new(s).ok()) {
                Some(username) => Command::DeleteUser { username },
                None => Command::Unknown {
                    raw: text.to_owned(),
                },
            },

            ".pw" => match rest.and_then(|s| Username::new(s).ok()) {
                Some(username) => Command::SetUserPassword { username },
                None => Command::Unknown {
                    raw: text.to_owned(),
                },
            },

            // ── Access policy ─────────────────────────────────────────────────
            "openaccess" => Command::OpenAccess,
            "closeaccess" => Command::CloseAccess,
            "guestroom" => match rest {
                Some(arg) if arg.eq_ignore_ascii_case("off") => {
                    Command::SetGuestRoom { name: None }
                }
                Some(name) if !name.is_empty() => Command::SetGuestRoom {
                    name: Some(name.to_owned()),
                },
                _ => Command::Unknown {
                    raw: text.to_owned(),
                },
            },

            _ => Command::Unknown {
                raw: text.to_owned(),
            },
        }
    }
}

// ── Response helpers ──────────────────────────────────────────────────────────

impl Response {
    /// Render this response to a user-visible text string.
    ///
    /// Returns `None` for variants that carry no displayable content.
    /// Transports should treat `None` as "send nothing to the user."
    pub fn render(&self) -> Option<String> {
        match self {
            Response::Text(t) => Some(t.clone()),
            Response::Prompt { text, .. } => Some(text.clone()),
            Response::LoggedIn { user } => Some(format!(
                "Welcome, {}. Type 'H' for commands.",
                user.as_str()
            )),
            Response::LoggedOut => Some("Goodbye. Your session has ended.".to_owned()),
            Response::Error(e) => Some(format!("Error: {e}")),
            Response::MultiText(parts) => Some(parts.join("\n")),
        }
    }

    /// Whether the next input from the user should be treated as a
    /// [`Command::WorkflowReply`] rather than a parsed command.
    ///
    /// Transports must track this flag per-session and pass it to
    /// [`Command::parse`] on the next input.
    pub fn sets_awaiting_reply(&self) -> bool {
        matches!(self, Response::Prompt { .. })
    }

    /// Whether the user's next input should be visually hidden
    /// (e.g. password entry). Only meaningful when
    /// [`sets_awaiting_reply`](Self::sets_awaiting_reply) is also `true`.
    pub fn hides_next_input(&self) -> bool {
        matches!(
            self,
            Response::Prompt {
                hide_input: true,
                ..
            }
        )
    }
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
            Response::MultiText(vec!["part 1".to_owned(), "part 2".to_owned()]),
        ];
        for r in responses {
            let json = serde_json::to_string(&r).unwrap();
            let back: Response = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back);
        }
    }
}
