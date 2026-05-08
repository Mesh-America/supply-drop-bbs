//! Domain events and notifications.
//!
//! - [`DomainEvent`] is fired by the host when state changes.
//!   Plugins subscribe via [`Host::events`](crate::Host::events) and
//!   react (e.g., the mesh transport pushes a notification when a
//!   new DM arrives for an online user).
//! - [`Notification`] is the payload a transport delivers to a
//!   user's session. It's the abstract "tell this user X"; the
//!   transport decides how to render and deliver.
//! - [`NotifyOutcome`] is what `TransportEngine::notify` returns
//!   so the host can record delivery state.

use crate::identity::{SessionId, Username};
use serde::{Deserialize, Serialize};

/// A state-change event the host emits. Plugins subscribe to the
/// stream via [`Host::events`](crate::Host::events).
///
/// Events are non-exhaustive on purpose: new domain operations
/// will introduce new variants. Plugins must handle the
/// `_` arm in any `match` they write over a `DomainEvent`. The
/// `#[non_exhaustive]` attribute makes the compiler enforce this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DomainEvent {
    /// A new session was created. Carries the session id and
    /// the transport that created it.
    SessionCreated {
        /// The newly-minted session.
        session: SessionId,
        /// The name of the transport that minted it (matches
        /// `TransportEngine::name`). Stored as `String` rather
        /// than `&'static str` so the event survives a serde
        /// roundtrip (audit log, reports, JSON metrics).
        transport: String,
    },

    /// A session was bound to a user (login completed).
    SessionAuthenticated {
        /// The session that just authenticated.
        session: SessionId,
        /// The user the session is now bound to.
        user: Username,
    },

    /// A session ended (logout, expiry, transport close).
    SessionEnded {
        /// The ended session.
        session: SessionId,
        /// Why it ended. Free-form string for now; may grow into
        /// an enum in a future release.
        reason: String,
    },

    /// A message was posted in a room or as a DM.
    ///
    /// Placeholder shape until the message domain types land in
    /// `bbs-core`. The fields here will become typed identifiers.
    MessagePosted {
        /// Username of the poster.
        sender: Username,
        /// Recipient: either a room name (public post) or
        /// `Some(username)` (DM).
        recipient: MessageRecipient,
        /// The internal message id (opaque integer for now).
        message_id: u64,
    },

    /// A user was created (registration submitted).
    UserCreated {
        /// The newly-created user.
        user: Username,
    },

    /// A user was validated by a sysop or aide.
    UserValidated {
        /// The user who was validated.
        user: Username,
    },
}

/// Where a message is going.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MessageRecipient {
    /// A public post in a named room.
    Room(String),
    /// A direct message to a specific user.
    Direct(Username),
}

/// An unsolicited payload to deliver to a session.
///
/// The host produces these in response to [`DomainEvent`]s (or
/// other internal triggers) and passes them to a transport via
/// [`TransportEngine::notify`](crate::TransportEngine::notify).
/// The transport renders to whatever its protocol expects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Notification {
    /// Plain text. Most notifications are this.
    Text(String),

    /// A new mail arrival nudge — used by transports that have
    /// out-of-band notification UI (e.g., the web admin's bell
    /// icon, mesh's "you have N new messages" prompt).
    MailWaiting {
        /// Number of unread messages waiting.
        count: u32,
    },

    /// A system event the user should know about (e.g., "your
    /// account has been validated"). Distinct from `Text` so
    /// transports can render with different emphasis.
    SystemEvent(String),
}

/// What happened when a transport tried to deliver a notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum NotifyOutcome {
    /// Delivered (or queued for guaranteed delivery in a way the
    /// transport considers reliable).
    Delivered,

    /// Couldn't deliver right now but the transport queued the
    /// notification for retry.
    Queued,

    /// The session is offline and the transport doesn't queue.
    /// The notification is dropped. Up to the host to decide
    /// whether to retry later via a different mechanism (e.g.,
    /// "you have new mail" on next login).
    Dropped,

    /// Permanent failure — the session no longer exists or the
    /// user has explicitly declined this notification class.
    PermanentFailure(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_event_serde_roundtrip() {
        let ev = DomainEvent::SessionAuthenticated {
            session: SessionId::__internal_new(7),
            user: Username::new("alice").unwrap(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: DomainEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn notification_serde_roundtrip() {
        for n in [
            Notification::Text("hi".to_owned()),
            Notification::MailWaiting { count: 3 },
            Notification::SystemEvent("validated".to_owned()),
        ] {
            let json = serde_json::to_string(&n).unwrap();
            let back: Notification = serde_json::from_str(&json).unwrap();
            assert_eq!(n, back);
        }
    }

    #[test]
    fn notify_outcome_serde_roundtrip() {
        for o in [
            NotifyOutcome::Delivered,
            NotifyOutcome::Queued,
            NotifyOutcome::Dropped,
            NotifyOutcome::PermanentFailure("unknown".to_owned()),
        ] {
            let json = serde_json::to_string(&o).unwrap();
            let back: NotifyOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(o, back);
        }
    }
}
