//! The `TransportEngine` capability trait.
//!
//! Implemented by transport plugins (cli, mesh, web admin). A
//! transport's job is to translate between its protocol's wire
//! format and the host's [`Command`](crate::Command) /
//! [`Response`](crate::Response) types, and to push notifications
//! to active sessions.
//!
//! See [docs/PLUGIN_API.md](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/PLUGIN_API.md)
//! for the prose introduction and worked examples.

use crate::error::TransportError;
use crate::event::{Notification, NotifyOutcome};
use crate::identity::SessionId;
use crate::plugin::Plugin;
use async_trait::async_trait;

/// A transport — a way for users (or other systems) to talk to
/// the BBS.
///
/// `TransportEngine` extends [`Plugin`]: a transport implements
/// both. The lifecycle (init, start, stop) comes from `Plugin`;
/// the transport-specific operations are here.
#[async_trait]
pub trait TransportEngine: Plugin {
    /// Push an unsolicited notification to a session.
    ///
    /// The host calls this when a [`DomainEvent`](crate::DomainEvent)
    /// it knows is interesting to a particular session needs to be
    /// surfaced — e.g., a new DM arrived for an online user.
    ///
    /// The transport decides how to deliver based on its medium:
    /// queue for retry, drop, fail, or deliver. The
    /// [`NotifyOutcome`] reported back lets the host's notification
    /// router decide whether to retry through a different
    /// transport (a user logged in on both web and mesh might
    /// receive a notification through whichever is more reliable).
    async fn notify(
        &self,
        session: SessionId,
        payload: Notification,
    ) -> Result<NotifyOutcome, TransportError>;
}
