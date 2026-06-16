//! Command parsing and response rendering for the Meshtastic transport.

use bbs_plugin_api::{event::Notification, identity::Username, Command, Response};

pub fn parse_command(text: &str, prefix: Option<char>, awaiting_reply: bool) -> Option<Command> {
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
        "whoami" => Some(Command::Whoami),
        "k" => Some(Command::ListRooms),
        "g" => Some(Command::GoNextUnread),
        "c" => Some(Command::ChangeRoom {
            target: rest.unwrap_or("").to_owned(),
        }),
        "m" => Some(Command::GoMail),
        "n" => Some(Command::ReadNew),
        "f" => Some(Command::ReadForward {
            after: rest.and_then(|s| s.parse::<i64>().ok()),
        }),
        "r" => Some(Command::ReadReverse),
        "s" => match rest {
            Some(q) if !q.is_empty() => Some(Command::SearchUsers {
                query: q.to_owned(),
            }),
            _ => Some(Command::ScanMessages),
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
        _ => Some(Command::Unknown {
            raw: text.to_owned(),
        }),
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
    fn truncate_preserves_utf8() {
        assert_eq!(truncate_utf8("ab😀cd", 5), "ab");
    }
}
