//! Command keymaps: which keywords trigger which BBS action.
//!
//! See `specs/002-command-keymaps/` (spec.md, `plan-action-keymap.md`, the
//! sourced `research-classic-bbs-commands.md`) for the design this
//! implements. GH #354.
//!
//! ## Design: complete tables, only the active keys work
//!
//! A [`Keymap`] is a **complete** table. It lists every [`KeymapAction`], each
//! with an ordered list of keywords. The first keyword of an action is its
//! **primary** key, the one user-facing messages show. Only the keywords in
//! the active keymap work: a native key a preset does not list is unknown
//! under that preset. [`Keymap::native`] is one such table, not a special
//! case.
//!
//! Some commands are not actions at all and keep fixed words in every keymap:
//! help, the account and admin commands, and the `quit`/`exit`/`bye`/`logout`
//! synonyms (so a user can always get out). Those words are reserved: a
//! keymap may not bind them ([`KeymapError::ReservedKeyword`]).
//!
//! Actions come in two namespaces. The top-level actions are typed at the
//! command prompt. The `Reading*` actions are typed while reading messages
//! one at a time. The same word may be bound in both namespaces (native `F`
//! is both "read forward" and "next message"), but never twice within one.
//!
//! [`Command::parse_with_keymap`](crate::Command::parse_with_keymap) applies
//! the keymap when parsing a line.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An action a keymap binds keywords to. A closed set: the commands a preset
/// has reason to remap. Admin and account commands are out of scope for v1
/// (spec.md open question 5).
///
/// The five `Reading*` variants are consulted only while reading messages
/// one at a time. Each covers both its bare form and its argument form
/// (`F` and `F <id>` are both `ReadingForward`).
///
/// `Serialize`/`Deserialize` use the variant name verbatim (`"Quit"`,
/// `"ChangeRoom"`, ...), which is the wire format for a custom keymap file's
/// `[bindings]` table. An unrecognised action name is a deserialization
/// error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum KeymapAction {
    /// Log out / end the session. Native: `q`.
    Quit,
    /// List the rooms on the board. Native: `k`.
    ListRooms,
    /// Jump to the next room with unread messages. Native: `g`.
    GoNextUnread,
    /// Change to a named/numbered room. Native: `c <target>`.
    ChangeRoom,
    /// Go to the Mail room. Native: `m`.
    GoMail,
    /// Read new (unread) messages in the current room. Native: `n`.
    ReadNew,
    /// Read forward, optionally jumping to a message id first. Native:
    /// `f [id]`.
    ReadForward,
    /// Read backward from the most recent message. Native: `r`.
    ReadReverse,
    /// List messages in the current room without fully reading them.
    /// Native: `s`.
    ScanMessages,
    /// Post a new message. Native: `e [body]`.
    EnterMessage,
    /// Delete a message by id. Native: `d <id>`.
    DeleteMessage,
    /// List who's currently online. Native: `w`.
    WhoIsOnline,
    /// While reading: next message, or jump to a message with `<key> <id>`.
    ReadingForward,
    /// While reading: previous message, or jump back to a message with
    /// `<key> <id>`.
    ReadingReverse,
    /// While reading: reply to the message on screen, or start the reply
    /// with `<key> <text>`.
    ReadingReply,
    /// While reading: show reading-mode help.
    ReadingHelp,
    /// While reading: delete the message on screen, or a specific one with
    /// `<key> <id>`.
    ReadingDelete,
}

impl KeymapAction {
    /// Every variant, for completeness checks (`Keymap::validate`).
    pub const ALL: &'static [KeymapAction] = &[
        KeymapAction::Quit,
        KeymapAction::ListRooms,
        KeymapAction::GoNextUnread,
        KeymapAction::ChangeRoom,
        KeymapAction::GoMail,
        KeymapAction::ReadNew,
        KeymapAction::ReadForward,
        KeymapAction::ReadReverse,
        KeymapAction::ScanMessages,
        KeymapAction::EnterMessage,
        KeymapAction::DeleteMessage,
        KeymapAction::WhoIsOnline,
        KeymapAction::ReadingForward,
        KeymapAction::ReadingReverse,
        KeymapAction::ReadingReply,
        KeymapAction::ReadingHelp,
        KeymapAction::ReadingDelete,
    ];

    /// Whether this action is one of the `Reading*` variants, which live in
    /// their own namespace (see the module docs).
    #[must_use]
    pub fn is_reading_only(self) -> bool {
        matches!(
            self,
            KeymapAction::ReadingForward
                | KeymapAction::ReadingReverse
                | KeymapAction::ReadingReply
                | KeymapAction::ReadingHelp
                | KeymapAction::ReadingDelete
        )
    }
}

/// Why a [`Keymap`] failed [`Keymap::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeymapError {
    /// `action` has no keyword, so the user could never trigger it.
    MissingAction(KeymapAction),
    /// The same keyword is bound to two actions in one namespace.
    DuplicateKeyword {
        /// The shared keyword.
        keyword: String,
        /// The first action bound to it.
        first: KeymapAction,
        /// The second action bound to it.
        second: KeymapAction,
    },
    /// The keyword is one `Command::parse_with_keymap` keeps for a command
    /// that is not a remappable action (an account or admin command, a help
    /// alias, a quit synonym, or the CANCEL/STOP fast path). Binding it would
    /// make that command unreachable.
    ReservedKeyword(String),
    /// The keyword is not in the shape the parser looks up: non-empty, no
    /// whitespace, no zero-width characters, already lowercase. It could
    /// never match a typed keyword.
    MalformedKeyword(String),
    /// An `unsupported` entry uses a keyword that is also bound to an action
    /// or reserved, so the friendly reply could never be reached.
    UnsupportedKeywordInUse(String),
}

impl std::fmt::Display for KeymapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeymapError::MissingAction(action) => write!(
                f,
                "{action:?} has no keyword in this keymap, so it could never be used. \
                 Every action needs at least one key"
            ),
            KeymapError::DuplicateKeyword {
                keyword,
                first,
                second,
            } => write!(
                f,
                "\"{keyword}\" is bound to both {first:?} and {second:?}; one keyword can \
                 only trigger one action"
            ),
            KeymapError::ReservedKeyword(keyword) => write!(
                f,
                "\"{keyword}\" is already used by a command this keymap can't remap \
                 (an account or admin command, a help alias, a quit synonym, or \
                 cancel/stop). Binding it here would make that command unreachable"
            ),
            KeymapError::MalformedKeyword(keyword) => write!(
                f,
                "\"{keyword}\" is not a valid keyword. It must be non-empty, contain no \
                 whitespace or zero-width characters, and already be lowercase, or it can \
                 never match what a user types"
            ),
            KeymapError::UnsupportedKeywordInUse(keyword) => write!(
                f,
                "\"{keyword}\" is listed as unsupported but is also bound to an action or \
                 reserved, so its message could never be shown"
            ),
        }
    }
}

impl std::error::Error for KeymapError {}

/// A named, complete keymap. See the module docs.
///
/// `Serialize`/`Deserialize` are the TOML format for a custom keymap file,
/// the same shape as the built-in presets:
///
/// ```toml
/// name = "my-bbs"
/// description = "..."
/// no_source_equivalent = ["ListRooms"]
///
/// [bindings]
/// Quit = ["g", "leave"]
/// ChangeRoom = ["a"]
/// # ...every action, see KeymapAction::ALL
///
/// [unsupported]
/// j = "No file areas on this BBS."
/// ```
///
/// `deny_unknown_fields` rejects a typo'd top-level key at parse time. An
/// unrecognised action name in `bindings` is likewise a parse error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keymap {
    /// Short identifier, e.g. `"native"`, `"maximus"`. Matches the `[bbs]
    /// keymap = "..."` config value.
    pub name: String,
    /// Human-readable description shown to the sysop when choosing a keymap.
    /// It should say what the preset covers and what it does not.
    pub description: String,
    /// Action to its keywords, primary first. Must contain every action
    /// (see [`Keymap::validate`]).
    pub bindings: BTreeMap<KeymapAction, Vec<String>>,
    /// Top-level keywords the source BBS has but this one does not, with a
    /// short reply for the user (for example `j` on the Maximus preset:
    /// "No file areas on this BBS."). Optional.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unsupported: BTreeMap<String, String>,
    /// Actions whose keys were chosen only because every action needs one,
    /// not because the source BBS has an equivalent. Documentation for the
    /// sysop; never presented as authentic.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_source_equivalent: Vec<KeymapAction>,
}

impl Keymap {
    /// Build a keymap from a full list of `(action, keywords)` pairs.
    fn build(
        name: &str,
        description: &str,
        bindings: &[(KeymapAction, &[&str])],
        unsupported: &[(&str, &str)],
        no_source_equivalent: &[KeymapAction],
    ) -> Self {
        Keymap {
            name: name.to_owned(),
            description: description.to_owned(),
            bindings: bindings
                .iter()
                .map(|(a, ks)| (*a, ks.iter().map(|k| (*k).to_owned()).collect()))
                .collect(),
            unsupported: unsupported
                .iter()
                .map(|(k, m)| ((*k).to_owned(), (*m).to_owned()))
                .collect(),
            no_source_equivalent: no_source_equivalent.to_vec(),
        }
    }

    /// Supply Drop's own default keymap.
    #[must_use]
    pub fn native() -> Self {
        use KeymapAction as A;
        Self::build(
            "native",
            "Supply Drop's own default commands.",
            &[
                (A::Quit, &["q"]),
                (A::ListRooms, &["k"]),
                (A::GoNextUnread, &["g"]),
                (A::ChangeRoom, &["c"]),
                (A::GoMail, &["m"]),
                (A::ReadNew, &["n"]),
                (A::ReadForward, &["f"]),
                (A::ReadReverse, &["r"]),
                (A::ScanMessages, &["s"]),
                (A::EnterMessage, &["e"]),
                (A::DeleteMessage, &["d"]),
                (A::WhoIsOnline, &["w"]),
                (A::ReadingForward, &["f"]),
                (A::ReadingReverse, &["r"]),
                (A::ReadingReply, &["e"]),
                (A::ReadingHelp, &["h", "?"]),
                (A::ReadingDelete, &["d"]),
            ],
            &[],
            &[],
        )
    }

    /// Native keymap with the given keywords moved onto the given actions.
    ///
    /// Each `(keyword, action)` pair removes `keyword` from whatever action
    /// held it and makes it the only keyword of `action`. The result is not
    /// guaranteed to validate (the displaced action may be left with no
    /// key). Meant for tests and tools that need a small variation on
    /// native, not for shipping presets.
    #[must_use]
    pub fn native_with_overrides(overrides: &[(&str, KeymapAction)]) -> Self {
        let mut km = Self::native();
        km.name = "custom".to_owned();
        for (keyword, action) in overrides {
            for (a, keywords) in km.bindings.iter_mut() {
                if a.is_reading_only() == action.is_reading_only() {
                    keywords.retain(|k| k != keyword);
                }
            }
            km.bindings.insert(*action, vec![(*keyword).to_owned()]);
        }
        km
    }

    /// The Maximus-style preset. Keys are from Maximus 3.0x's shipped menu file
    /// (`ctl/menus.ctl`): the hotkey of a menu option is the first letter of
    /// its description. Maximus is menu based and has no separate reading
    /// mode, so the Message menu keys serve as the reading keys. Maximus has
    /// no mail room, no "next room with unread" and no list-rooms key, so
    /// those use plain words and are listed in `no_source_equivalent`.
    #[must_use]
    pub fn maximus() -> Self {
        use KeymapAction as A;
        Self::build(
            "maximus",
            "Maximus-style keys: G(oodbye), A(rea change), L(ist brief), N/P next and \
             previous, E(nter), R(eply) and K(ill) while reading. Keys only: no file \
             areas, menus, chat, bulletins or terminal settings. Maximus has no mail \
             room, list-rooms or next-unread key, so those use the words MAIL, ROOMS \
             and READ, and ] (next area) stands in for next unread.",
            &[
                (A::Quit, &["g"]),
                (A::ListRooms, &["rooms"]),
                (A::GoNextUnread, &["]"]),
                (A::ChangeRoom, &["a"]),
                (A::GoMail, &["mail"]),
                (A::ReadNew, &["n"]),
                (A::ReadForward, &["read"]),
                (A::ReadReverse, &["p"]),
                (A::ScanMessages, &["l"]),
                (A::EnterMessage, &["e"]),
                (A::DeleteMessage, &["k"]),
                (A::WhoIsOnline, &["w"]),
                (A::ReadingForward, &["n"]),
                (A::ReadingReverse, &["p"]),
                (A::ReadingReply, &["r"]),
                (A::ReadingHelp, &["?"]),
                (A::ReadingDelete, &["k"]),
            ],
            &[
                ("f", "No file areas or forwarding on this BBS."),
                ("j", "No file areas on this BBS."),
                ("y", "No sysop paging on this BBS."),
                ("o", "No offline reader on this BBS."),
                ("t", "No message tagging on this BBS."),
                ("c", "No terminal settings on this BBS."),
                ("s", "No statistics screen on this BBS."),
                ("m", "Rooms are flat here. Use A to change room."),
                ("/", "No chat on this BBS."),
                ("=", "No nonstop reading on this BBS."),
                ("+", "No thread navigation on this BBS."),
                ("*", "No re-read command on this BBS."),
            ],
            &[A::ListRooms, A::GoNextUnread, A::GoMail, A::ReadForward],
        )
    }

    /// The packet-radio BBS (F6FBB and BPQ) style preset. Commands are from the
    /// F6FBB `docbbs.htm` and BPQ `BBSUserCommands.html` user command pages:
    /// `L` lists new headers, `LL n` the last n, `LM` your mail, `RN` (F6FBB)
    /// or `RM` (BPQ) reads new mail, `R n` reads a message, `K n` deletes one,
    /// `SR` replies. Packet BBS has no rooms and no reading mode, and bare `R`
    /// is undocumented, so room navigation and the stepping keys use plain
    /// words. `Q` sets your QTH there, so logoff is the word `LOGOFF` (and
    /// `BYE`, which works on every board).
    #[must_use]
    pub fn packet_bbs() -> Self {
        use KeymapAction as A;
        Self::build(
            "packet-bbs",
            "Packet BBS style keys (F6FBB and BPQ): L(ist new), LL (list last), LM (your \
             mail), RN or RM (read new mail), R <number> (read), K <number> (kill), SR \
             (reply). Packet BBS has no rooms, so room navigation uses the words ROOMS, \
             GOTO and NEXT. BYE logs off on every board. Sending mail is done in the Mail \
             room here, not with SP or SB, and there is no forwarding, bulletin \
             categories or node commands.",
            &[
                (A::Quit, &["logoff"]),
                (A::ListRooms, &["rooms"]),
                (A::GoNextUnread, &["next"]),
                (A::ChangeRoom, &["goto"]),
                (A::GoMail, &["lm"]),
                (A::ReadNew, &["rn", "rm"]),
                (A::ReadForward, &["r"]),
                (A::ReadReverse, &["back"]),
                (A::ScanMessages, &["l", "ll"]),
                (A::EnterMessage, &["e"]),
                (A::DeleteMessage, &["k"]),
                (A::WhoIsOnline, &["who"]),
                (A::ReadingForward, &["next"]),
                (A::ReadingReverse, &["back"]),
                (A::ReadingReply, &["sr"]),
                (A::ReadingHelp, &["h", "?"]),
                (A::ReadingDelete, &["k"]),
            ],
            &[
                ("q", "Type BYE to log off."),
                ("s", "Send from the Mail room: LM, then E @user message."),
                ("sp", "Send from the Mail room: LM, then E @user message."),
                ("sb", "No bulletin categories. Post in a room: GOTO name."),
                ("sc", "No message copying on this BBS."),
                ("km", "Delete mail in the Mail room with K <number>."),
                ("lb", "No bulletin lists. Type ROOMS to list rooms."),
                ("lc", "No category lists. Type ROOMS to list rooms."),
                ("lt", "No NTS traffic on this BBS."),
                ("lu", "Type RN to read new messages."),
                ("ln", "Type RN to read new messages."),
                ("lr", "Newest-first lists are not available. Type L."),
                ("n", "Set your name with PROFILE."),
                ("t", "No sysop paging on this BBS."),
                ("d", "To delete a message type K <number>."),
                ("w", "Type WHO to see who is online."),
                ("jk", "Type WHO to see who is online."),
                ("c", "No node or conference commands on this BBS."),
                ("node", "No node commands on this BBS."),
                ("nodes", "No node commands on this BBS."),
                ("i", "Use WHOIS <user> or PROFILE for user details."),
                ("x", "No expert mode on this BBS."),
                ("files", "No file areas on this BBS."),
                ("yapp", "No file transfers on this BBS."),
            ],
            &[
                A::Quit,
                A::ListRooms,
                A::GoNextUnread,
                A::ChangeRoom,
                A::ReadReverse,
                A::EnterMessage,
                A::WhoIsOnline,
                A::ReadingForward,
                A::ReadingReverse,
            ],
        )
    }

    /// The PCBoard-style preset. Commands are from the PCBoard 15.x manual
    /// (the kuehlbox.wtf wiki transcription): `G` goodbye, `J` join a
    /// conference, `Q` quick scan, `E` enter, `K` kill, `WHO`, `R;S` read new,
    /// `R;L` read from the last message backward, `R n` read message n, and at
    /// the end-of-message prompt `NEXT`, `PREV`, `RE` (reply) and `K`.
    /// PCBoard has no mail room (`Y` only scans your mail, used here as the
    /// nearest key), no list-rooms or next-unread command, and no documented
    /// help key at the end-of-message prompt. Its `M` (graphics mode) and `P`
    /// (page length) are answered with a message, never bound.
    #[must_use]
    pub fn pcboard() -> Self {
        use KeymapAction as A;
        Self::build(
            "pcboard",
            "PCBoard style keys: G(oodbye), J(oin conference), Q(uick scan), E(nter), \
             K(ill), WHO, R;S (read new), R;L (newest first), R <number>. Reading: NEXT, \
             PREV, RE (reply), K. Y (your mail) is the nearest key to going to Mail. \
             Keys only: no graphics mode, page length, file areas, doors or chat. PCBoard \
             has no list-rooms or next-unread key, so those use the words ROOMS and NEXT.",
            &[
                (A::Quit, &["g"]),
                (A::ListRooms, &["rooms"]),
                (A::GoNextUnread, &["next"]),
                (A::ChangeRoom, &["j"]),
                (A::GoMail, &["y"]),
                (A::ReadNew, &["r;s"]),
                (A::ReadForward, &["r"]),
                (A::ReadReverse, &["r;l"]),
                (A::ScanMessages, &["q"]),
                (A::EnterMessage, &["e"]),
                (A::DeleteMessage, &["k"]),
                (A::WhoIsOnline, &["who"]),
                (A::ReadingForward, &["next"]),
                (A::ReadingReverse, &["prev"]),
                (A::ReadingReply, &["re"]),
                (A::ReadingHelp, &["h", "?"]),
                (A::ReadingDelete, &["k"]),
            ],
            &[
                ("m", "Graphics mode is not available on this BBS."),
                ("p", "Page length is not available on this BBS."),
                ("d", "Downloads are not available on this BBS."),
                ("f", "File areas are not available on this BBS."),
                ("l", "File areas are not available on this BBS."),
                ("z", "File areas are not available on this BBS."),
                ("t", "Transfer protocols are not available on this BBS."),
                ("qwk", "Offline mail is not available on this BBS."),
                ("open", "Doors are not available on this BBS."),
                ("o", "Paging the sysop is not available on this BBS."),
                ("chat", "Chat is not available on this BBS."),
                ("node", "Chat is not available on this BBS."),
                ("news", "System news is not available on this BBS."),
                ("s", "Questionnaires are not available on this BBS."),
                ("select", "Conference flags are not available on this BBS."),
                ("a", "Conference flags are not available on this BBS."),
                ("x", "Expert mode is not available on this BBS."),
                ("menu", "Menus are not available on this BBS."),
                ("lang", "Language settings are not available on this BBS."),
                ("alias", "Aliases are not available on this BBS."),
                ("c", "Use E @user to write to a user."),
                ("w", "Use PASSWD to change your password."),
                ("ts", "Message text search is not available on this BBS."),
                ("reply", "Read the message first, then type RE to reply."),
                ("user", "Use SEARCH <name> to find users."),
            ],
            &[A::ListRooms, A::GoNextUnread, A::ReadingHelp],
        )
    }

    /// The WWIV-style preset. Keys are from WWIV 5.x's shipped menu and read
    /// prompt (`main.mnu.json`, `mbmain.msg`, `msgscan.cpp`). Telegard and
    /// Renegade share some read-prompt keys, but their defaults are not
    /// verified and are not covered. WWIV's `H` (hop to a sub) is this BBS's
    /// help key and cannot be bound.
    #[must_use]
    pub fn wwiv_family() -> Self {
        use KeymapAction as A;
        Self::build(
            "wwiv-family",
            "WWIV-style keys (WWIV 5.x; Telegard and Renegade are not covered): * (list \
             subs), N(ew), S(can), P(ost), M(ail), R(emove your post), O(ff), //WHO, - \
             (back) and W (reply) while reading. WWIV's H (hop to a sub) is help here, so \
             use HOP. No file transfer, doors, chat, voting or conferences.",
            &[
                (A::Quit, &["o"]),
                (A::ListRooms, &["*"]),
                (A::GoNextUnread, &["unread"]),
                (A::ChangeRoom, &["hop"]),
                (A::GoMail, &["m"]),
                (A::ReadNew, &["n"]),
                (A::ReadForward, &["next"]),
                (A::ReadReverse, &["back"]),
                (A::ScanMessages, &["s"]),
                (A::EnterMessage, &["p"]),
                (A::DeleteMessage, &["r"]),
                (A::WhoIsOnline, &["//who"]),
                (A::ReadingForward, &["]"]),
                (A::ReadingReverse, &["-"]),
                (A::ReadingReply, &["w"]),
                (A::ReadingHelp, &["?"]),
                (A::ReadingDelete, &["d"]),
            ],
            &[
                ("t", "No file areas on this BBS."),
                (".", "No doors on this BBS."),
                ("c", "No chat on this BBS."),
                ("a", "No automessage on this BBS."),
                ("g", "No bulletins on this BBS."),
                ("f", "No feedback command on this BBS."),
                ("d", "No user settings menu on this BBS."),
                ("x", "No expert mode on this BBS."),
                ("l", "No caller list on this BBS."),
                ("i", "No system info page on this BBS."),
                ("j", "No conferences on this BBS."),
                ("q", "No quick scan. Use N to read new messages."),
                ("e", "To send mail: type M, then P @user message."),
            ],
            &[
                A::GoNextUnread,
                A::ChangeRoom,
                A::ReadForward,
                A::ReadReverse,
                A::ReadingDelete,
            ],
        )
    }

    /// The Synchronet-style preset, from the default command shell
    /// (`exec/default.js`) and the reading prompt in `readmsgs.cpp`. `E`
    /// opens Synchronet's e-mail menu, which here goes to the Mail room;
    /// Synchronet has no next-unread, newest-first or delete-by-id key at the
    /// main prompt, so those use plain words.
    #[must_use]
    pub fn synchronet() -> Self {
        use KeymapAction as A;
        Self::build(
            "synchronet",
            "Synchronet-style keys: * (list sub-boards), J(ump), N(ew), R(ead), L(ist), \
             P(ost), E (mail), W(ho), O(ff). While reading: + next, - back, A reply, D \
             delete. No file libraries, doors, chat, QWK, polls or time bank. Next \
             unread, newest first and delete use the words UNREAD, BACK and DELETE.",
            &[
                (A::Quit, &["o"]),
                (A::ListRooms, &["*"]),
                (A::GoNextUnread, &["unread"]),
                (A::ChangeRoom, &["j"]),
                (A::GoMail, &["e"]),
                (A::ReadNew, &["n"]),
                (A::ReadForward, &["r"]),
                (A::ReadReverse, &["back"]),
                (A::ScanMessages, &["l"]),
                (A::EnterMessage, &["p"]),
                (A::DeleteMessage, &["delete"]),
                (A::WhoIsOnline, &["w"]),
                (A::ReadingForward, &["+"]),
                (A::ReadingReverse, &["-"]),
                (A::ReadingReply, &["a"]),
                (A::ReadingHelp, &["?"]),
                (A::ReadingDelete, &["d"]),
            ],
            &[
                ("t", "No file libraries on this BBS."),
                ("x", "No doors on this BBS."),
                ("c", "No chat on this BBS."),
                ("q", "No QWK packets on this BBS."),
                ("m", "No time bank on this BBS."),
                ("g", "No text files on this BBS."),
                ("i", "No information menu on this BBS."),
                ("f", "No text search on this BBS."),
                ("s", "Mail is a room here: type E for your mail."),
                ("d", "No user settings menu on this BBS."),
                ("a", "No auto-message on this BBS."),
                ("z", "No continuous scan on this BBS."),
            ],
            &[A::GoNextUnread, A::ReadReverse, A::DeleteMessage],
        )
    }

    /// Look up a built-in preset by name, the string a `[bbs] keymap = "..."`
    /// config value or `config set-keymap <name>` carries. `"native"` returns
    /// [`Keymap::native`]. An unrecognised name returns `None`.
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "native" => Some(Self::native()),
            "maximus" => Some(Self::maximus()),
            "packet-bbs" => Some(Self::packet_bbs()),
            "pcboard" => Some(Self::pcboard()),
            "wwiv-family" => Some(Self::wwiv_family()),
            "synchronet" => Some(Self::synchronet()),
            _ => None,
        }
    }

    /// Every built-in preset name, `"native"` first.
    pub const BUILTIN_NAMES: &'static [&'static str] = &[
        "native",
        "maximus",
        "packet-bbs",
        "pcboard",
        "wwiv-family",
        "synchronet",
    ];

    /// The keywords bound to `action`, primary first. Empty for an action a
    /// (not yet validated) keymap does not list.
    #[must_use]
    pub fn keywords(&self, action: KeymapAction) -> &[String] {
        self.bindings.get(&action).map_or(&[], Vec::as_slice)
    }

    /// The primary key of `action`, the one messages should show. Falls back
    /// to the native key for an action a keymap does not list, which cannot
    /// happen for a keymap that passed [`Keymap::validate`].
    #[must_use]
    pub fn primary(&self, action: KeymapAction) -> &str {
        self.keywords(action).first().map_or_else(
            || {
                static NATIVE: std::sync::OnceLock<Keymap> = std::sync::OnceLock::new();
                let native = NATIVE.get_or_init(Keymap::native);
                native.keywords(action).first().map_or("", String::as_str)
            },
            String::as_str,
        )
    }

    /// The key to show a user for `action` in a message: its primary key in
    /// upper case (`"N"`, `"//WHO"`). Every user-facing message that names a
    /// key must get it from here so it names the key that works on this
    /// board, not the native one.
    #[must_use]
    pub fn key(&self, action: KeymapAction) -> String {
        self.primary(action).to_uppercase()
    }

    /// [`Keymap::key`] followed by an argument placeholder, for example
    /// `key_with(ReadForward, "<id>")` gives `"F <id>"`.
    #[must_use]
    pub fn key_with(&self, action: KeymapAction, arg: &str) -> String {
        format!("{} {arg}", self.key(action))
    }

    /// The top-level action bound to `keyword`, if any. `keyword` must
    /// already be lowercase.
    #[must_use]
    pub fn action_for(&self, keyword: &str) -> Option<KeymapAction> {
        self.lookup(keyword, false)
    }

    /// The reading-mode action bound to `keyword`, if any. `keyword` must
    /// already be lowercase.
    #[must_use]
    pub fn reading_action_for(&self, keyword: &str) -> Option<KeymapAction> {
        self.lookup(keyword, true)
    }

    fn lookup(&self, keyword: &str, reading: bool) -> Option<KeymapAction> {
        self.bindings
            .iter()
            .find(|(a, ks)| a.is_reading_only() == reading && ks.iter().any(|k| k == keyword))
            .map(|(a, _)| *a)
    }

    /// The friendly reply for a keyword this BBS does not support, if the
    /// keymap declares one. `keyword` must already be lowercase.
    #[must_use]
    pub fn unsupported_message(&self, keyword: &str) -> Option<&str> {
        self.unsupported.get(keyword).map(String::as_str)
    }

    /// Total number of keywords across all actions, for status output.
    #[must_use]
    pub fn keyword_count(&self) -> usize {
        self.bindings.values().map(Vec::len).sum()
    }

    /// Check that the keymap is complete and every keyword is usable:
    ///
    /// - every [`KeymapAction`] has at least one keyword,
    /// - every keyword is well formed (non-empty, no whitespace or
    ///   zero-width characters, already lowercase),
    /// - no top-level keyword is reserved (see [`is_reserved_keyword`]),
    /// - no keyword is bound to two actions within one namespace,
    /// - no `unsupported` keyword is bound or reserved.
    ///
    /// # Errors
    /// The first problem found, in the order above. Returns one at a time to
    /// keep the check simple; fix and call again for the next.
    pub fn validate(&self) -> Result<(), KeymapError> {
        for action in KeymapAction::ALL.iter().copied() {
            if self.keywords(action).is_empty() {
                return Err(KeymapError::MissingAction(action));
            }
        }

        for (action, keywords) in &self.bindings {
            for keyword in keywords {
                if !is_well_formed(keyword) {
                    return Err(KeymapError::MalformedKeyword(keyword.clone()));
                }
                if !action.is_reading_only() && is_reserved_keyword(keyword) {
                    return Err(KeymapError::ReservedKeyword(keyword.clone()));
                }
            }
        }

        for reading in [false, true] {
            let mut seen: BTreeMap<&str, KeymapAction> = BTreeMap::new();
            for (action, keywords) in &self.bindings {
                if action.is_reading_only() != reading {
                    continue;
                }
                for keyword in keywords {
                    if let Some(&first) = seen.get(keyword.as_str()) {
                        if first != *action {
                            return Err(KeymapError::DuplicateKeyword {
                                keyword: keyword.clone(),
                                first,
                                second: *action,
                            });
                        }
                    }
                    seen.insert(keyword.as_str(), *action);
                }
            }
        }

        for keyword in self.unsupported.keys() {
            if !is_well_formed(keyword) {
                return Err(KeymapError::MalformedKeyword(keyword.clone()));
            }
            if is_reserved_keyword(keyword) || self.action_for(keyword).is_some() {
                return Err(KeymapError::UnsupportedKeywordInUse(keyword.clone()));
            }
        }
        Ok(())
    }
}

/// Non-empty, no whitespace, no zero-width characters, already lowercase.
fn is_well_formed(keyword: &str) -> bool {
    !keyword.is_empty()
        && keyword == keyword.to_lowercase()
        && !keyword.chars().any(|c| {
            c.is_whitespace() || matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}')
        })
}

/// Whether `keyword` is one `Command::parse_with_keymap` keeps for a command
/// that is not a remappable action (fixed in every keymap).
#[must_use]
pub fn is_reserved_keyword(keyword: &str) -> bool {
    RESERVED_KEYWORDS.contains(&keyword)
}

/// Keywords the parser recognises for something other than a remappable
/// [`KeymapAction`]: help, account and admin commands, the quit synonyms
/// (kept so a user can always log out), and CANCEL/STOP, which the parser
/// intercepts before any keymap lookup.
///
/// This is the match in `command.rs`'s `parse_fixed`, word for word (plus
/// `cancel` and `stop`). Keep it in sync by hand;
/// the test `fixed_words_and_the_reserved_list_are_the_same_set` in
/// `command.rs` guards against drift.
pub(crate) const RESERVED_KEYWORDS: &[&str] = &[
    "h",
    "help",
    "?",
    "register",
    "login",
    "logout",
    "quit",
    "exit",
    "bye",
    "search",
    ".ff",
    "pending",
    "v",
    "b",
    "ban",
    "unban",
    "timeout",
    "u",
    "users",
    "whois",
    "whoami",
    "profile",
    "passwd",
    ".c",
    ".dr",
    ".er",
    ".eu",
    ".du",
    ".aide",
    ".sysop",
    ".user",
    ".pw",
    "openaccess",
    "closeaccess",
    "guestroom",
    "cancel",
    "stop",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn all_keymaps() -> Vec<Keymap> {
        Keymap::BUILTIN_NAMES
            .iter()
            .map(|n| Keymap::by_name(n).unwrap())
            .collect()
    }

    #[test]
    fn every_builtin_keymap_validates() {
        for km in all_keymaps() {
            assert_eq!(km.validate(), Ok(()), "preset {:?}", km.name);
        }
    }

    #[test]
    fn by_name_resolves_every_builtin_name_and_only_those() {
        assert_eq!(Keymap::by_name("not-a-real-preset"), None);
        for name in Keymap::BUILTIN_NAMES {
            let km = Keymap::by_name(name).expect(name);
            assert_eq!(&km.name, name);
        }
    }

    #[test]
    fn native_lists_every_action_with_its_documented_primary_key() {
        use KeymapAction as A;
        let native = Keymap::native();
        for (action, key) in [
            (A::Quit, "q"),
            (A::ListRooms, "k"),
            (A::GoNextUnread, "g"),
            (A::ChangeRoom, "c"),
            (A::GoMail, "m"),
            (A::ReadNew, "n"),
            (A::ReadForward, "f"),
            (A::ReadReverse, "r"),
            (A::ScanMessages, "s"),
            (A::EnterMessage, "e"),
            (A::DeleteMessage, "d"),
            (A::WhoIsOnline, "w"),
            (A::ReadingForward, "f"),
            (A::ReadingReverse, "r"),
            (A::ReadingReply, "e"),
            (A::ReadingHelp, "h"),
            (A::ReadingDelete, "d"),
        ] {
            assert_eq!(native.primary(action), key, "{action:?}");
        }
        // Every action is covered by the list above.
        assert_eq!(native.bindings.len(), KeymapAction::ALL.len());
    }

    #[test]
    fn primary_falls_back_to_native_for_an_unlisted_action() {
        let mut km = Keymap::maximus();
        km.bindings.remove(&KeymapAction::GoNextUnread);
        assert_eq!(km.primary(KeymapAction::GoNextUnread), "g");
    }

    #[test]
    fn key_is_the_upper_cased_primary() {
        let km = Keymap::maximus();
        assert_eq!(km.key(KeymapAction::Quit), "G");
        assert_eq!(km.key(KeymapAction::GoNextUnread), "]");
        assert_eq!(km.key_with(KeymapAction::ReadForward, "<id>"), "READ <id>");
        assert_eq!(
            Keymap::wwiv_family().key(KeymapAction::WhoIsOnline),
            "//WHO"
        );
    }

    #[test]
    fn primary_is_the_first_keyword() {
        let km = Keymap::native();
        assert_eq!(km.primary(KeymapAction::ReadingHelp), "h");
        assert_eq!(km.keywords(KeymapAction::ReadingHelp), ["h", "?"]);
    }

    #[test]
    fn top_level_and_reading_lookups_are_separate_namespaces() {
        let km = Keymap::native();
        assert_eq!(km.action_for("f"), Some(KeymapAction::ReadForward));
        assert_eq!(
            km.reading_action_for("f"),
            Some(KeymapAction::ReadingForward)
        );
        assert_eq!(km.action_for("?"), None);
        assert_eq!(km.reading_action_for("?"), Some(KeymapAction::ReadingHelp));
    }

    #[test]
    fn a_preset_drops_the_native_keys_it_replaces() {
        let km = Keymap::maximus();
        assert_eq!(km.action_for("g"), Some(KeymapAction::Quit));
        assert_eq!(km.action_for("]"), Some(KeymapAction::GoNextUnread));
        assert_eq!(km.action_for("q"), None, "native q must not carry over");
        assert_eq!(km.action_for("s"), None, "native s must not carry over");
        assert_eq!(km.primary(KeymapAction::ScanMessages), "l");
    }

    #[test]
    fn pcboard_never_binds_its_trap_keys_to_a_different_meaning() {
        // PCBoard M = graphics mode, P = page length; neither may become a
        // Supply Drop action. They are answered with a friendly message.
        let km = Keymap::pcboard();
        assert_eq!(km.action_for("m"), None);
        assert_eq!(km.action_for("p"), None);
        assert!(km.unsupported_message("m").is_some());
        assert!(km.unsupported_message("p").is_some());
        // Q = quick scan is a real PCBoard meaning and maps to scan.
        assert_eq!(km.action_for("q"), Some(KeymapAction::ScanMessages));
    }

    #[test]
    fn a_keymap_missing_an_action_is_rejected() {
        let mut km = Keymap::native();
        km.bindings.remove(&KeymapAction::GoNextUnread);
        assert_eq!(
            km.validate(),
            Err(KeymapError::MissingAction(KeymapAction::GoNextUnread))
        );
        let mut km = Keymap::native();
        km.bindings.insert(KeymapAction::GoNextUnread, vec![]);
        assert_eq!(
            km.validate(),
            Err(KeymapError::MissingAction(KeymapAction::GoNextUnread))
        );
    }

    #[test]
    fn stealing_a_key_without_relocating_the_displaced_action_is_rejected() {
        // `g` moves to Quit; GoNextUnread is left with no key.
        let km = Keymap::native_with_overrides(&[("g", KeymapAction::Quit)]);
        assert_eq!(
            km.validate(),
            Err(KeymapError::MissingAction(KeymapAction::GoNextUnread))
        );
    }

    #[test]
    fn one_keyword_on_two_actions_in_one_namespace_is_rejected() {
        let mut km = Keymap::native();
        km.bindings
            .insert(KeymapAction::ListRooms, vec!["n".to_owned()]);
        match km.validate() {
            Err(KeymapError::DuplicateKeyword { keyword, .. }) => assert_eq!(keyword, "n"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_same_keyword_in_both_namespaces_is_fine() {
        // Native already does this (f, r, e, d).
        assert_eq!(Keymap::native().validate(), Ok(()));
    }

    #[test]
    fn binding_a_reserved_keyword_is_rejected() {
        for reserved in [
            "h", "b", "ban", "logout", "bye", "search", "cancel", "stop", ".ff",
        ] {
            let mut km = Keymap::native();
            km.bindings
                .insert(KeymapAction::ListRooms, vec![reserved.to_owned()]);
            assert_eq!(
                km.validate(),
                Err(KeymapError::ReservedKeyword(reserved.to_owned())),
                "{reserved}"
            );
        }
    }

    #[test]
    fn a_reading_action_may_use_a_word_reserved_at_the_top_level() {
        // `h` is help at the command prompt but ReadingHelp's native key.
        assert!(Keymap::native()
            .keywords(KeymapAction::ReadingHelp)
            .contains(&"h".to_owned()));
        assert_eq!(Keymap::native().validate(), Ok(()));
    }

    #[test]
    fn malformed_keywords_are_rejected() {
        for bad in ["", "Up", "two words", "ab\u{200B}", "tab\t"] {
            let mut km = Keymap::native();
            km.bindings
                .insert(KeymapAction::ListRooms, vec![bad.to_owned()]);
            assert_eq!(
                km.validate(),
                Err(KeymapError::MalformedKeyword(bad.to_owned())),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn multi_key_and_long_word_keywords_are_allowed() {
        let mut km = Keymap::native();
        km.bindings.insert(
            KeymapAction::GoMail,
            vec!["lm".to_owned(), "mail".to_owned()],
        );
        assert_eq!(km.validate(), Ok(()));
        assert_eq!(km.action_for("mail"), Some(KeymapAction::GoMail));
    }

    #[test]
    fn an_unsupported_keyword_that_is_bound_or_reserved_is_rejected() {
        let mut km = Keymap::native();
        km.unsupported.insert("q".to_owned(), "x".to_owned());
        assert_eq!(
            km.validate(),
            Err(KeymapError::UnsupportedKeywordInUse("q".to_owned()))
        );
        let mut km = Keymap::native();
        km.unsupported.insert("b".to_owned(), "x".to_owned());
        assert_eq!(
            km.validate(),
            Err(KeymapError::UnsupportedKeywordInUse("b".to_owned()))
        );
    }

    #[test]
    fn every_preset_lists_only_real_gap_actions() {
        for km in all_keymaps() {
            for action in &km.no_source_equivalent {
                assert!(!km.keywords(*action).is_empty(), "{}: {action:?}", km.name);
            }
        }
    }

    /// Exhaustive match: adding a `KeymapAction` variant fails to compile here
    /// until `ALL` and this test are updated.
    #[allow(dead_code)]
    fn _all_variants_are_matched_exhaustively(action: KeymapAction) {
        match action {
            KeymapAction::Quit
            | KeymapAction::ListRooms
            | KeymapAction::GoNextUnread
            | KeymapAction::ChangeRoom
            | KeymapAction::GoMail
            | KeymapAction::ReadNew
            | KeymapAction::ReadForward
            | KeymapAction::ReadReverse
            | KeymapAction::ScanMessages
            | KeymapAction::EnterMessage
            | KeymapAction::DeleteMessage
            | KeymapAction::WhoIsOnline
            | KeymapAction::ReadingForward
            | KeymapAction::ReadingReverse
            | KeymapAction::ReadingReply
            | KeymapAction::ReadingHelp
            | KeymapAction::ReadingDelete => {}
        }
    }

    #[test]
    fn all_has_no_duplicate_entries() {
        let mut seen = std::collections::HashSet::new();
        for action in KeymapAction::ALL {
            assert!(seen.insert(*action), "duplicate in ALL: {action:?}");
        }
    }

    #[test]
    fn every_builtin_round_trips_through_toml() {
        for km in all_keymaps() {
            let text = toml::to_string(&km).unwrap();
            let back: Keymap = toml::from_str(&text).unwrap();
            assert_eq!(back, km, "{}", km.name);
        }
    }

    #[test]
    fn a_hand_authored_toml_keymap_parses_and_validates() {
        let mut km = Keymap::native();
        km.name = "my-bbs".to_owned();
        km.bindings
            .insert(KeymapAction::Quit, vec!["g".to_owned(), "leave".to_owned()]);
        km.bindings
            .insert(KeymapAction::GoNextUnread, vec!["next".to_owned()]);
        let text = toml::to_string(&km).unwrap();
        assert!(text.contains("[bindings]"), "{text}");
        let back: Keymap = toml::from_str(&text).unwrap();
        assert_eq!(back.validate(), Ok(()));
        assert_eq!(back.keywords(KeymapAction::Quit), ["g", "leave"]);
    }

    #[test]
    fn an_unrecognised_action_name_in_toml_is_a_parse_error() {
        let text = r#"
name = "x"
description = "y"
[bindings]
NotAnAction = ["z"]
"#;
        assert!(toml::from_str::<Keymap>(text).is_err());
    }

    #[test]
    fn an_unknown_top_level_field_in_toml_is_a_parse_error() {
        let mut text = toml::to_string(&Keymap::native()).unwrap();
        text.push_str("\n[binding]\nQuit = [\"z\"]\n");
        assert!(toml::from_str::<Keymap>(&text).is_err());
    }

    #[test]
    fn a_partial_toml_keymap_parses_but_fails_validation() {
        let text = r#"
name = "old-style"
description = "only some actions"
[bindings]
Quit = ["g"]
"#;
        let km: Keymap = toml::from_str(text).unwrap();
        assert!(matches!(km.validate(), Err(KeymapError::MissingAction(_))));
    }

    // ── Preset accuracy (GH #354 phase 7) ────────────────────────────────

    #[test]
    fn unsupported_replies_are_short_enough_for_a_radio_message() {
        for km in all_keymaps() {
            for (key, message) in &km.unsupported {
                assert!(
                    message.len() <= 60,
                    "{}: {key:?} reply is {} bytes: {message}",
                    km.name,
                    message.len()
                );
                assert!(!message.contains('\n'), "{}: {key:?}", km.name);
            }
        }
    }

    /// Keys a source system really has keep the meaning the research verified
    /// (`research-classic-bbs-commands.md` and the Phase 7 addendum).
    #[test]
    fn verified_source_keys_map_to_the_actions_the_research_found() {
        use KeymapAction as A;
        let check = |km: Keymap, expected: &[(&str, A)], reading: &[(&str, A)]| {
            for (key, action) in expected {
                assert_eq!(km.action_for(key), Some(*action), "{} {key}", km.name);
            }
            for (key, action) in reading {
                assert_eq!(
                    km.reading_action_for(key),
                    Some(*action),
                    "{} reading {key}",
                    km.name
                );
            }
        };
        check(
            Keymap::maximus(),
            &[
                ("g", A::Quit),
                ("a", A::ChangeRoom),
                ("l", A::ScanMessages),
                ("e", A::EnterMessage),
                ("k", A::DeleteMessage),
                ("w", A::WhoIsOnline),
                ("p", A::ReadReverse),
            ],
            &[
                ("n", A::ReadingForward),
                ("p", A::ReadingReverse),
                ("r", A::ReadingReply),
                ("k", A::ReadingDelete),
            ],
        );
        check(
            Keymap::wwiv_family(),
            &[
                ("o", A::Quit),
                ("*", A::ListRooms),
                ("m", A::GoMail),
                ("n", A::ReadNew),
                ("s", A::ScanMessages),
                ("p", A::EnterMessage),
                ("r", A::DeleteMessage),
                ("//who", A::WhoIsOnline),
            ],
            &[("-", A::ReadingReverse), ("w", A::ReadingReply)],
        );
        check(
            Keymap::synchronet(),
            &[
                ("o", A::Quit),
                ("*", A::ListRooms),
                ("j", A::ChangeRoom),
                ("n", A::ReadNew),
                ("r", A::ReadForward),
                ("l", A::ScanMessages),
                ("p", A::EnterMessage),
                ("w", A::WhoIsOnline),
            ],
            &[
                ("+", A::ReadingForward),
                ("-", A::ReadingReverse),
                ("a", A::ReadingReply),
                ("d", A::ReadingDelete),
            ],
        );
        check(
            Keymap::pcboard(),
            &[
                ("g", A::Quit),
                ("j", A::ChangeRoom),
                ("q", A::ScanMessages),
                ("e", A::EnterMessage),
                ("k", A::DeleteMessage),
                ("r;s", A::ReadNew),
                ("r;l", A::ReadReverse),
                ("r", A::ReadForward),
                ("who", A::WhoIsOnline),
            ],
            &[
                ("next", A::ReadingForward),
                ("prev", A::ReadingReverse),
                ("re", A::ReadingReply),
                ("k", A::ReadingDelete),
            ],
        );
        check(
            Keymap::packet_bbs(),
            &[
                ("lm", A::GoMail),
                ("rn", A::ReadNew),
                ("rm", A::ReadNew),
                ("l", A::ScanMessages),
                ("ll", A::ScanMessages),
                ("r", A::ReadForward),
                ("k", A::DeleteMessage),
            ],
            &[("sr", A::ReadingReply), ("k", A::ReadingDelete)],
        );
    }

    /// Source keys that mean something else there must never be bound to a
    /// different action here; the research listed these as traps.
    #[test]
    fn trap_keys_are_never_bound_to_a_different_meaning() {
        // (preset, key a source user would expect to do something else)
        for (preset, traps) in [
            ("maximus", &["f", "j", "y", "o", "t", "c", "s", "m"][..]),
            (
                "pcboard",
                &[
                    "m", "p", "d", "f", "l", "z", "t", "o", "s", "a", "x", "c", "w",
                ][..],
            ),
            (
                "packet-bbs",
                &["q", "s", "sp", "sb", "n", "t", "d", "w", "c", "i", "x"][..],
            ),
            ("wwiv-family", &["t", "c", "a", "g", "f", "d", "q"][..]),
            (
                "synchronet",
                &["t", "x", "c", "q", "m", "g", "f", "s", "d"][..],
            ),
        ] {
            let km = Keymap::by_name(preset).unwrap();
            for key in traps {
                assert_eq!(km.action_for(key), None, "{preset} binds trap key {key:?}");
                assert!(
                    km.unsupported_message(key).is_some(),
                    "{preset} gives trap key {key:?} no reply"
                );
            }
        }
    }

    /// A preset's gap keys are the actions whose key is only there because every
    /// action needs one, so they must not be a key the source system gives a
    /// different meaning (that would be a trap): a gap action uses a plain word.
    #[test]
    fn gap_actions_use_plain_words_or_keys_the_source_really_has() {
        for km in all_keymaps() {
            if km.name == "native" {
                continue;
            }
            for action in &km.no_source_equivalent {
                if action.is_reading_only() {
                    continue;
                }
                let key = km.primary(*action);
                // A single-character gap key is allowed only where the research
                // verified that character for this system.
                if key.chars().count() == 1 {
                    let allowed: &[&str] = match km.name.as_str() {
                        // `]` is Maximus's "next area", the closest key to next unread.
                        "maximus" => &["]"],
                        // `e` is this BBS's own key for entering a message.
                        "packet-bbs" => &["e"],
                        _ => &[],
                    };
                    assert!(
                        allowed.contains(&key),
                        "{}: gap action {action:?} uses the single key {key:?}",
                        km.name
                    );
                }
            }
        }
    }
}
