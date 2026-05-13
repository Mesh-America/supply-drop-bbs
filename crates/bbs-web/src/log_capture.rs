//! Application-level log capture for the web admin log view.
//!
//! A `tracing::Layer` intercepts INFO, WARN, and ERROR events and stores
//! them in a shared `LogBuffer`.  The buffer is polled by the web API's
//! `GET /api/v1/logs` endpoint via incremental cursors.

use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

const LOG_BUF_CAP: usize = 500;

/// A monotonically-sequenced ring buffer for recent log lines.
///
/// Each line gets a unique `seq` number.  Clients send `?after=N` to receive
/// only lines with `seq >= N`, enabling efficient incremental polling.
#[derive(Default)]
pub struct LogBuffer {
    lines: std::collections::VecDeque<(u64, String)>,
    next_seq: u64,
}

impl LogBuffer {
    /// Create an empty buffer.
    pub fn new() -> Self {
        Self {
            lines: std::collections::VecDeque::new(),
            next_seq: 0,
        }
    }

    /// Append a log line, evicting the oldest entry when the ring is full.
    pub fn push(&mut self, text: String) {
        self.lines.push_back((self.next_seq, text));
        self.next_seq += 1;
        while self.lines.len() > LOG_BUF_CAP {
            self.lines.pop_front();
        }
    }

    /// Return all lines with `seq >= after` and the next cursor value.
    pub fn since(&self, after: u64) -> (u64, Vec<String>) {
        let lines = self
            .lines
            .iter()
            .filter(|(seq, _)| *seq >= after)
            .map(|(_, text)| text.clone())
            .collect();
        (self.next_seq, lines)
    }
}

/// `tracing::Layer` that captures INFO/WARN/ERROR events into a [`LogBuffer`].
pub struct LogCaptureLayer {
    buf: Arc<Mutex<LogBuffer>>,
}

impl LogCaptureLayer {
    /// Create a layer that writes into `buf`.
    pub fn new(buf: Arc<Mutex<LogBuffer>>) -> Self {
        Self { buf }
    }
}

impl<S: Subscriber> Layer<S> for LogCaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        // Capture INFO, WARN, ERROR — skip DEBUG and TRACE (too noisy).
        if *meta.level() > tracing::Level::INFO {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let msg = visitor.message.unwrap_or_default();
        let level = meta.level().as_str();
        let target = meta.target();
        let line = format!("[{level} {target}] {msg}");
        self.buf.lock().expect("log_buf poisoned").push(line);
    }
}

/// Create a log capture layer and its shared buffer.
///
/// The `Arc<Mutex<LogBuffer>>` is passed to `WebPlugin::set_log_buffer()` so
/// the API and the tracing layer share the same ring buffer.
pub fn new_log_capture_layer() -> (LogCaptureLayer, Arc<Mutex<LogBuffer>>) {
    let buf = Arc::new(Mutex::new(LogBuffer::new()));
    let layer = LogCaptureLayer::new(Arc::clone(&buf));
    (layer, buf)
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
            self.message = Some(s.trim_matches('"').to_owned());
        }
    }
}
