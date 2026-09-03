//! Per-node Meshtastic session state.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use bbs_plugin_api::SessionId;

const WORKFLOW_REPLY_DEDUP_SECS: u64 = 10;
const MESSAGE_DEDUP_SECS: u64 = 10;
/// Mirrors `bbs_mesh::session::PENDING_PROTECT_TTL_SECS` — see that
/// constant's doc comment for the adversary-resistance rationale.
const PENDING_PROTECT_TTL_SECS: u64 = 86_400; // 24 hours

#[derive(Debug)]
pub struct SessionEntry {
    pub session_id: SessionId,
    pub awaiting_reply: bool,
    pub last_workflow_reply: Option<(String, Instant)>,
    pub last_message: Option<(String, Instant)>,
}

#[derive(Debug, Default)]
pub struct SessionState {
    pub my_node_num: Option<u32>,
    /// The device's current LoRa config, captured from the `want_config` sync
    /// stream. Used to skip redundant LoRa writes on connect — writing LoRa
    /// config (even unchanged) reboots the radio, so we only write when the
    /// desired region/preset actually differs from what the device reports.
    pub device_lora: Option<crate::proto::LoRaConfig>,
    /// The device's current owner `User` (id, long/short name, public key),
    /// captured from the local node's NodeInfo during sync. Writing the owner
    /// (`SetOwner`) also reboots the radio on current firmware, so we skip the
    /// write when the configured name already matches — keeping the radio online
    /// so its own periodic NodeInfo broadcasts (how neighbours discover us) keep
    /// firing. Also serves the web "device snapshot" without a live admin round-trip.
    pub device_owner: Option<crate::proto::User>,
    /// The device's current security/PKC config, captured during sync. Lets the
    /// web serve the public key + admin-channel state without a live admin GET.
    pub device_security: Option<crate::proto::SecurityConfig>,
    /// The device's current DeviceConfig, captured during sync. Used to merge
    /// `node_info_broadcast_secs` (and skip-if-unchanged) without clobbering
    /// role/other device fields.
    pub device_config: Option<crate::proto::DeviceConfig>,
    pub by_node: HashMap<u32, SessionEntry>,
    pub by_session: HashMap<SessionId, u32>,
    /// `node_num`s currently awaiting a contact-protection decision — mirrors
    /// `bbs_mesh::session::SessionState::pending_protect` (see [`Self::mark_pending_protect`]).
    /// Unlike MeshCore's prefix-to-pubkey gap, Meshtastic needs no identity
    /// resolution step here: `node_num` is always known directly from
    /// `packet.from`.
    pending_protect: HashMap<u32, Instant>,
}

impl SessionState {
    pub fn lookup(&self, node_num: u32) -> Option<SessionId> {
        self.by_node.get(&node_num).map(|e| e.session_id)
    }

    pub fn get_or_insert(&mut self, node_num: u32, new_id: SessionId) -> (SessionId, bool) {
        if let Some(entry) = self.by_node.get(&node_num) {
            return (entry.session_id, false);
        }
        self.by_node.insert(
            node_num,
            SessionEntry {
                session_id: new_id,
                awaiting_reply: false,
                last_workflow_reply: None,
                last_message: None,
            },
        );
        self.by_session.insert(new_id, node_num);
        (new_id, true)
    }

    pub fn remove_by_node(&mut self, node_num: u32) -> Option<SessionId> {
        self.pending_protect.remove(&node_num);
        if let Some(entry) = self.by_node.remove(&node_num) {
            self.by_session.remove(&entry.session_id);
            Some(entry.session_id)
        } else {
            None
        }
    }

    /// Mark `node_num` as pending a contact-protection decision — an
    /// unconditional insert, idempotent regardless of whether it was already
    /// pending (see research.md Decision 3's round-21 correction, mirrored
    /// here for consistency with MeshCore even though Meshtastic's
    /// `dispatch_message`/`record_node_advert` are serialized on one task and
    /// don't currently need it — see T037's own note). Sweeps entries older
    /// than `PENDING_PROTECT_TTL_SECS` first — lazy, O(n) over the (small,
    /// TTL-bounded) pending set, no separate timer needed.
    pub fn mark_pending_protect(&mut self, node_num: u32) {
        self.mark_pending_protect_at(node_num, Instant::now());
    }

    /// [`Self::mark_pending_protect`] with an injectable `now`, so the TTL
    /// sweep is unit-testable without sleeping for real hours.
    fn mark_pending_protect_at(&mut self, node_num: u32, now: Instant) {
        let ttl = Duration::from_secs(PENDING_PROTECT_TTL_SECS);
        self.pending_protect
            .retain(|_, inserted_at| now.saturating_duration_since(*inserted_at) < ttl);
        self.pending_protect.insert(node_num, now);
    }

    /// Clear `node_num`'s pending-protect mark — called once a protection
    /// attempt reaches a terminal outcome (`Protected`, `ProtectedWithEviction`,
    /// `AlreadyProtected`, `Ineligible`, or `CapReached`).
    pub fn clear_pending_protect(&mut self, node_num: u32) {
        self.pending_protect.remove(&node_num);
    }

    /// Return `true` if `node_num` is currently marked as pending a
    /// contact-protection decision.
    pub fn is_pending_protect(&self, node_num: u32) -> bool {
        self.pending_protect.contains_key(&node_num)
    }

    pub fn node_for_session(&self, session: SessionId) -> Option<u32> {
        self.by_session.get(&session).copied()
    }

    pub fn sessions(&self) -> Vec<SessionId> {
        self.by_session.keys().copied().collect()
    }

    pub fn set_awaiting_reply(&mut self, node_num: u32, value: bool) {
        if let Some(entry) = self.by_node.get_mut(&node_num) {
            entry.awaiting_reply = value;
        }
    }

    pub fn is_awaiting_reply(&self, node_num: u32) -> bool {
        self.by_node
            .get(&node_num)
            .is_some_and(|e| e.awaiting_reply)
    }

    pub fn set_last_workflow_reply(&mut self, node_num: u32, text: String) {
        if let Some(entry) = self.by_node.get_mut(&node_num) {
            entry.last_workflow_reply = Some((text, Instant::now()));
        }
    }

    pub fn is_recent_workflow_reply(&self, node_num: u32, text: &str) -> bool {
        self.by_node.get(&node_num).is_some_and(|entry| {
            entry
                .last_workflow_reply
                .as_ref()
                .is_some_and(|(reply, instant)| {
                    reply == text
                        && instant.elapsed() < Duration::from_secs(WORKFLOW_REPLY_DEDUP_SECS)
                })
        })
    }

    pub fn dedup_message(&mut self, node_num: u32, text: &str) -> bool {
        if let Some(entry) = self.by_node.get_mut(&node_num) {
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

    const NODE: u32 = 0x1234_5678;

    #[test]
    fn pending_protect_marks_and_clears() {
        let mut st = SessionState::default();
        assert!(!st.is_pending_protect(NODE));
        st.mark_pending_protect(NODE);
        assert!(st.is_pending_protect(NODE));
        st.clear_pending_protect(NODE);
        assert!(!st.is_pending_protect(NODE));
    }

    #[test]
    fn pending_protect_mark_is_idempotent_and_reentrant() {
        let mut st = SessionState::default();
        let other = NODE.wrapping_add(1);

        st.mark_pending_protect(NODE);
        st.mark_pending_protect(NODE);
        assert!(st.is_pending_protect(NODE));

        st.mark_pending_protect(other);
        st.clear_pending_protect(other);
        assert!(!st.is_pending_protect(other));
        assert!(st.is_pending_protect(NODE));
    }

    #[test]
    fn pending_protect_ttl_sweeps_stale_entries() {
        let mut st = SessionState::default();
        let other = NODE.wrapping_add(1);
        let third = NODE.wrapping_add(2);

        let t0 = Instant::now();
        st.mark_pending_protect_at(NODE, t0);

        let inside = t0 + Duration::from_secs(PENDING_PROTECT_TTL_SECS - 1);
        st.mark_pending_protect_at(other, inside);
        assert!(
            st.is_pending_protect(NODE),
            "still inside the TTL window — must not be swept"
        );

        let outside = t0 + Duration::from_secs(PENDING_PROTECT_TTL_SECS + 1);
        st.mark_pending_protect_at(third, outside);
        assert!(
            !st.is_pending_protect(NODE),
            "older than the TTL — must be swept on the next insert"
        );
        assert!(st.is_pending_protect(other));
        assert!(st.is_pending_protect(third));
    }

    #[test]
    fn remove_by_node_clears_pending_protect() {
        let mut st = SessionState::default();
        st.mark_pending_protect(NODE);
        assert!(st.is_pending_protect(NODE));
        st.remove_by_node(NODE);
        assert!(!st.is_pending_protect(NODE));
    }
}
