//! Per-node session state for the MeshCore transport.
//!
//! Each MeshCore node that sends a direct message to the BBS is represented by
//! a 6-byte public-key prefix (the first 6 bytes of its Ed25519 public key).
//! This prefix is stable for a given radio node and is what the companion frame
//! protocol exposes in [`ContactMsg`](meshcore_companion::types::ContactMsg).
//!
//! # Session lifecycle
//!
//! 1. First direct message from a prefix → [`SessionState::get_or_insert`]
//!    mints a fresh BBS session via `Host::create_session` and records the
//!    mapping in both directions.
//! 2. Subsequent messages → existing session is returned immediately.
//! 3. On a clean shutdown (client dropped) or after a prolonged silence the
//!    supervisor may eventually call `Host::end_session`; the mapping is
//!    removed from [`SessionState`] at that point.
//!
//! # Workflow tracking
//!
//! The BBS host returns `Response::Prompt` when it wants the user's next
//! message to be interpreted as a continuation of a multi-step flow (e.g.,
//! entering a password during login).  [`SessionEntry::awaiting_reply`] records
//! this flag so the command parser knows whether to emit
//! `Command::WorkflowReply` instead of trying to parse a command keyword.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bbs_plugin_api::SessionId;

/// How long a workflow reply is remembered for deduplication.
const WORKFLOW_REPLY_DEDUP_SECS: u64 = 60;

/// Per-node state tracked inside [`SessionState`].
#[derive(Debug)]
pub struct SessionEntry {
    /// The BBS session identifier minted by the host.
    pub session_id: SessionId,

    /// `true` after the host sends `Response::Prompt`; cleared once the
    /// next user message is dispatched as `Command::WorkflowReply`.
    ///
    /// This ensures passwords, answers to challenge questions, and other
    /// prompted input are never mis-parsed as command keywords.
    pub awaiting_reply: bool,

    /// The last text sent as a `WorkflowReply`, with the time it was
    /// processed. Used to silently drop mesh retransmissions of workflow
    /// input (passwords etc.) that arrive after the workflow completes.
    pub last_workflow_reply: Option<(String, Instant)>,
}

/// Bi-directional map between MeshCore pubkey prefixes and BBS session IDs.
///
/// Both directions are needed:
/// - **prefix → entry**: looked up on every inbound message to find (or create)
///   the session.
/// - **session → prefix**: looked up in
///   [`TransportEngine::notify`](bbs_plugin_api::TransportEngine::notify) to
///   find the destination node for a pushed notification.
///
/// Protected by a `std::sync::Mutex` in [`MeshTransport`](crate::MeshTransport).
/// The lock is never held across an `.await` point.
#[derive(Debug, Default)]
pub struct SessionState {
    /// Pubkey prefix (6 bytes) → session entry.
    pub by_prefix: HashMap<[u8; 6], SessionEntry>,
    /// Session ID → pubkey prefix (6 bytes).
    pub by_session: HashMap<SessionId, [u8; 6]>,
}

impl SessionState {
    /// Look up an existing session for `prefix`, or register a new one using
    /// the provided `new_id`.
    ///
    /// Returns `(session_id, is_new)` where `is_new` is `true` if `new_id` was
    /// consumed.  The caller should only create `new_id` (via
    /// [`Host::create_session`](bbs_plugin_api::Host::create_session)) if it
    /// does not already have a session for the prefix — see [`Self::lookup`].
    pub fn get_or_insert(&mut self, prefix: [u8; 6], new_id: SessionId) -> (SessionId, bool) {
        if let Some(entry) = self.by_prefix.get(&prefix) {
            return (entry.session_id, false);
        }
        self.by_prefix.insert(
            prefix,
            SessionEntry {
                session_id: new_id,
                awaiting_reply: false,
                last_workflow_reply: None,
            },
        );
        self.by_session.insert(new_id, prefix);
        (new_id, true)
    }

    /// Look up the session for `prefix` without creating one.
    pub fn lookup(&self, prefix: &[u8; 6]) -> Option<SessionId> {
        self.by_prefix.get(prefix).map(|e| e.session_id)
    }

    /// Remove the session for `prefix` (e.g. on explicit logout or expiry).
    /// Returns the removed `SessionId` if one existed.
    pub fn remove_by_prefix(&mut self, prefix: &[u8; 6]) -> Option<SessionId> {
        if let Some(entry) = self.by_prefix.remove(prefix) {
            self.by_session.remove(&entry.session_id);
            Some(entry.session_id)
        } else {
            None
        }
    }

    /// Set the `awaiting_reply` flag for `prefix`.  No-op if the prefix has no
    /// session.
    pub fn set_awaiting_reply(&mut self, prefix: &[u8; 6], value: bool) {
        if let Some(entry) = self.by_prefix.get_mut(prefix) {
            entry.awaiting_reply = value;
        }
    }

    /// Return `true` if the session for `prefix` is currently awaiting a
    /// workflow reply.
    pub fn is_awaiting_reply(&self, prefix: &[u8; 6]) -> bool {
        self.by_prefix.get(prefix).is_some_and(|e| e.awaiting_reply)
    }

    /// Record `text` as the most-recently-processed workflow reply for
    /// `prefix`.  Called immediately after dispatching a `WorkflowReply`.
    pub fn set_last_workflow_reply(&mut self, prefix: &[u8; 6], text: String) {
        if let Some(entry) = self.by_prefix.get_mut(prefix) {
            entry.last_workflow_reply = Some((text, Instant::now()));
        }
    }

    /// Return `true` if `text` matches the last workflow reply for `prefix`
    /// and that reply was processed within the deduplication window.
    ///
    /// Used to silently drop mesh retransmissions of workflow input (e.g.
    /// passwords) that arrive after the workflow has already completed.
    pub fn is_recent_workflow_reply(&self, prefix: &[u8; 6], text: &str) -> bool {
        if let Some(entry) = self.by_prefix.get(prefix) {
            if let Some((reply, instant)) = &entry.last_workflow_reply {
                return reply == text
                    && instant.elapsed() < Duration::from_secs(WORKFLOW_REPLY_DEDUP_SECS);
            }
        }
        false
    }
}
