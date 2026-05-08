//! Worked example: a tiny plugin built against `bbs-plugin-api`.
//!
//! The point of this test is to **prove the contract is usable**:
//! if this file compiles and runs, the API is at least
//! self-consistent enough that a plugin author can write a real
//! plugin without going outside `bbs-plugin-api` (plus `async-trait`
//! and `serde` for the standard ergonomics).
//!
//! What "Echo" does: subscribes to domain events, and on each
//! `MessagePosted` event, reports a count via a public method that
//! the test can inspect.
//!
//! This is intentionally not a transport — transports need
//! networking, which would bring in too much for an API-only
//! exercise. A future commit will add a worked-transport example.

use async_trait::async_trait;
use bbs_plugin_api::testing::MockHost;
use bbs_plugin_api::{event::MessageRecipient, DomainEvent, Host, Plugin, PluginError, Username};
use serde::Deserialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Config for our example plugin: just a name to log under.
#[derive(Deserialize, Default)]
struct EchoConfig {}

/// The plugin. Holds a `Host` handle (for events), a counter, and
/// the spawned worker task's join handle.
struct Echo {
    host: Arc<dyn Host>,
    posts_seen: Arc<AtomicUsize>,
    worker: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Echo {
    /// Tests use this to inspect the plugin's state.
    fn posts_seen(&self) -> usize {
        self.posts_seen.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Plugin for Echo {
    type Config = EchoConfig;

    fn name(&self) -> &'static str {
        "echo-example"
    }

    fn version(&self) -> &'static str {
        "0.0.0-test"
    }

    async fn init(_config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError> {
        Ok(Self {
            host,
            posts_seen: Arc::new(AtomicUsize::new(0)),
            worker: std::sync::Mutex::new(None),
        })
    }

    async fn start(&self) -> Result<(), PluginError> {
        let mut rx = self.host.events();
        let counter = self.posts_seen.clone();
        let handle = tokio::spawn(async move {
            while let Ok(event) = rx.recv().await {
                if matches!(event, DomainEvent::MessagePosted { .. }) {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        *self.worker.lock().expect("Echo: worker mutex poisoned") = Some(handle);
        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        if let Some(handle) = self
            .worker
            .lock()
            .expect("Echo: worker mutex poisoned")
            .take()
        {
            handle.abort();
        }
        Ok(())
    }
}

/// The contract proves itself: we build the plugin, drive it with
/// fake events through `MockHost`, and verify behaviour.
#[tokio::test]
async fn echo_plugin_counts_message_events() {
    let host: Arc<MockHost> = Arc::new(MockHost::new());
    let plugin = Echo::init(EchoConfig::default(), host.clone())
        .await
        .unwrap();
    plugin.start().await.unwrap();

    // Emit three MessagePosted events and one unrelated event.
    let alice = Username::new("alice").unwrap();
    let bob = Username::new("bob").unwrap();
    for i in 0..3u64 {
        host.emit_event(DomainEvent::MessagePosted {
            sender: alice.clone(),
            recipient: MessageRecipient::Direct(bob.clone()),
            message_id: i,
        })
        .unwrap();
    }
    host.emit_event(DomainEvent::UserCreated { user: bob.clone() })
        .unwrap();

    // Yield so the worker task has a chance to drain the channel.
    // tokio::task::yield_now isn't enough because the task is on
    // a separate worker thread; a short sleep is the sturdy choice.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(plugin.posts_seen(), 3);

    plugin.stop().await.unwrap();
}

/// We also want to confirm the contract enforces what it claims —
/// specifically that `process_command` errors on unknown sessions.
#[tokio::test]
async fn host_rejects_unknown_session() {
    use bbs_plugin_api::{Command, HostError, SessionId};

    let mock = MockHost::new();
    let bogus = SessionId::__internal_new(99_999);
    let err = mock
        .process_command(bogus, Command::Logout)
        .await
        .unwrap_err();
    assert!(
        matches!(err, HostError::UnknownSession(s) if s == bogus),
        "expected UnknownSession error, got: {err:?}",
    );
}
