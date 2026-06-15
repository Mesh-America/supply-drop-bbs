//! Command parsing and response/notification rendering for the mesh transport.
//!
//! # Command parsing
//!
//! [`parse_command`] converts the raw text of an incoming direct message into a
//! [`Command`] that the host can process.  The rules are:
//!
//! 1. If the session is **awaiting a workflow reply** (e.g. the host just
//!    asked for a password), the entire message is wrapped in
//!    [`Command::WorkflowReply`] regardless of its content.
//! 2. If a **command prefix** is configured (e.g. `'!'`), only messages that
//!    start with that character are parsed as commands; all others are treated
//!    as [`Command::WorkflowReply`] (covering mid-workflow input that doesn't
//!    start with the prefix).
//! 3. Otherwise every message is parsed as a command keyword.  Unrecognised
//!    keywords become [`Command::Unknown`].
//!
//! Returns `None` when the message should be silently ignored — specifically
//! when a prefix is configured but the message neither starts with that prefix
//! nor belongs to an active workflow.
//!
//! # Response rendering
//!
//! [`format_response`] converts a [`Response`] from the host into the text
//! string that will be sent back to the mesh node over the radio.
//!
//! # Notification rendering
//!
//! [`render_notification`] converts a [`Notification`] (a host-initiated push)
//! into the text string delivered via `OutboundFrame::SendTxtMsg`.

use bbs_plugin_api::{event::Notification, identity::Username, Command, PermissionLevel, Response};

// ── Command parsing ───────────────────────────────────────────────────────────

/// Parse the raw text of an incoming direct message into a [`Command`].
///
/// ## Parameters
///
/// - `text`: the raw message text, straight from the wire.
/// - `prefix`: optional single-character prefix configured by the operator.
/// - `awaiting_reply`: `true` if the host is waiting for workflow input from
///   this session (e.g. a password prompt was just sent).
///
/// ## Return value
///
/// - `Some(Command)` — a command to dispatch to the host.
/// - `None` — the message should be silently dropped (prefix configured,
///   message doesn't start with it, and no workflow is active).
pub fn parse_command(text: &str, prefix: Option<char>, awaiting_reply: bool) -> Option<Command> {
    // Trim standard whitespace and null bytes.  Some MeshCore firmware
    // null-terminates its text payloads; without this, "N\0" would not match
    // the "n" keyword and would produce Command::Unknown instead of ReadNew.
    let text = text.trim().trim_matches('\0');

    // ── Cancel / stop always break out of any workflow ───────────────────────
    if matches!(text.to_ascii_lowercase().as_str(), "cancel" | "stop") {
        return Some(Command::Cancel);
    }

    // ── Workflow continuations take priority ─────────────────────────────────
    // If the host is waiting for a reply, treat the whole message as one
    // regardless of whether it looks like a command keyword.
    if awaiting_reply {
        return Some(Command::WorkflowReply {
            reply: text.to_owned(),
        });
    }

    // ── Strip optional command prefix ────────────────────────────────────────
    let text = if let Some(p) = prefix {
        if let Some(stripped) = text.strip_prefix(p) {
            stripped.trim_start()
        } else {
            // Prefix is configured but this message doesn't start with it —
            // not a command and no workflow is active.
            return None;
        }
    } else {
        text
    };

    if text.is_empty() {
        return Some(Command::Unknown { raw: String::new() });
    }

    // ── Keyword dispatch ─────────────────────────────────────────────────────
    // Split on the first run of whitespace: `word` is the command keyword,
    // `rest` is the remainder (trimmed), or None if there is none.
    let (word, rest) = split_first_word(text);
    let keyword = word.to_ascii_lowercase();

    match keyword.as_str() {
        "h" | "help" | "?" => Some(Command::Help {
            topic: rest.map(str::to_owned),
        }),

        "register" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(username) => Some(Command::Register { username }),
            None => Some(Command::Help {
                topic: Some("register".to_owned()),
            }),
        },

        "login" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(username) => Some(Command::Login { username }),
            None => Some(Command::Help {
                topic: Some("login".to_owned()),
            }),
        },

        // ── Room navigation ──────────────────────────────────────────────────
        "k" => Some(Command::ListRooms),

        "g" => Some(Command::GoNextUnread),

        "c" => Some(Command::ChangeRoom {
            target: rest.unwrap_or("").to_owned(),
        }),

        "m" => Some(Command::GoMail),

        "i" => Some(Command::IgnoreRoom),

        // ── Message reading ──────────────────────────────────────────────────
        "n" => Some(Command::ReadNew),

        "f" => {
            let after = rest.and_then(|s| s.parse::<i64>().ok());
            Some(Command::ReadForward { after })
        }

        "r" => Some(Command::ReadReverse),

        "s" => match rest {
            Some(q) if !q.is_empty() => Some(Command::SearchUsers {
                query: q.to_owned(),
            }),
            _ => Some(Command::ScanMessages),
        },

        ".ff" => Some(Command::FastForward),

        // ── Message posting / deletion ───────────────────────────────────────
        "e" => Some(Command::EnterMessage {
            body: rest.filter(|s| !s.is_empty()).map(str::to_owned),
        }),

        "d" => match rest.and_then(|s| s.parse::<i64>().ok()) {
            Some(id) => Some(Command::DeleteMessage { id }),
            None => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },

        // ── Session control ──────────────────────────────────────────────────
        "q" => Some(Command::Quit),

        "cancel" | "stop" => Some(Command::Cancel),

        // ── Moderation / account ─────────────────────────────────────────────
        "w" => Some(Command::WhoIsOnline),

        "pending" => Some(Command::ListPending),

        "v" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(username) => Some(Command::ValidateUser { username }),
            None => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
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
                Ok(target) => Some(Command::BlockUser { target, force }),
                Err(_) => Some(Command::Unknown {
                    raw: text.to_owned(),
                }),
            }
        }

        "ban" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(username) => Some(Command::BanUser { username }),
            None => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },

        "unban" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(username) => Some(Command::UnbanUser { username }),
            None => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },

        "u" | "users" => Some(Command::ListUsers {
            filter: rest.map(str::to_owned),
        }),

        "whois" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(username) => Some(Command::UserInfo { username }),
            None => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },

        "whoami" => Some(Command::Whoami),

        "profile" => Some(Command::EditProfile),

        "passwd" => Some(Command::ChangePassword),

        // ── Room management ──────────────────────────────────────────────────
        ".c" => match rest {
            Some(name) if !name.is_empty() => Some(Command::CreateRoom {
                name: name.to_owned(),
            }),
            _ => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },

        ".dr" => match rest {
            Some(name) if !name.is_empty() => Some(Command::DeleteRoom {
                name: name.to_owned(),
            }),
            _ => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },

        ".er" => Some(Command::EditRoom),

        ".eu" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(username) => Some(Command::EditUser { username }),
            None => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },

        ".du" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(username) => Some(Command::DeleteUser { username }),
            None => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },

        ".aide" => Some(parse_set_level(rest, PermissionLevel::Aide)),
        ".sysop" => Some(parse_set_level(rest, PermissionLevel::Sysop)),
        ".user" => Some(parse_set_level(rest, PermissionLevel::User)),

        _ => Some(Command::Unknown {
            raw: text.to_owned(),
        }),
    }
}

/// Split `s` on the first run of ASCII whitespace.
///
/// Returns `(first_word, rest)` where `rest` is `Some` (trimmed) if there
/// were characters after the first word, or `None` otherwise.
fn split_first_word(s: &str) -> (&str, Option<&str>) {
    match s.find(|c: char| c.is_ascii_whitespace()) {
        None => (s, None),
        Some(i) => {
            let rest = s[i..].trim_start();
            (&s[..i], if rest.is_empty() { None } else { Some(rest) })
        }
    }
}

/// Parse a `.AIDE` / `.SYSOP` / `.USER <user>` set-level command. (#127)
fn parse_set_level(rest: Option<&str>, level: PermissionLevel) -> Command {
    match rest.and_then(|s| Username::new(s).ok()) {
        Some(username) => Command::SetUserLevel { username, level },
        // Missing/invalid username → show the command's usage rather than a
        // generic "unknown command". (#127 follow-up)
        None => {
            let topic = match level {
                PermissionLevel::Aide => ".aide",
                PermissionLevel::Sysop => ".sysop",
                _ => ".user",
            };
            Command::Help {
                topic: Some(topic.to_owned()),
            }
        }
    }
}

// ── Response rendering ────────────────────────────────────────────────────────

/// Render a [`Response`] from the host into the text that will be sent back to
/// the mesh node.
///
/// Returns `None` for response variants that carry no user-visible text (e.g.
/// future variants added to the non-exhaustive enum that this crate doesn't
/// know about yet).
pub fn format_response(response: &Response) -> Option<String> {
    match response {
        Response::Text(t) => Some(t.clone()),

        // Prompts are plain text on mesh — there's no "hidden input" concept
        // over LoRa radio.  We send the prompt text and rely on the
        // `awaiting_reply` flag to interpret the next message correctly.
        Response::Prompt { text, .. } => Some(text.clone()),

        Response::LoggedIn { user } => Some(format!(
            "Welcome, {}. Type 'H' for commands.",
            user.as_str()
        )),

        Response::LoggedOut => Some("Goodbye. Your session has ended.".to_owned()),

        Response::Error(e) => Some(format!("Error: {e}")),

        // MultiText: join parts for callers that expect a single string.
        // The mesh transport handles MultiText specially in dispatch_message.
        Response::MultiText(parts) => Some(parts.join("\n")),

        // Non-exhaustive catch-all — future variants we don't render yet.
        _ => None,
    }
}

// ── Notification rendering ────────────────────────────────────────────────────

/// Render a [`Notification`] into the text delivered to a mesh node.
///
/// Called from [`MeshTransport::notify`](crate::MeshTransport) when the host
/// pushes an unsolicited event to an active session.
pub fn render_notification(notification: &Notification) -> String {
    match notification {
        Notification::Text(t) => t.clone(),
        Notification::MailWaiting { count } => format!(
            "You have {} unread message{}. Reply 'mail' to read.",
            count,
            if *count == 1 { "" } else { "s" }
        ),
        Notification::SystemEvent(s) => format!("[system] {s}"),
        // Non-exhaustive catch-all.
        _ => "[notification]".to_owned(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(text: &str) -> Option<Command> {
        parse_command(text, None, false)
    }

    fn cmd_prefix(text: &str) -> Option<Command> {
        parse_command(text, Some('!'), false)
    }

    // ── No prefix, no workflow ───────────────────────────────────────────────

    #[test]
    fn help_no_topic() {
        assert_eq!(cmd("help"), Some(Command::Help { topic: None }));
        assert_eq!(cmd("HELP"), Some(Command::Help { topic: None }));
        assert_eq!(cmd("?"), Some(Command::Help { topic: None }));
    }

    #[test]
    fn help_with_topic() {
        assert_eq!(
            cmd("help rooms"),
            Some(Command::Help {
                topic: Some("rooms".to_owned())
            })
        );
        assert_eq!(
            cmd("HELP  rooms"),
            Some(Command::Help {
                topic: Some("rooms".to_owned())
            })
        );
    }

    #[test]
    fn register_valid_username() {
        let username = Username::new("alice").unwrap();
        assert_eq!(cmd("register alice"), Some(Command::Register { username }));
    }

    #[test]
    fn register_missing_username_shows_help() {
        assert_eq!(
            cmd("register"),
            Some(Command::Help {
                topic: Some("register".to_owned())
            })
        );
    }

    #[test]
    fn register_invalid_username_shows_help() {
        // Usernames can't have spaces; "register alice bob" → help for register.
        assert_eq!(
            cmd("register alice bob"),
            Some(Command::Help {
                topic: Some("register".to_owned())
            })
        );
    }

    #[test]
    fn login_valid_username() {
        let username = Username::new("bob").unwrap();
        assert_eq!(cmd("login bob"), Some(Command::Login { username }));
    }

    #[test]
    fn quit() {
        assert_eq!(cmd("q"), Some(Command::Quit));
        assert_eq!(cmd("Q"), Some(Command::Quit));
    }

    #[test]
    fn logout_is_unknown_on_mesh() {
        assert!(matches!(cmd("logout"), Some(Command::Unknown { .. })));
    }

    #[test]
    fn whoami_is_parsed_on_mesh() {
        assert!(matches!(cmd("whoami"), Some(Command::Whoami)));
    }

    #[test]
    fn set_level_commands_parsed_on_mesh() {
        // `.AIDE`/`.SYSOP`/`.USER <user>` promote/demote (sysop-gated by host). (#127)
        let u = Username::new("bob").unwrap();
        assert_eq!(
            cmd(".aide bob"),
            Some(Command::SetUserLevel {
                username: u.clone(),
                level: PermissionLevel::Aide
            })
        );
        assert_eq!(
            cmd(".sysop bob"),
            Some(Command::SetUserLevel {
                username: u.clone(),
                level: PermissionLevel::Sysop
            })
        );
        assert_eq!(
            cmd(".user bob"),
            Some(Command::SetUserLevel {
                username: u,
                level: PermissionLevel::User
            })
        );
        // Missing username → the command's usage help, not a generic unknown.
        assert_eq!(
            cmd(".aide"),
            Some(Command::Help {
                topic: Some(".aide".to_owned())
            })
        );
    }

    #[test]
    fn unknown_keyword() {
        assert_eq!(
            cmd("rooms"),
            Some(Command::Unknown {
                raw: "rooms".to_owned()
            })
        );
    }

    #[test]
    fn whitespace_trimmed() {
        assert_eq!(cmd("  help  "), Some(Command::Help { topic: None }));
    }

    // ── Workflow reply ───────────────────────────────────────────────────────

    #[test]
    fn awaiting_reply_wraps_everything() {
        // Even if the text looks like a command keyword, WorkflowReply is used.
        let result = parse_command("help", None, true);
        assert_eq!(
            result,
            Some(Command::WorkflowReply {
                reply: "help".to_owned()
            })
        );
    }

    #[test]
    fn awaiting_reply_password_text() {
        let result = parse_command("mysecretpassword", None, true);
        assert_eq!(
            result,
            Some(Command::WorkflowReply {
                reply: "mysecretpassword".to_owned()
            })
        );
    }

    // ── Command prefix ───────────────────────────────────────────────────────

    #[test]
    fn prefix_required_strips_prefix() {
        assert_eq!(cmd_prefix("!help"), Some(Command::Help { topic: None }));
    }

    #[test]
    fn prefix_required_without_prefix_is_none() {
        // No active workflow → silently ignored.
        assert_eq!(cmd_prefix("help"), None);
    }

    #[test]
    fn prefix_awaiting_reply_ignores_prefix_check() {
        // Mid-workflow: user sends password without the prefix → WorkflowReply.
        let result = parse_command("mypassword", Some('!'), true);
        assert_eq!(
            result,
            Some(Command::WorkflowReply {
                reply: "mypassword".to_owned()
            })
        );
    }

    #[test]
    fn prefix_with_topic() {
        assert_eq!(
            cmd_prefix("!help rooms"),
            Some(Command::Help {
                topic: Some("rooms".to_owned())
            })
        );
    }

    // ── Response rendering ───────────────────────────────────────────────────

    #[test]
    fn format_text_response() {
        assert_eq!(
            format_response(&Response::Text("hello".to_owned())),
            Some("hello".to_owned())
        );
    }

    #[test]
    fn format_prompt_response() {
        assert_eq!(
            format_response(&Response::Prompt {
                text: "Password:".to_owned(),
                hide_input: true,
            }),
            Some("Password:".to_owned())
        );
    }

    #[test]
    fn format_logged_in() {
        let user = Username::new("alice").unwrap();
        let text = format_response(&Response::LoggedIn { user }).unwrap();
        assert!(
            text.contains("alice"),
            "LoggedIn response must include username"
        );
    }

    #[test]
    fn format_logged_out() {
        let text = format_response(&Response::LoggedOut).unwrap();
        assert!(!text.is_empty());
    }

    #[test]
    fn format_error() {
        let text = format_response(&Response::Error("not found".to_owned())).unwrap();
        assert!(text.contains("not found"));
    }

    // ── Notification rendering ───────────────────────────────────────────────

    #[test]
    fn render_text_notification() {
        assert_eq!(
            render_notification(&Notification::Text("hello".to_owned())),
            "hello"
        );
    }

    #[test]
    fn render_mail_waiting_singular() {
        let text = render_notification(&Notification::MailWaiting { count: 1 });
        assert!(text.contains('1') && !text.contains("messages"));
    }

    #[test]
    fn render_mail_waiting_plural() {
        let text = render_notification(&Notification::MailWaiting { count: 3 });
        assert!(text.contains('3') && text.contains("messages"));
    }

    #[test]
    fn render_system_event() {
        let text = render_notification(&Notification::SystemEvent("validated".to_owned()));
        assert!(text.contains("validated") && text.contains("[system]"));
    }
}
