//! In-process WARN/ERROR capture for the web admin error report.
//!
//! A `tracing::Layer` intercepts WARN and ERROR events, deduplicates them
//! by a fingerprint of (level + target + first 100 chars of message), and
//! stores the rolling set in an in-memory ring buffer.  An `Arc<Mutex<ErrorStore>>`
//! is shared between the layer and the web API.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

const MAX_ENTRIES: usize = 200;
const ERROR_CHANNEL_CAP: usize = 64;

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn make_fingerprint(level: &str, target: &str, message: &str) -> String {
    let prefix = &message[..message.len().min(100)];
    format!("{level}\x00{target}\x00{prefix}")
}

/// A single deduplicated WARN/ERROR entry in the error store.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorEntry {
    pub level: String,
    pub target: String,
    pub message: String,
    pub count: u64,
    pub first_seen: u64,
    pub last_seen: u64,
    #[serde(skip)]
    pub(crate) fingerprint: String,
}

/// In-memory ring buffer of WARN/ERROR events.
pub struct ErrorStore {
    entries: Vec<ErrorEntry>,
    error_tx: broadcast::Sender<ErrorEntry>,
}

impl ErrorStore {
    fn new(tx: broadcast::Sender<ErrorEntry>) -> Self {
        Self {
            entries: Vec::new(),
            error_tx: tx,
        }
    }

    /// Record a WARN or ERROR event, deduplicating against existing entries.
    pub fn record(&mut self, level: &str, target: &str, message: &str) {
        let fp = make_fingerprint(level, target, message);
        let now = unix_now();

        if let Some(e) = self.entries.iter_mut().find(|e| e.fingerprint == fp) {
            e.count += 1;
            e.last_seen = now;
            return;
        }

        // Evict the least-recently-seen entry when at capacity.
        if self.entries.len() >= MAX_ENTRIES {
            if let Some(pos) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.last_seen)
                .map(|(i, _)| i)
            {
                self.entries.remove(pos);
            }
        }

        let entry = ErrorEntry {
            level: level.to_owned(),
            target: target.to_owned(),
            message: message.to_owned(),
            count: 1,
            first_seen: now,
            last_seen: now,
            fingerprint: fp,
        };

        // Notify SSE subscribers for ERROR-level events.
        if level == "ERROR" {
            let _ = self.error_tx.send(entry.clone());
        }
        self.entries.push(entry);
    }

    /// Return all entries sorted by count descending.
    pub fn list_sorted(&self) -> Vec<ErrorEntry> {
        let mut v = self.entries.clone();
        v.sort_by(|a, b| b.count.cmp(&a.count));
        v
    }
}

/// `tracing::Layer` that intercepts WARN/ERROR events into an [`ErrorStore`].
pub struct ErrorTrackerLayer {
    store: Arc<Mutex<ErrorStore>>,
}

impl ErrorTrackerLayer {
    pub(crate) fn new(store: Arc<Mutex<ErrorStore>>) -> Self {
        Self { store }
    }
}

impl<S: Subscriber> Layer<S> for ErrorTrackerLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Level ordering: TRACE > DEBUG > INFO > WARN > ERROR
        // Skip anything more verbose than WARN.
        if *meta.level() > tracing::Level::WARN {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let msg = visitor.message.unwrap_or_default();
        self.store.lock().expect("error_store poisoned").record(
            &meta.level().to_string(),
            meta.target(),
            &msg,
        );
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let s = format!("{value:?}");
            // Debug-format for &str wraps in quotes; strip them.
            self.message = Some(s.trim_matches('"').to_owned());
        }
    }
}

/// Create an error tracker layer plus the shared store.
///
/// The returned `broadcast::Sender` can be cloned into `AppState` so SSE
/// handlers can subscribe without locking the store.
pub fn new_error_tracker() -> (
    ErrorTrackerLayer,
    Arc<Mutex<ErrorStore>>,
    broadcast::Sender<ErrorEntry>,
) {
    let (tx, _) = broadcast::channel(ERROR_CHANNEL_CAP);
    let store = Arc::new(Mutex::new(ErrorStore::new(tx.clone())));
    let layer = ErrorTrackerLayer::new(Arc::clone(&store));
    (layer, store, tx)
}
