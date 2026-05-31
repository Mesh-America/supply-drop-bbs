//! Per-node Meshtastic session state.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use bbs_plugin_api::SessionId;

const WORKFLOW_REPLY_DEDUP_SECS: u64 = 10;
const MESSAGE_DEDUP_SECS: u64 = 10;

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
    pub by_node: HashMap<u32, SessionEntry>,
    pub by_session: HashMap<SessionId, u32>,
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
        if let Some(entry) = self.by_node.remove(&node_num) {
            self.by_session.remove(&entry.session_id);
            Some(entry.session_id)
        } else {
            None
        }
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
