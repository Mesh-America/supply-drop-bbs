//! Command parsing and response rendering for the Meshtastic transport.

use bbs_plugin_api::{event::Notification, Command, Keymap, Response};

/// Parse the raw text of an incoming direct message into a [`Command`].
///
/// A thin wrapper: the shared radio parser lives in
/// [`Command::parse_radio`], so MeshCore and Meshtastic cannot drift apart.
/// `keymap` is the active [`Keymap`], fetched fresh per message so a live
/// switch takes effect immediately. Returns `None` when the message should be
/// silently dropped (a prefix is configured and the message lacks it).
pub fn parse_command(
    text: &str,
    prefix: Option<char>,
    awaiting_reply: bool,
    keymap: &Keymap,
) -> Option<Command> {
    Command::parse_radio(text, prefix, awaiting_reply, keymap)
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
    use bbs_plugin_api::identity::Username;

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
        let km = Keymap::native_with_overrides(&[("a", bbs_plugin_api::KeymapAction::ChangeRoom)]);
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

    // ── Parser hardening (#409, #411, #412, #413) ────────────────────────

    fn hardening(text: &str) -> Option<Command> {
        parse_command(text, None, false, &Keymap::native())
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
