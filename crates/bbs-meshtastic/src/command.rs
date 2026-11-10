//! Command parsing and response rendering for the Meshtastic transport.

use bbs_plugin_api::command::{
    is_cancel_line, normalise_keyword, skip_leading_noise, split_first_word, trim_line,
};
use bbs_plugin_api::{event::Notification, identity::Username, Command, PermissionLevel, Response};

pub fn parse_command(text: &str, prefix: Option<char>, awaiting_reply: bool) -> Option<Command> {
    // `raw` is the payload form (trimmed of whitespace/NULs only). Invisible
    // characters are stripped from a probe copy for CANCEL/STOP and keyword
    // matching, never from payloads (workflow replies, passwords, bodies).
    let raw = trim_line(text);

    if is_cancel_line(raw) {
        return Some(Command::Cancel);
    }

    if awaiting_reply {
        return Some(Command::WorkflowReply {
            reply: raw.to_owned(),
        });
    }

    let text = if let Some(p) = prefix {
        if let Some(stripped) = skip_leading_noise(raw).strip_prefix(p) {
            skip_leading_noise(stripped)
        } else {
            return None;
        }
    } else {
        skip_leading_noise(raw)
    };

    if text.is_empty() {
        return Some(Command::Unknown { raw: String::new() });
    }

    let (word, rest) = split_first_word(text);
    let keyword = normalise_keyword(word);

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
        // Bare `s` scans; `s <text>` is Unknown, never a silent scan (#411, F-008) —
        // see bbs-plugin-api::Command::parse's identical arm for the reason.
        "s" => match rest {
            None => Some(Command::ScanMessages),
            Some(_) => Some(Command::Unknown {
                raw: text.to_owned(),
            }),
        },
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
    fn s_scans_only_when_bare_and_search_is_the_user_search() {
        assert_eq!(hardening("s"), Some(Command::ScanMessages));
        assert!(matches!(hardening("s foo"), Some(Command::Unknown { .. })));
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

    // ── Audit regressions (F-001, F-005, F-006, F-008) ───────────────────

    #[test]
    fn payloads_keep_zero_width_joiners_when_awaiting_reply() {
        for text in [
            "family \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "mi\u{200C}xed",
            "pw\u{200B}x\u{FEFF}",
        ] {
            assert_eq!(
                parse_command(text, None, true),
                Some(Command::WorkflowReply {
                    reply: text.to_owned()
                }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn message_body_keeps_zwj_emoji() {
        let body = "\u{1F468}\u{200D}\u{1F469}";
        let line = format!("e {body}");
        assert_eq!(
            parse_command(&line, None, false),
            Some(Command::EnterMessage {
                body: Some(body.to_owned())
            })
        );
    }

    #[test]
    fn cancel_survives_invisible_noise_and_nuls() {
        for awaiting in [true, false] {
            for text in [
                "cancel \u{200B}",
                "\u{FEFF} stop",
                "cancel\0\u{200B}",
                "\u{200B}\0 CANCEL \u{2060}",
                "st\u{00AD}op",
                "cancel\u{200E}",
                "cancel\u{FE0F}",
                "cancel\u{E0041}",
            ] {
                assert_eq!(
                    parse_command(text, None, awaiting),
                    Some(Command::Cancel),
                    "{text:?} awaiting={awaiting}"
                );
            }
        }
    }

    #[test]
    fn kelvin_sign_does_not_fold_to_k() {
        // U+212A lowercases to ASCII 'k' under Unicode folding; keywords use
        // ASCII folding only, so this is Unknown, not ListRooms.
        assert!(matches!(
            parse_command("\u{212A}", None, false),
            Some(Command::Unknown { .. })
        ));
        assert_eq!(parse_command("K", None, false), Some(Command::ListRooms));
    }

    #[test]
    fn s_with_text_is_unknown_not_a_scan() {
        assert_eq!(parse_command("s", None, false), Some(Command::ScanMessages));
        assert!(matches!(
            parse_command("s foo", None, false),
            Some(Command::Unknown { .. })
        ));
        assert_eq!(
            parse_command("search bob", None, false),
            Some(Command::SearchUsers {
                query: "bob".to_owned()
            })
        );
    }

    #[test]
    fn one_shot_passwords_keep_zero_width_characters() {
        for pw in ["a\u{200D}b", "pe\u{200C}rs", "x\u{200B}y\u{FEFF}"] {
            match parse_command(&format!("register bob {pw}"), None, false) {
                Some(Command::RegisterOneShot { password, .. }) => {
                    assert_eq!(password.expose(), pw)
                }
                other => panic!("unexpected {other:?}"),
            }
            match parse_command(&format!("login bob {pw}"), None, false) {
                Some(Command::LoginOneShot { password, .. }) => {
                    assert_eq!(password.expose(), pw)
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    /// Mirrors `bbs-mesh`'s `sysop_words_match_canonical_parser`: this parser
    /// must agree with the canonical one on the audit (#411-#413) inputs.
    #[test]
    fn hardening_inputs_match_canonical_parser() {
        for text in [
            "f\u{00A0}12",
            "c\u{00A0}lobby",
            "cancel \u{200B}",
            "\u{FEFF} stop",
            "cancel\0\u{200B}",
            "f abc",
            "f 10",
            "s",
            "s text",
            "search text",
            "search",
            "\u{212A}",
            "timeout bob 1",
            "guestroom Lobby",
        ] {
            for awaiting in [false, true] {
                assert_eq!(
                    parse_command(text, None, awaiting),
                    Some(Command::parse(text, awaiting)),
                    "{text:?} awaiting={awaiting}"
                );
            }
        }
    }

    #[test]
    fn trailing_nul_is_trimmed_like_meshcore() {
        assert_eq!(parse_command("N\0", None, false), Some(Command::ReadNew));
    }
}
