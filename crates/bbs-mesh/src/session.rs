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

/// How long an unresolved pending-protect entry is remembered before being
/// swept, per `specs/001-persist-mesh-contacts/research.md` Decision 3's
/// round-21/26 fixes. Generous enough to comfortably outlast any commonly-
/// configured advertising interval, while still bounding memory against a
/// hostile peer manufacturing many never-resolving identities (each pending
/// entry is a few bytes; the sweep is what keeps this from growing forever).
const PENDING_PROTECT_TTL_SECS: u64 = 86_400; // 24 hours

/// Minimum gap between targeted `GetContacts` catch-up requests (see
/// [`SessionState::should_request_contacts_catchup`]). Matches
/// `MIN_ADVERT_SPACING` in transport.rs — the same "don't hammer the radio
/// with a background maintenance request" scale, not a real-time retry.
const CONTACTS_CATCHUP_COOLDOWN_SECS: u64 = 60;

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

    /// When the current workflow *stage* began awaiting a reply. Used to expire a
    /// workflow stranded by a lost prompt reply (the node keeps sending messages
    /// that are consumed as workflow input whose responses it never sees). Stamped
    /// only when the prompt text changes (a new stage), so a user stuck repeating
    /// the same prompt does not keep resetting the idle timer. `None` when not
    /// awaiting a reply.
    pub awaiting_reply_since: Option<Instant>,

    /// The text of the last `Response::Prompt` sent to this node. Distinguishes a
    /// *new* workflow stage (different prompt text → reset `awaiting_reply_since`)
    /// from a stage stuck because its prompt reply keeps getting lost (identical
    /// prompt text → timer keeps running so the workflow can time out).
    pub last_prompt_text: Option<String>,

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

    /// Full 32-byte public key for this node, populated the first time any
    /// identity-bearing frame arrives from it — `NewAdvert`, `Contact`,
    /// `PathUpdated`, or the lightweight `Advert`.  `None` until then.
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
    /// Senders whose contact-protection decision is still pending more data
    /// (an unresolved identity or an incomplete `AdvertBus` record) — see
    /// `specs/001-persist-mesh-contacts/research.md` Decision 3. Value is
    /// insertion time, used only for the TTL sweep in
    /// [`Self::mark_pending_protect`].
    pending_protect: HashMap<[u8; 6], Instant>,
    /// The last time a targeted `GetContacts` catch-up request was sent —
    /// see [`Self::should_request_contacts_catchup`]. A single cooldown
    /// shared by every sender rather than one per sender, since `GetContacts`
    /// fetches every contact the device knows about at once, not one contact
    /// at a time. `SessionState` (and this field with it) lives for the
    /// whole transport's lifetime, not just one physical connection — it is
    /// never reset on reconnect, so the cooldown persists across reconnects
    /// too.
    contacts_catchup_requested_at: Option<Instant>,
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
                awaiting_reply_since: None,
                last_prompt_text: None,
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
        self.pending_protect.remove(prefix);
        if let Some(entry) = self.by_prefix.remove(prefix) {
            self.by_session.remove(&entry.session_id);
            Some(entry.session_id)
        } else {
            None
        }
    }

    /// Update the workflow-reply state for `prefix` from a host response.
    ///
    /// `prompt_text` is `Some` when the host response was a `Response::Prompt`
    /// (the node's next message continues a workflow) and `None` otherwise.
    ///
    /// On a prompt, `awaiting_reply` is set and `awaiting_reply_since` is stamped
    /// **only if the prompt text differs from the last one** — a *new* workflow
    /// stage. A repeated identical prompt (a stage stuck because its reply keeps
    /// getting lost on a multi-hop link) does not reset the timer, so
    /// [`Self::awaiting_reply_expired`] can eventually free the node. On a
    /// non-prompt response the workflow state is cleared. No-op if the prefix has
    /// no session.
    pub fn update_awaiting_reply(&mut self, prefix: &[u8; 6], prompt_text: Option<&str>) {
        if let Some(entry) = self.by_prefix.get_mut(prefix) {
            match prompt_text {
                Some(text) => {
                    entry.awaiting_reply = true;
                    if entry.last_prompt_text.as_deref() != Some(text) {
                        entry.awaiting_reply_since = Some(Instant::now());
                        entry.last_prompt_text = Some(text.to_owned());
                    }
                }
                None => {
                    entry.awaiting_reply = false;
                    entry.awaiting_reply_since = None;
                    entry.last_prompt_text = None;
                }
            }
        }
    }

    /// Return `true` if the session for `prefix` is currently awaiting a
    /// workflow reply.
    pub fn is_awaiting_reply(&self, prefix: &[u8; 6]) -> bool {
        self.by_prefix.get(prefix).is_some_and(|e| e.awaiting_reply)
    }

    /// Return `true` if `prefix` has been awaiting a workflow reply (for the
    /// current stage) longer than `timeout` — i.e. the workflow is stale (its
    /// prompt reply was likely lost) and should be cancelled. `false` if the
    /// prefix is not awaiting a reply or is still within the window.
    pub fn awaiting_reply_expired(&self, prefix: &[u8; 6], timeout: Duration) -> bool {
        self.by_prefix
            .get(prefix)
            .and_then(|e| e.awaiting_reply_since)
            .is_some_and(|since| since.elapsed() >= timeout)
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
        self.dedup_by_timestamp_at(prefix, timestamp, text, Instant::now())
    }

    /// [`Self::dedup_by_timestamp`] with an injectable `now`, so the window and
    /// cap eviction are unit-testable without sleeping. `now` must be monotonic
    /// across calls (it is when threaded from `Instant::now`).
    fn dedup_by_timestamp_at(
        &mut self,
        prefix: &[u8; 6],
        timestamp: u32,
        text: &str,
        now: Instant,
    ) -> bool {
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
            .is_some_and(|(_, _, seen)| now.saturating_duration_since(*seen) >= window)
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
            .push_back((timestamp, text.to_owned(), now));
        // Bound memory. Note the cap can evict a still-in-window identity under a
        // burst of >RECENT_MSG_CAP distinct messages, so the effective dedup
        // horizon is min(TIMESTAMP_DEDUP_SECS, last RECENT_MSG_CAP messages) — at
        // airtime-limited LoRa rates this many distinct messages in 120s is rare.
        if entry.recent_msgs.len() > RECENT_MSG_CAP {
            entry.recent_msgs.pop_front();
        }
        false
    }

    /// Return `true` if `text` matches the last processed message for `prefix`
    /// within the deduplication window.  If it does not match, record `text`
    /// as the new last message and return `false`.
    ///
    /// This is the **fallback** dedup path, used only when the sender supplies no
    /// per-message timestamp (`timestamp == 0`); when a timestamp is present
    /// [`Self::dedup_by_timestamp`] handles dedup instead. Silently drops radio
    /// retransmissions of regular commands.
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

    /// Mark `prefix` as pending a contact-protection decision — an
    /// unconditional insert, idempotent regardless of whether it was already
    /// pending (see research.md Decision 3's round-21 correction: this must
    /// be a plain re-insert, not a conditional one, since a concurrent
    /// resolver could have raced it in between). Sweeps entries older than
    /// `PENDING_PROTECT_TTL_SECS` first — lazy, O(n) over the (small,
    /// TTL-bounded) pending set, no separate timer needed.
    pub fn mark_pending_protect(&mut self, prefix: [u8; 6]) {
        self.mark_pending_protect_at(prefix, Instant::now());
    }

    /// [`Self::mark_pending_protect`] with an injectable `now`, so the TTL
    /// sweep is unit-testable without sleeping for real hours.
    fn mark_pending_protect_at(&mut self, prefix: [u8; 6], now: Instant) {
        let ttl = Duration::from_secs(PENDING_PROTECT_TTL_SECS);
        self.pending_protect
            .retain(|_, inserted_at| now.saturating_duration_since(*inserted_at) < ttl);
        self.pending_protect.insert(prefix, now);
    }

    /// Clear `prefix`'s pending-protect mark — called once a protection
    /// attempt reaches a terminal outcome (`Protected`, `ProtectedWithEviction`,
    /// `AlreadyProtected`, `Ineligible`, or `CapReached`).
    pub fn clear_pending_protect(&mut self, prefix: &[u8; 6]) {
        self.pending_protect.remove(prefix);
    }

    /// Return `true` if `prefix` is currently marked as pending a
    /// contact-protection decision.
    pub fn is_pending_protect(&self, prefix: &[u8; 6]) -> bool {
        self.pending_protect.contains_key(prefix)
    }

    /// True when a targeted `GetContacts` catch-up request should be sent —
    /// more than `CONTACTS_CATCHUP_COOLDOWN_SECS` have passed since the last
    /// one (or none has ever been sent for the lifetime of this transport).
    ///
    /// Why this exists: the one-time, connect-time `GetContacts` (see
    /// `transport.rs`'s `event_loop` — its `ClientEvent::Connected` handler)
    /// only captures contacts the device already knew about at that exact
    /// moment. A sender who first DMs the BBS
    /// afterward — the common case, since BBS uptime and a given user's
    /// first contact are unrelated — gets a `NoRecordYet` protection outcome
    /// forever: the device now knows this contact internally (it routed the
    /// DM reply), but its own subsequent adverts for an already-known
    /// contact come through as the lightweight, name/type-less kind, not a
    /// full re-send. Nothing else in this transport re-asks the device for
    /// full contact details once the initial connect-time sync has passed,
    /// so without this, such a sender can never become eligible for
    /// protection no matter how many times they advert.
    pub fn should_request_contacts_catchup(&self) -> bool {
        self.should_request_contacts_catchup_at(Instant::now())
    }

    /// [`Self::should_request_contacts_catchup`] with an injectable `now`,
    /// so the cooldown is unit-testable without sleeping for real.
    fn should_request_contacts_catchup_at(&self, now: Instant) -> bool {
        let cooldown = Duration::from_secs(CONTACTS_CATCHUP_COOLDOWN_SECS);
        self.contacts_catchup_requested_at
            .is_none_or(|at| now.saturating_duration_since(at) >= cooldown)
    }

    /// Record that a `GetContacts` catch-up request was just sent, starting
    /// the cooldown before another one will be considered.
    pub fn mark_contacts_catchup_requested(&mut self) {
        self.mark_contacts_catchup_requested_at(Instant::now());
    }

    /// [`Self::mark_contacts_catchup_requested`] with an injectable `now`.
    fn mark_contacts_catchup_requested_at(&mut self, now: Instant) {
        self.contacts_catchup_requested_at = Some(now);
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
    fn awaiting_reply_stamps_on_prompt_and_clears() {
        let mut st = state_with_session();
        assert!(!st.is_awaiting_reply(&PREFIX));

        st.update_awaiting_reply(&PREFIX, Some("Choose a password:"));
        assert!(st.is_awaiting_reply(&PREFIX));
        assert!(
            st.by_prefix
                .get(&PREFIX)
                .unwrap()
                .awaiting_reply_since
                .is_some(),
            "a prompt stamps the idle-timeout clock"
        );

        // A non-prompt response ends the workflow and clears the clock.
        st.update_awaiting_reply(&PREFIX, None);
        assert!(!st.is_awaiting_reply(&PREFIX));
        assert!(st
            .by_prefix
            .get(&PREFIX)
            .unwrap()
            .awaiting_reply_since
            .is_none());
    }

    #[test]
    fn repeated_prompt_does_not_reset_the_idle_clock() {
        let mut st = state_with_session();
        let stuck = "Password must be at least 8 characters. Try again:";
        st.update_awaiting_reply(&PREFIX, Some(stuck));
        let first = st.by_prefix.get(&PREFIX).unwrap().awaiting_reply_since;

        // The SAME prompt text (a stage stuck because its reply keeps getting lost)
        // must NOT reset the clock — otherwise a retrying user never times out.
        st.update_awaiting_reply(&PREFIX, Some(stuck));
        let second = st.by_prefix.get(&PREFIX).unwrap().awaiting_reply_since;
        assert_eq!(
            first, second,
            "an identical repeated prompt keeps the timestamp"
        );

        // A DIFFERENT prompt (the workflow advanced a stage) DOES reset it.
        st.update_awaiting_reply(&PREFIX, Some("Confirm your password:"));
        let third = st.by_prefix.get(&PREFIX).unwrap().awaiting_reply_since;
        assert_ne!(first, third, "a new workflow stage resets the idle clock");
    }

    #[test]
    fn awaiting_reply_expiry_respects_window() {
        let mut st = state_with_session();
        // Not awaiting → never expired.
        assert!(!st.awaiting_reply_expired(&PREFIX, Duration::ZERO));

        st.update_awaiting_reply(&PREFIX, Some("Choose a password:"));
        // Elapsed is always ≥ 0, so a zero window is immediately expired…
        assert!(st.awaiting_reply_expired(&PREFIX, Duration::ZERO));
        // …but a generous window is not.
        assert!(!st.awaiting_reply_expired(&PREFIX, Duration::from_secs(3600)));
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

    #[test]
    fn timestamp_dedup_cap_evicts_oldest_identity() {
        let mut st = state_with_session();
        let now = Instant::now();
        // Fill the ring with RECENT_MSG_CAP+1 distinct identities at one instant
        // (within the window, so only the cap — not the window — evicts).
        for i in 0..=RECENT_MSG_CAP as u32 {
            assert!(
                !st.dedup_by_timestamp_at(&PREFIX, 1_000 + i, "cmd", now),
                "each distinct timestamp is a new message"
            );
        }
        assert!(
            st.by_prefix.get(&PREFIX).unwrap().recent_msgs.len() <= RECENT_MSG_CAP,
            "the ring stays bounded by RECENT_MSG_CAP"
        );
        // The most-recent identity is still remembered → its resend is dropped.
        assert!(
            st.dedup_by_timestamp_at(&PREFIX, 1_000 + RECENT_MSG_CAP as u32, "cmd", now),
            "a recent identity within the cap is still deduped"
        );
        // The oldest identity (ts 1_000) was evicted by the cap, so a resend of it
        // is reprocessed even though it is within the 120s window — the documented
        // cap-vs-window trade-off.
        assert!(
            !st.dedup_by_timestamp_at(&PREFIX, 1_000, "cmd", now),
            "an identity evicted by the cap is no longer deduped"
        );
    }

    #[test]
    fn timestamp_dedup_window_expiry_treats_late_resend_as_new() {
        let mut st = state_with_session();
        let t0 = Instant::now();
        assert!(!st.dedup_by_timestamp_at(&PREFIX, 1_000, "login bob", t0));
        // A resend just inside the window is still dropped.
        let inside = t0 + Duration::from_secs(TIMESTAMP_DEDUP_SECS - 1);
        assert!(
            st.dedup_by_timestamp_at(&PREFIX, 1_000, "login bob", inside),
            "a resend within the window is deduped"
        );
        // Past the window the identity is evicted and the resend is processed anew.
        let outside = t0 + Duration::from_secs(TIMESTAMP_DEDUP_SECS + 1);
        assert!(
            !st.dedup_by_timestamp_at(&PREFIX, 1_000, "login bob", outside),
            "a resend after the window expires is processed as new"
        );
    }

    #[test]
    fn pending_protect_marks_and_clears() {
        let mut st = SessionState::default();
        assert!(!st.is_pending_protect(&PREFIX));
        st.mark_pending_protect(PREFIX);
        assert!(st.is_pending_protect(&PREFIX));
        st.clear_pending_protect(&PREFIX);
        assert!(!st.is_pending_protect(&PREFIX));
    }

    #[test]
    fn pending_protect_mark_is_idempotent_and_reentrant() {
        let mut st = SessionState::default();
        // Re-marking an already-pending entry (round-21 fix: must be an
        // unconditional re-insert, not a conditional one) must not error or
        // clear it.
        st.mark_pending_protect(PREFIX);
        st.mark_pending_protect(PREFIX);
        assert!(st.is_pending_protect(&PREFIX));
        // Clearing an entry that was never marked is a no-op, not a panic.
        let other: [u8; 6] = [9, 9, 9, 9, 9, 9];
        st.clear_pending_protect(&other);
        assert!(!st.is_pending_protect(&other));
    }

    /// Cross-transport audit regression (Phase 4 verifier): Meshtastic's
    /// sibling `remove_by_node` clears `pending_protect` on removal; this
    /// method didn't. Low impact (a stale mark self-heals via the 24h TTL
    /// sweep), but the two transports' equivalent methods should behave the
    /// same way rather than silently drift apart.
    #[test]
    fn remove_by_prefix_clears_pending_protect() {
        let mut st = SessionState::default();
        st.get_or_insert(PREFIX, SessionId::__internal_new(1));
        st.mark_pending_protect(PREFIX);
        assert!(st.is_pending_protect(&PREFIX));
        st.remove_by_prefix(&PREFIX);
        assert!(!st.is_pending_protect(&PREFIX));
    }

    #[test]
    fn pending_protect_ttl_sweeps_stale_entries() {
        let mut st = SessionState::default();
        let t0 = Instant::now();
        let other: [u8; 6] = [9, 9, 9, 9, 9, 9];
        st.mark_pending_protect_at(PREFIX, t0);

        // Still within the TTL: a later insert for a different prefix must
        // not sweep the first one away.
        let inside = t0 + Duration::from_secs(PENDING_PROTECT_TTL_SECS - 1);
        st.mark_pending_protect_at(other, inside);
        assert!(
            st.is_pending_protect(&PREFIX),
            "an entry still within its TTL must survive another insert's sweep"
        );

        // Past the TTL: the next insert's lazy sweep must evict the stale one.
        let outside = t0 + Duration::from_secs(PENDING_PROTECT_TTL_SECS + 1);
        let third: [u8; 6] = [7, 7, 7, 7, 7, 7];
        st.mark_pending_protect_at(third, outside);
        assert!(
            !st.is_pending_protect(&PREFIX),
            "an entry past its TTL must be swept by the next insert"
        );
    }

    #[test]
    fn contacts_catchup_is_allowed_before_any_request_and_blocked_right_after() {
        let mut st = SessionState::default();
        assert!(
            st.should_request_contacts_catchup(),
            "nothing sent yet — a first catch-up must be allowed"
        );
        let t0 = Instant::now();
        st.mark_contacts_catchup_requested_at(t0);
        assert!(
            !st.should_request_contacts_catchup_at(t0),
            "immediately after a request, the cooldown must block another"
        );
    }

    #[test]
    fn contacts_catchup_cooldown_boundary_is_at_least_not_strictly_greater() {
        let mut st = SessionState::default();
        let t0 = Instant::now();
        st.mark_contacts_catchup_requested_at(t0);

        let just_inside = t0 + Duration::from_secs(CONTACTS_CATCHUP_COOLDOWN_SECS - 1);
        assert!(
            !st.should_request_contacts_catchup_at(just_inside),
            "still within the cooldown window must stay blocked"
        );

        // The real check is `>=`, not `>` — exactly at the boundary is allowed.
        let at_boundary = t0 + Duration::from_secs(CONTACTS_CATCHUP_COOLDOWN_SECS);
        assert!(
            st.should_request_contacts_catchup_at(at_boundary),
            "exactly at the cooldown boundary must be allowed again"
        );
    }
}
