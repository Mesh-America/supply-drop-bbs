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

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use bbs_plugin_api::SessionId;

/// How long a workflow reply is remembered for deduplication.
/// Meshtastic retransmissions happen within a few seconds; 10 s is generous.
/// 60 s caused false-positive drops when a user typed a short string (e.g. "h")
/// as a workflow reply and then immediately sent "h" for help.
const WORKFLOW_REPLY_DEDUP_SECS: u64 = 10;

/// How long any inbound message is remembered for deduplication.
/// Covers radio retransmissions of regular commands (non-workflow).
const MESSAGE_DEDUP_SECS: u64 = 10;

/// How long a message identity (sender timestamp + text) is remembered for the
/// timestamp-based dedup in [`SessionState::dedup_by_timestamp`].
///
/// A genuine retransmission reuses the sender's per-message timestamp, so this
/// window can be far more generous than [`MESSAGE_DEDUP_SECS`] without risking
/// false-positive drops of legitimately repeated text (a real new message
/// carries a new timestamp). It only has to outlast a retransmission train and
/// the sender's own ACK-retry timeout — the gap that let the text-only window
/// miss a delayed resend and reprocess it (the "Error: Already logged in"
/// symptom).
const TIMESTAMP_DEDUP_SECS: u64 = 120;

/// Maximum number of recent message identities remembered per node for
/// timestamp-based dedup. Bounds memory; comfortably covers an interleaved
/// retransmission train.
const RECENT_MSG_CAP: usize = 16;

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

    /// The last inbound message text and the time it was processed.
    /// Used to silently drop radio retransmissions of regular commands when the
    /// sender supplies no per-message timestamp (the `dedup_by_timestamp`
    /// fallback).
    pub last_message: Option<(String, Instant)>,

    /// Recently-seen `(sender timestamp, text, processed-at)` identities,
    /// oldest first. A retransmitted message reuses the sender's timestamp, so
    /// matching on `(timestamp, text)` drops resends robustly — even ones that
    /// arrive long after the original or after the workflow state changed,
    /// unlike the text-only [`Self::last_message`] window. Only populated for
    /// messages whose sender timestamp is non-zero. Bounded by
    /// `RECENT_MSG_CAP`.
    pub recent_msgs: VecDeque<(u32, String, Instant)>,

    /// Full 32-byte public key for this node, populated the first time a
    /// `NewAdvert` frame arrives from this node.  `None` until that happens.
    /// Used to send `ResetPath` after delivering a message so the next
    /// outbound message floods rather than using a potentially-stale path.
    pub full_pubkey: Option<[u8; 32]>,
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
    /// The BBS node's own 32-byte public key, set on the first `Connected`
    /// event.  Used to detect when the radio echoes our own advert back so
    /// the `NewAdvert` handler can substitute the configured GPS rather than
    /// the radio's hardware GPS reading (which is 0,0 when no GPS lock).
    pub self_pubkey: Option<[u8; 32]>,
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
                last_message: None,
                recent_msgs: VecDeque::new(),
                full_pubkey: None,
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

    /// Store the full 32-byte public key for `prefix`.  No-op if the prefix
    /// has no session yet (the key will be recorded when a session is created).
    pub fn set_full_pubkey(&mut self, prefix: &[u8; 6], pubkey: [u8; 32]) {
        if let Some(entry) = self.by_prefix.get_mut(prefix) {
            entry.full_pubkey = Some(pubkey);
        }
    }

    /// Return the full 32-byte public key for `prefix`, if known.
    pub fn get_full_pubkey(&self, prefix: &[u8; 6]) -> Option<[u8; 32]> {
        self.by_prefix.get(prefix)?.full_pubkey
    }

    /// Clear the message-dedup baseline for `prefix` so the next inbound
    /// message is never treated as a retransmission, even if its text matches
    /// the one just processed.  No-op if the prefix has no session.
    ///
    /// Called when the host issues a new `Response::Prompt`: a prompt starts a
    /// fresh reply turn, so the user's next message is genuine new input — most
    /// importantly when it legitimately repeats the previous reply (e.g. typing
    /// the same password again at "Confirm your password:"). Without this, the
    /// general dedup in [`Self::dedup_message`] would silently drop the matching
    /// confirmation. See issue #104.
    pub fn clear_last_message(&mut self, prefix: &[u8; 6]) {
        if let Some(entry) = self.by_prefix.get_mut(prefix) {
            entry.last_message = None;
        }
    }

    /// Deduplicate an inbound message by the sender's per-message `timestamp`.
    ///
    /// Returns `true` (drop it) if this node already sent a message with the
    /// same `(timestamp, text)` within `TIMESTAMP_DEDUP_SECS`; otherwise
    /// records the identity and returns `false`. A `timestamp` of `0` means the
    /// sender supplied none, so this records nothing and returns `false` — the
    /// caller falls back to [`Self::dedup_message`].
    ///
    /// This is the robust path: a retransmission reuses the sender's timestamp,
    /// so a resend is dropped even when it arrives past the text-only window or
    /// after the workflow state changed. `text` is part of the key so two
    /// distinct messages that happen to share a one-second timestamp are not
    /// conflated.  No-op (returns `false`) if `prefix` has no session.
    pub fn dedup_by_timestamp(&mut self, prefix: &[u8; 6], timestamp: u32, text: &str) -> bool {
        if timestamp == 0 {
            return false;
        }
        let Some(entry) = self.by_prefix.get_mut(prefix) else {
            return false;
        };
        // Evict identities older than the window. Entries are pushed in time
        // order, so the oldest are always at the front.
        let window = Duration::from_secs(TIMESTAMP_DEDUP_SECS);
        while entry
            .recent_msgs
            .front()
            .is_some_and(|(_, _, seen)| seen.elapsed() >= window)
        {
            entry.recent_msgs.pop_front();
        }
        if entry
            .recent_msgs
            .iter()
            .any(|(ts, t, _)| *ts == timestamp && t == text)
        {
            return true;
        }
        entry
            .recent_msgs
            .push_back((timestamp, text.to_owned(), Instant::now()));
        if entry.recent_msgs.len() > RECENT_MSG_CAP {
            entry.recent_msgs.pop_front();
        }
        false
    }

    /// Return `true` if `text` matches the last processed message for `prefix`
    /// within the deduplication window.  If it does not match, record `text`
    /// as the new last message and return `false`.
    ///
    /// Used to silently drop radio retransmissions of regular commands.
    pub fn dedup_message(&mut self, prefix: &[u8; 6], text: &str) -> bool {
        if let Some(entry) = self.by_prefix.get_mut(prefix) {
            if let Some((last, instant)) = &entry.last_message {
                if last == text && instant.elapsed() < Duration::from_secs(MESSAGE_DEDUP_SECS) {
                    return true;
                }
            }
            entry.last_message = Some((text.to_owned(), Instant::now()));
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bbs_plugin_api::SessionId;

    const PREFIX: [u8; 6] = [1, 2, 3, 4, 5, 6];

    fn state_with_session() -> SessionState {
        let mut st = SessionState::default();
        st.get_or_insert(PREFIX, SessionId::__internal_new(1));
        st
    }

    #[test]
    fn dedup_drops_immediate_retransmission() {
        let mut st = state_with_session();
        assert!(!st.dedup_message(&PREFIX, "pw"), "first copy is processed");
        assert!(
            st.dedup_message(&PREFIX, "pw"),
            "an identical retransmission within the window is dropped"
        );
    }

    /// Issue #104: the password and its confirmation are the same text. After
    /// the host prompts "Confirm your password:" the transport clears the dedup
    /// baseline, so the matching confirmation is processed rather than dropped
    /// as a retransmission.
    #[test]
    fn clear_last_message_allows_identical_reply_after_prompt() {
        let mut st = state_with_session();
        assert!(
            !st.dedup_message(&PREFIX, "pw"),
            "password entry is processed"
        );

        // Host returned a Prompt → a fresh reply turn begins.
        st.clear_last_message(&PREFIX);
        assert!(
            !st.dedup_message(&PREFIX, "pw"),
            "the matching confirmation must NOT be dropped after a fresh prompt"
        );

        // A genuine retransmission of the confirmation is still dropped.
        assert!(
            st.dedup_message(&PREFIX, "pw"),
            "a retransmission of the confirmation is still deduped"
        );
    }

    #[test]
    fn timestamp_dedup_drops_resend_with_same_timestamp() {
        let mut st = state_with_session();
        assert!(
            !st.dedup_by_timestamp(&PREFIX, 1_000, "login bob"),
            "first copy is processed"
        );
        assert!(
            st.dedup_by_timestamp(&PREFIX, 1_000, "login bob"),
            "a resend reusing the sender timestamp is dropped"
        );
    }

    #[test]
    fn timestamp_dedup_allows_new_message_with_new_timestamp() {
        let mut st = state_with_session();
        // Same text, but a genuinely new message carries a new timestamp — it
        // must be processed (this is the issue #104 password/confirmation case).
        assert!(!st.dedup_by_timestamp(&PREFIX, 1_000, "secret"));
        assert!(
            !st.dedup_by_timestamp(&PREFIX, 1_001, "secret"),
            "identical text with a fresh timestamp is a new message, not a resend"
        );
    }

    #[test]
    fn timestamp_dedup_distinguishes_text_within_one_timestamp() {
        let mut st = state_with_session();
        // Two distinct messages that happen to share a one-second timestamp must
        // both be processed — the text is part of the identity.
        assert!(!st.dedup_by_timestamp(&PREFIX, 1_000, "rooms"));
        assert!(
            !st.dedup_by_timestamp(&PREFIX, 1_000, "read"),
            "different text under the same timestamp is not a resend"
        );
        // ...but a true resend of either is still dropped.
        assert!(st.dedup_by_timestamp(&PREFIX, 1_000, "rooms"));
    }

    #[test]
    fn timestamp_dedup_skips_zero_timestamp() {
        let mut st = state_with_session();
        // A zero timestamp means "not supplied" — never dedup on it, so the
        // caller falls back to the text-only window instead.
        assert!(!st.dedup_by_timestamp(&PREFIX, 0, "hello"));
        assert!(
            !st.dedup_by_timestamp(&PREFIX, 0, "hello"),
            "a zero timestamp is never treated as a duplicate"
        );
    }
}
