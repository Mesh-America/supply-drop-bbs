//! Command parsing and response rendering for the Meshtastic transport.

use bbs_plugin_api::{event::Notification, identity::Username, Command, PermissionLevel, Response};

pub fn parse_command(text: &str, prefix: Option<char>, awaiting_reply: bool) -> Option<Command> {
    let text = text.trim();
    let stripped = strip_zero_width(text);
    let text = stripped.as_str();

    if matches!(text.to_lowercase().as_str(), "cancel" | "stop") {
        return Some(Command::Cancel);
    }

    if awaiting_reply {
        return Some(Command::WorkflowReply {
            reply: text.to_owned(),
        });
    }

    let text = if let Some(p) = prefix {
        if let Some(stripped) = text.strip_prefix(p) {
            stripped.trim_start()
        } else {
            return None;
        }
    } else {
        text
    };

    if text.is_empty() {
        return Some(Command::Unknown { raw: String::new() });
    }

    let (word, rest) = split_first_word(text);
    let keyword = word.to_lowercase();

    match keyword.as_str() {
        "h" | "help" | "?" => Some(Command::Help {
            topic: rest.map(str::to_owned),
        }),
        // `register <user>` → interactive; `register <user> <password>` → one-shot.
        "register" => match rest {
            Some(r) => {
                let (name, password) = split_first_word(r);
                if name.is_empty() {
                    Some(Command::Help {
                        topic: Some("register".to_owned()),
                    })
                } else if let Some(password) = password {
                    Some(Command::RegisterOneShot {
                        username: name.to_owned(),
                        password: password.into(),
                    })
                } else {
                    Some(Command::Register {
                        username: name.to_owned(),
                    })
                }
            }
            None => Some(Command::Help {
                topic: Some("register".to_owned()),
            }),
        },
        // `login <user>` → interactive; `login <user> <password>` → one-shot.
        "login" => match rest {
            Some(r) => {
                let (name, password) = split_first_word(r);
                match (Username::new(name).ok(), password) {
                    (Some(username), Some(password)) => Some(Command::LoginOneShot {
                        username,
                        password: password.into(),
                    }),
                    (Some(username), None) => Some(Command::Login { username }),
                    (None, _) => Some(Command::Help {
                        topic: Some("login".to_owned()),
                    }),
                }
            }
            None => Some(Command::Help {
                topic: Some("login".to_owned()),
            }),
        },
        "whoami" => Some(Command::Whoami),
        "k" => Some(Command::ListRooms),
        "g" => Some(Command::GoNextUnread),
        "c" => Some(Command::ChangeRoom {
            target: rest.unwrap_or("").to_owned(),
        }),
        "m" => Some(Command::GoMail),
        "n" => Some(Command::ReadNew),
        "f" => match rest {
            None => Some(Command::ReadForward { after: None }),
            Some(s) => match s.parse::<i64>() {
                Ok(id) => Some(Command::ReadForward { after: Some(id) }),
                Err(_) => Some(Command::Unknown {
                    raw: text.to_owned(),
                }),
            },
        },
        "r" => Some(Command::ReadReverse),
        // Unconditionally ScanMessages regardless of trailing text (#411) —
        // see bbs-plugin-api::Command::parse's identical arm for the reason.
        "s" => Some(Command::ScanMessages),
        "search" => match rest {
            Some(q) if !q.is_empty() => Some(Command::SearchUsers {
                query: q.to_owned(),
            }),
            _ => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },
        ".ff" => Some(Command::FastForward),
        "e" => Some(Command::EnterMessage {
            body: rest.filter(|s| !s.is_empty()).map(str::to_owned),
        }),
        "d" => match rest.and_then(|s| s.parse::<i64>().ok()) {
            Some(id) => Some(Command::DeleteMessage { id }),
            None => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },
        // All quit aliases map to one intent (Quit), parallel with the mesh and
        // canonical parsers. (#124 follow-up)
        "q" | "quit" | "exit" | "bye" | "logout" => Some(Command::Quit),
        "cancel" | "stop" => Some(Command::Cancel),
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
        "timeout" => {
            let mut parts = rest.unwrap_or("").split_whitespace();
            let username = parts.next().and_then(|s| Username::new(s).ok());
            let days = parts.next().and_then(|s| s.parse::<u8>().ok());
            match (username, days) {
                (Some(username), Some(days)) if (1..=5).contains(&days) => {
                    Some(Command::TimeoutUser { username, days })
                }
                _ => Some(Command::Unknown {
                    raw: text.to_owned(),
                }),
            }
        }
        "u" | "users" => Some(Command::ListUsers {
            filter: rest.map(str::to_owned),
        }),
        "whois" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(username) => Some(Command::UserInfo { username }),
            None => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },
        "profile" => Some(Command::EditProfile),
        "passwd" => Some(Command::ChangePassword),
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
        ".pw" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(username) => Some(Command::SetUserPassword { username }),
            None => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },
        ".aide" => Some(parse_set_level(rest, PermissionLevel::Aide)),
        ".sysop" => Some(parse_set_level(rest, PermissionLevel::Sysop)),
        ".user" => Some(parse_set_level(rest, PermissionLevel::User)),
        "openaccess" => Some(Command::OpenAccess),
        "closeaccess" => Some(Command::CloseAccess),
        "guestroom" => match rest {
            Some(arg) if arg.eq_ignore_ascii_case("off") => {
                Some(Command::SetGuestRoom { name: None })
            }
            Some(name) if !name.is_empty() => Some(Command::SetGuestRoom {
                name: Some(name.to_owned()),
            }),
            _ => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },
        _ => Some(Command::Unknown {
            raw: text.to_owned(),
        }),
    }
}

/// Split `s` on the first run of whitespace.
///
/// Unicode-aware (`char::is_whitespace`), matching `text.trim()`'s own
/// whitespace definition — an ASCII-only split here left a line joined by
/// e.g. a non-breaking space (U+00A0) as one unmatched word instead of
/// keyword + argument (#413).
fn split_first_word(s: &str) -> (&str, Option<&str>) {
    match s.find(char::is_whitespace) {
        None => (s, None),
        Some(i) => {
            let rest = s[i..].trim_start();
            (&s[..i], if rest.is_empty() { None } else { Some(rest) })
        }
    }
}

/// Strip zero-width and other default-ignorable code points that survive
/// `trim()`/`to_lowercase()` — see `bbs_plugin_api::command`'s identical
/// helper for the full rationale (#412).
fn strip_zero_width(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(*c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}'))
        .collect()
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

pub fn format_response(response: &Response) -> Option<String> {
    match response {
        Response::Text(t) => Some(t.clone()),
        Response::Prompt { text, .. } => Some(text.clone()),
        Response::LoggedIn { user } => Some(format!(
            "Welcome, {}. Type 'H' for commands.",
            user.as_str()
        )),
        Response::LoggedOut => Some("Goodbye. Your session has ended.".to_owned()),
        Response::Error(e) => Some(format!("Error: {e}")),
        Response::MultiText(parts) => Some(parts.join("\n")),
        _ => None,
    }
}

pub fn render_notification(notification: &Notification) -> String {
    match notification {
        Notification::Text(t) => t.clone(),
        Notification::MailWaiting { count } => format!(
            "You have {} unread message{}. Reply 'M' to read.",
            count,
            if *count == 1 { "" } else { "s" }
        ),
        Notification::SystemEvent(s) => format!("[system] {s}"),
        _ => "[notification]".to_owned(),
    }
}

pub fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_without_match_is_ignored() {
        assert_eq!(parse_command("hello", Some('!'), false), None);
        assert!(matches!(
            parse_command("!h", Some('!'), false),
            Some(Command::Help { .. })
        ));
    }

    #[test]
    fn workflow_reply_beats_prefix() {
        assert_eq!(
            parse_command("secret", Some('!'), true),
            Some(Command::WorkflowReply {
                reply: "secret".to_owned()
            })
        );
    }

    #[test]
    fn timeout_user_and_days() {
        let u = Username::new("bob").unwrap();
        assert_eq!(
            parse_command("timeout bob 3", None, false),
            Some(Command::TimeoutUser {
                username: u.clone(),
                days: 3
            })
        );
        assert_eq!(
            parse_command("TIMEOUT bob 5", None, false),
            Some(Command::TimeoutUser {
                username: u,
                days: 5
            })
        );
    }

    #[test]
    fn timeout_bad_args_is_unknown() {
        for text in [
            "timeout",
            "timeout bob",
            "timeout bob 0",
            "timeout bob 6",
            "timeout bob x",
        ] {
            assert_eq!(
                parse_command(text, None, false),
                Some(Command::Unknown {
                    raw: text.to_owned()
                }),
                "{text}"
            );
        }
    }

    #[test]
    fn access_policy_words() {
        assert_eq!(
            parse_command("openaccess", None, false),
            Some(Command::OpenAccess)
        );
        assert_eq!(
            parse_command("CloseAccess", None, false),
            Some(Command::CloseAccess)
        );
        assert_eq!(
            parse_command("guestroom Lobby", None, false),
            Some(Command::SetGuestRoom {
                name: Some("Lobby".to_owned())
            })
        );
        assert_eq!(
            parse_command("guestroom OFF", None, false),
            Some(Command::SetGuestRoom { name: None })
        );
        assert_eq!(
            parse_command("guestroom", None, false),
            Some(Command::Unknown {
                raw: "guestroom".to_owned()
            })
        );
    }

    /// The radio parser must agree with the canonical `Command::parse` on the
    /// sysop/aide words — they drifted once (timeout, openaccess, closeaccess,
    /// guestroom fell through to Unknown on radio).
    #[test]
    fn sysop_words_match_canonical_parser() {
        for text in [
            "timeout bob 1",
            "timeout bob 9",
            "timeout",
            "openaccess",
            "closeaccess",
            "guestroom Lobby",
            "guestroom off",
            "guestroom",
        ] {
            assert_eq!(
                parse_command(text, None, false),
                Some(Command::parse(text, false)),
                "{text}"
            );
        }
    }

    #[test]
    fn truncate_preserves_utf8() {
        assert_eq!(truncate_utf8("ab😀cd", 5), "ab");
    }

    // ── Parser hardening (#411, #412, #413) ──────────────────────────────

    fn hardening(text: &str) -> Option<Command> {
        parse_command(text, None, false)
    }

    #[test]
    fn s_is_always_scan_and_search_is_the_user_search() {
        assert_eq!(hardening("s"), Some(Command::ScanMessages));
        assert_eq!(hardening("s foo"), Some(Command::ScanMessages));
        assert_eq!(
            hardening("search bob"),
            Some(Command::SearchUsers {
                query: "bob".to_owned()
            })
        );
        assert!(matches!(hardening("search"), Some(Command::Unknown { .. })));
    }

    #[test]
    fn zero_width_characters_cannot_defeat_cancel() {
        for text in ["cancel\u{200B}", "\u{FEFF}STOP", "ca\u{200D}ncel"] {
            assert_eq!(hardening(text), Some(Command::Cancel), "{text:?}");
        }
    }

    #[test]
    fn non_breaking_space_splits_keyword_and_argument() {
        assert!(matches!(
            hardening("c\u{00A0}lobby"),
            Some(Command::ChangeRoom { .. })
        ));
    }

    #[test]
    fn f_with_a_bad_id_is_unknown_not_continue() {
        assert_eq!(
            hardening("f 10"),
            Some(Command::ReadForward { after: Some(10) })
        );
        assert!(matches!(hardening("f 1o"), Some(Command::Unknown { .. })));
    }
}
