//! The `Host` trait — what plugins call into.
//!
//! `bbs-core` provides the canonical implementation of this trait.
//! Plugins receive an `Arc<dyn Host>` at `init` time and use it for
//! everything they want the BBS to do: process commands, manage
//! sessions, subscribe to events.
//!
//! ## Permission gating
//!
//! Methods that touch user-visible state take a [`PermissionCtx`]
//! argument. The host enforces that the context's tier satisfies
//! the operation's requirement; transports cannot synthesise
//! contexts of arbitrary authority because the only way to mint
//! a `PermissionCtx` is through the host itself
//! (see [`PermissionCtx::__internal_new`]).
//!
//! ## What's NOT here yet
//!
//! Domain accessors — `host.users()`, `host.rooms()`,
//! `host.messages()` — are referenced in the architecture doc but
//! not yet on the trait. They'll land alongside the corresponding
//! types in `bbs-core`. For now `process_command` is the route
//! through which transport plugins manipulate domain state.

use std::sync::Arc;

use crate::advert::AdvertBus;
use crate::command::{Command, Response};
use crate::error::HostError;
use crate::event::DomainEvent;
use crate::identity::SessionId;
use crate::permissions::PermissionCtx;
use async_trait::async_trait;
use tokio::sync::broadcast;

/// What the host exposes to plugins.
///
/// Implementations of this trait are produced by `bbs-core` and
/// passed to each plugin at `init`. Plugins hold an
/// `Arc<dyn Host>` for their lifetime.
#[async_trait]
pub trait Host: Send + Sync {
    // ── Command processing ──────────────────────────────────────

    /// Process a command from a session.
    ///
    /// Permission checks happen inside the host based on the
    /// session's currently-bound user (or the unvalidated
    /// pre-auth tier if the session isn't bound yet). Transports
    /// cannot bypass these checks.
    ///
    /// The session may not be bound to a user — registration and
    /// login flow through this method on a pre-auth session.
    async fn process_command(
        &self,
        session: SessionId,
        cmd: Command,
    ) -> Result<Response, HostError>;

    // ── Sessions ────────────────────────────────────────────────

    /// Mint a new, unbound session for the given transport.
    /// Returns the freshly-allocated `SessionId`. The transport
    /// is responsible for binding this session ID to its
    /// connection (e.g., setting a cookie, recording a node->id
    /// mapping).
    ///
    /// The transport name must match a `TransportEngine::name()`
    /// of a loaded transport plugin. The host records this for
    /// audit.
    async fn create_session(&self, transport: &'static str) -> Result<SessionId, HostError>;

    /// End a session. Idempotent: ending an already-ended or
    /// unknown session returns Ok. The host emits a
    /// [`DomainEvent::SessionEnded`] for downstream consumers.
    async fn end_session(&self, session: SessionId) -> Result<(), HostError>;

    /// Look up the permission context for a session. Plugins use
    /// this when they need to check authority before doing
    /// anything optimistic — the host's own methods do the gating
    /// internally, so most plugins don't call this directly.
    async fn permission_ctx(&self, session: SessionId) -> Result<PermissionCtx, HostError>;

    // ── Domain events ───────────────────────────────────────────

    /// Subscribe to the domain-event stream. Each call returns a
    /// new `broadcast::Receiver`; events fan out to all active
    /// subscribers.
    ///
    /// The capacity of the broadcast channel is set by the host;
    /// slow consumers may receive `RecvError::Lagged` and miss
    /// events. Plugins should be ready to handle this — for most
    /// notifications, missing one is fine because the next
    /// trigger fires another. Plugins that need durable delivery
    /// (audit log, reports) should subscribe directly to the
    /// audit log via `process_command`-driven flows, not events.
    fn events(&self) -> broadcast::Receiver<DomainEvent>;

    // ── Mesh adverts ────────────────────────────────────────────

    /// Access the shared advert bus.
    ///
    /// `MeshTransport` writes records here when adverts are heard
    /// over the air and subscribes to the send-request channel.
    /// `WebPlugin` reads the list and triggers sends on behalf of
    /// the sysop.
    fn advert_bus(&self) -> Arc<AdvertBus>;
}
