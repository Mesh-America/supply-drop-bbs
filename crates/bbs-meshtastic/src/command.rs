//! Command parsing and response rendering for the Meshtastic transport.

use bbs_plugin_api::{event::Notification, identity::Username, Command, Keymap, Response};

/// Parse the raw text of an incoming direct message into a [`Command`].
///
/// See `crates/bbs-mesh/src/command.rs`'s `parse_command` doc comment — this
/// is Meshtastic's copy of the same design (GH #354 Phase 2): one-shot
/// `register`/`login` stays here (radio-only, ahead of any keymap lookup;
/// reserved so a keymap can never bind it), everything else delegates to
/// [`Command::parse_with_keymap`].
pub fn parse_command(
    text: &str,
    prefix: Option<char>,
    awaiting_reply: bool,
    keymap: &Keymap,
) -> Option<Command> {
    let text = text.trim();

    if matches!(text.to_ascii_lowercase().as_str(), "cancel" | "stop") {
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
    let keyword = word.to_ascii_lowercase();

    match keyword.as_str() {
        // `register <user>` → interactive; `register <user> <password>` → one-shot.
        "register" => Some(match rest {
            Some(r) => {
                let (name, password) = split_first_word(r);
                if name.is_empty() {
                    Command::Help {
                        topic: Some("register".to_owned()),
                    }
                } else if let Some(password) = password {
                    Command::RegisterOneShot {
                        username: name.to_owned(),
                        password: password.into(),
                    }
                } else {
                    Command::Register {
                        username: name.to_owned(),
                    }
                }
            }
            None => Command::Help {
                topic: Some("register".to_owned()),
            },
        }),
        // `login <user>` → interactive; `login <user> <password>` → one-shot.
        "login" => Some(match rest {
            Some(r) => {
                let (name, password) = split_first_word(r);
                match (Username::new(name).ok(), password) {
                    (Some(username), Some(password)) => Command::LoginOneShot {
                        username,
                        password: password.into(),
                    },
                    (Some(username), None) => Command::Login { username },
                    (None, _) => Command::Help {
                        topic: Some("login".to_owned()),
                    },
                }
            }
            None => Command::Help {
                topic: Some("login".to_owned()),
            },
        }),
        // Every other keyword goes through the canonical, keymap-aware
        // parser. `awaiting_reply` is always `false` here.
        _ => Some(Command::parse_with_keymap(text, false, keymap)),
    }
}

fn split_first_word(s: &str) -> (&str, Option<&str>) {
    match s.find(|c: char| c.is_ascii_whitespace()) {
        None => (s, None),
        Some(i) => {
            let rest = s[i..].trim_start();
            (&s[..i], if rest.is_empty() { None } else { Some(rest) })
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
        assert_eq!(
            parse_command("hello", Some('!'), false, &Keymap::native()),
            None
        );
        assert!(matches!(
            parse_command("!h", Some('!'), false, &Keymap::native()),
            Some(Command::Help { .. })
        ));
    }

    #[test]
    fn workflow_reply_beats_prefix() {
        assert_eq!(
            parse_command("secret", Some('!'), true, &Keymap::native()),
            Some(Command::WorkflowReply {
                reply: "secret".to_owned()
            })
        );
    }

    #[test]
    fn timeout_user_and_days() {
        let u = Username::new("bob").unwrap();
        assert_eq!(
            parse_command("timeout bob 3", None, false, &Keymap::native()),
            Some(Command::TimeoutUser {
                username: u.clone(),
                days: 3
            })
        );
        assert_eq!(
            parse_command("TIMEOUT bob 5", None, false, &Keymap::native()),
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
                parse_command(text, None, false, &Keymap::native()),
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
            parse_command("openaccess", None, false, &Keymap::native()),
            Some(Command::OpenAccess)
        );
        assert_eq!(
            parse_command("CloseAccess", None, false, &Keymap::native()),
            Some(Command::CloseAccess)
        );
        assert_eq!(
            parse_command("guestroom Lobby", None, false, &Keymap::native()),
            Some(Command::SetGuestRoom {
                name: Some("Lobby".to_owned())
            })
        );
        assert_eq!(
            parse_command("guestroom OFF", None, false, &Keymap::native()),
            Some(Command::SetGuestRoom { name: None })
        );
        assert_eq!(
            parse_command("guestroom", None, false, &Keymap::native()),
            Some(Command::Unknown {
                raw: "guestroom".to_owned()
            })
        );
    }

    /// The radio parser must agree with the canonical `Command::parse` on the
    /// sysop/aide words — they drifted once (timeout, openaccess, closeaccess,
    /// guestroom fell through to Unknown on radio). GH #354 Phase 2 made this
    /// parity structural (everything but register/login now delegates to
    /// `Command::parse_with_keymap`), so this also covers keywords that
    /// never drifted, as a belt-and-braces regression guard.
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
            ".aide bob",
            ".sysop bob",
            ".user bob",
            ".aide",
            ".pw bob",
            "ban bob",
            "unban bob",
            "whois bob",
            "k",
            "g",
            "c lobby",
            "s",
            "s query text",
            "d 5",
            "help topic",
        ] {
            assert_eq!(
                parse_command(text, None, false, &Keymap::native()),
                Some(Command::parse(text, false)),
                "{text}"
            );
        }
    }

    /// GH #354 Phase 2: a keymap override now reaches the Meshtastic parser
    /// too, not just the canonical `Command::parse`.
    #[test]
    fn keymap_override_reaches_the_meshtastic_parser() {
        let km = Keymap {
            name: "test".to_owned(),
            description: "test".to_owned(),
            bindings: std::collections::BTreeMap::from([(
                "a".to_owned(),
                bbs_plugin_api::KeymapAction::ChangeRoom,
            )]),
        };
        assert_eq!(
            parse_command("a Lobby", None, false, &km),
            Some(Command::ChangeRoom {
                target: "Lobby".to_owned()
            })
        );
        assert_eq!(
            parse_command("register alice", None, false, &km),
            Some(Command::Register {
                username: "alice".to_owned()
            })
        );
        assert_eq!(
            parse_command("cancel", None, false, &km),
            Some(Command::Cancel)
        );
    }

    #[test]
    fn truncate_preserves_utf8() {
        assert_eq!(truncate_utf8("ab😀cd", 5), "ab");
    }
}
