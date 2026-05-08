//! Test-time `Host` for plugin unit tests.
//!
//! [`MockHost`] is a minimal `Host` implementation plugin authors
//! can use to drive their plugins through deterministic scenarios
//! in unit tests. It tracks created sessions, records calls, and
//! lets tests script the responses to `process_command`.
//!
//! ## Example
//!
//! ```
//! use bbs_plugin_api::{Host, Command, Response};
//! use bbs_plugin_api::testing::MockHost;
//! use std::sync::Arc;
//!
//! # async fn example() {
//! let mock = Arc::new(MockHost::new());
//! mock.set_response_for(
//!     |c| matches!(c, Command::Logout),
//!     Response::LoggedOut,
//! );
//! let session = mock.create_session("cli").await.unwrap();
//! let resp = mock.process_command(session, Command::Logout).await.unwrap();
//! assert_eq!(resp, Response::LoggedOut);
//! # }
//! ```
//!
//! `MockHost` is intentionally simple — there's no expectation
//! framework, no fluent assertions, just a thread-safe handle that
//! plugin tests use to set up state and inspect what happened.

use crate::command::{Command, Response};
use crate::error::HostError;
use crate::event::DomainEvent;
use crate::identity::{SessionId, Username};
use crate::permissions::{PermissionCtx, PermissionLevel};
use crate::Host;
use async_trait::async_trait;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::broadcast;

/// Type alias for a command-matcher predicate. A boxed `Fn` so
/// matchers can capture state.
type CommandMatcher = Box<dyn Fn(&Command) -> bool + Send + Sync>;

/// A scripted `Host` for plugin tests.
///
/// `MockHost` implements [`Host`] with deterministic, in-memory
/// behaviour. Use it like this:
///
/// 1. Construct: `let mock = Arc::new(MockHost::new());`
/// 2. Optionally script responses: `mock.set_response_for(...)`
/// 3. Pass `mock.clone()` (as `Arc<dyn Host>`) to your plugin's
///    `init`.
/// 4. Drive your plugin in the test.
/// 5. Inspect what happened: `mock.commands_received()`, etc.
pub struct MockHost {
    next_session_id: AtomicU64,
    state: Mutex<MockHostState>,
    events: broadcast::Sender<DomainEvent>,
}

struct MockHostState {
    /// Live sessions and their bound usernames (if any).
    sessions: Vec<MockSession>,
    /// Scripted responses, evaluated in order; first match wins.
    responses: Vec<(CommandMatcher, Response)>,
    /// Default response when no scripted matcher matches.
    default_response: Response,
    /// Every command that has been dispatched, in order.
    commands_received: Vec<(SessionId, Command)>,
}

#[derive(Debug, Clone)]
struct MockSession {
    id: SessionId,
    /// Recorded for future inspection helpers (e.g., a
    /// `session_transport(id)` accessor for tests verifying
    /// transport routing). Not currently read; the dead-code
    /// allow is intentional to keep the test fixture's surface
    /// stable across additions.
    #[allow(dead_code)]
    transport: &'static str,
    user: Option<Username>,
    level: PermissionLevel,
    alive: bool,
}

impl Default for MockHost {
    fn default() -> Self {
        Self::new()
    }
}

impl MockHost {
    /// Construct a fresh mock with no sessions and a default
    /// response of `Response::Text("(unscripted)")`.
    #[must_use]
    pub fn new() -> Self {
        // Capacity 64 is enough for any reasonable test; tests
        // doing high-volume event work can construct their own
        // channel and assert on lag explicitly.
        let (tx, _rx) = broadcast::channel(64);
        Self {
            next_session_id: AtomicU64::new(1),
            state: Mutex::new(MockHostState {
                sessions: Vec::new(),
                responses: Vec::new(),
                default_response: Response::Text("(unscripted)".to_owned()),
                commands_received: Vec::new(),
            }),
            events: tx,
        }
    }

    /// Pre-create a session with a bound user at a specific
    /// permission level. Useful for tests that want to skip the
    /// login flow.
    pub fn with_authenticated_session(
        &self,
        transport: &'static str,
        user: Username,
        level: PermissionLevel,
    ) -> SessionId {
        let id = SessionId::__internal_new(self.next_session_id.fetch_add(1, Ordering::SeqCst));
        let mut state = self.state.lock().expect("mock poisoned");
        state.sessions.push(MockSession {
            id,
            transport,
            user: Some(user),
            level,
            alive: true,
        });
        id
    }

    /// Script a response for any command matching `matcher`.
    /// Matchers are evaluated in the order they were added; the
    /// first match wins. If no matcher matches, the
    /// `default_response` is used.
    pub fn set_response_for<F>(&self, matcher: F, response: Response)
    where
        F: Fn(&Command) -> bool + Send + Sync + 'static,
    {
        let mut state = self.state.lock().expect("mock poisoned");
        state.responses.push((Box::new(matcher), response));
    }

    /// Replace the default response (used when no scripted matcher
    /// matches).
    pub fn set_default_response(&self, response: Response) {
        let mut state = self.state.lock().expect("mock poisoned");
        state.default_response = response;
    }

    /// Inspect the commands that have been dispatched, in order.
    /// Returns owned clones — the mock keeps its own record.
    #[must_use]
    pub fn commands_received(&self) -> Vec<(SessionId, Command)> {
        self.state
            .lock()
            .expect("mock poisoned")
            .commands_received
            .clone()
    }

    /// Emit a domain event to subscribers. Tests use this to
    /// drive event-consumer plugins.
    ///
    /// # Errors
    ///
    /// Propagates the broadcast send error if there are no
    /// subscribers (`broadcast::error::SendError`). For tests
    /// where you don't care, ignore the result.
    pub fn emit_event(
        &self,
        event: DomainEvent,
    ) -> Result<usize, broadcast::error::SendError<DomainEvent>> {
        self.events.send(event)
    }
}

#[async_trait]
impl Host for MockHost {
    async fn process_command(
        &self,
        session: SessionId,
        cmd: Command,
    ) -> Result<Response, HostError> {
        let mut state = self.state.lock().expect("mock poisoned");

        // Verify the session is known.
        if !state.sessions.iter().any(|s| s.id == session && s.alive) {
            return Err(HostError::UnknownSession(session));
        }

        state.commands_received.push((session, cmd.clone()));

        for (matcher, response) in &state.responses {
            if matcher(&cmd) {
                return Ok(response.clone());
            }
        }
        Ok(state.default_response.clone())
    }

    async fn create_session(&self, transport: &'static str) -> Result<SessionId, HostError> {
        let id = SessionId::__internal_new(self.next_session_id.fetch_add(1, Ordering::SeqCst));
        let mut state = self.state.lock().expect("mock poisoned");
        state.sessions.push(MockSession {
            id,
            transport,
            user: None,
            level: PermissionLevel::Unvalidated,
            alive: true,
        });
        let _ = self.events.send(DomainEvent::SessionCreated {
            session: id,
            transport: transport.to_owned(),
        });
        Ok(id)
    }

    async fn end_session(&self, session: SessionId) -> Result<(), HostError> {
        let mut state = self.state.lock().expect("mock poisoned");
        for s in &mut state.sessions {
            if s.id == session {
                s.alive = false;
            }
        }
        let _ = self.events.send(DomainEvent::SessionEnded {
            session,
            reason: "test mock end_session".to_owned(),
        });
        Ok(())
    }

    async fn permission_ctx(&self, session: SessionId) -> Result<PermissionCtx, HostError> {
        let state = self.state.lock().expect("mock poisoned");
        let s = state
            .sessions
            .iter()
            .find(|s| s.id == session && s.alive)
            .ok_or(HostError::UnknownSession(session))?;
        Ok(PermissionCtx::__internal_new(s.id, s.user.clone(), s.level))
    }

    fn events(&self) -> broadcast::Receiver<DomainEvent> {
        self.events.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_and_end_session_round_trip() {
        let mock = MockHost::new();
        let id = mock.create_session("cli").await.unwrap();
        // session known → permission ctx works
        let ctx = mock.permission_ctx(id).await.unwrap();
        assert_eq!(ctx.session, id);
        assert_eq!(ctx.level, PermissionLevel::Unvalidated);
        assert!(ctx.username.is_none());

        mock.end_session(id).await.unwrap();
        // session ended → permission ctx errors
        let err = mock.permission_ctx(id).await.unwrap_err();
        assert!(matches!(err, HostError::UnknownSession(_)));
    }

    #[tokio::test]
    async fn unknown_session_errors_on_command() {
        let mock = MockHost::new();
        let bogus = SessionId::__internal_new(99_999);
        let err = mock
            .process_command(bogus, Command::Logout)
            .await
            .unwrap_err();
        assert!(matches!(err, HostError::UnknownSession(_)));
    }

    #[tokio::test]
    async fn scripted_response_matches() {
        let mock = MockHost::new();
        let id = mock.create_session("cli").await.unwrap();

        mock.set_response_for(|c| matches!(c, Command::Logout), Response::LoggedOut);

        let resp = mock.process_command(id, Command::Logout).await.unwrap();
        assert_eq!(resp, Response::LoggedOut);

        // Unmatched falls through to default.
        let resp = mock
            .process_command(id, Command::Help { topic: None })
            .await
            .unwrap();
        assert!(matches!(resp, Response::Text(_)));

        // The mock recorded both commands.
        let received = mock.commands_received();
        assert_eq!(received.len(), 2);
        assert_eq!(received[0].0, id);
        assert!(matches!(received[0].1, Command::Logout));
    }

    #[tokio::test]
    async fn pre_authenticated_session_helper() {
        let mock = MockHost::new();
        let user = Username::new("alice").unwrap();
        let id = mock.with_authenticated_session("test", user.clone(), PermissionLevel::Sysop);
        let ctx = mock.permission_ctx(id).await.unwrap();
        assert_eq!(ctx.username.as_ref(), Some(&user));
        assert_eq!(ctx.level, PermissionLevel::Sysop);
    }

    #[tokio::test]
    async fn events_fan_out_to_subscribers() {
        let mock = MockHost::new();
        let mut rx1 = mock.events();
        let mut rx2 = mock.events();

        let id = mock.create_session("cli").await.unwrap();

        // create_session emits a SessionCreated event.
        let ev = rx1.recv().await.unwrap();
        assert!(matches!(
            ev,
            DomainEvent::SessionCreated { session, .. } if session == id
        ));
        let ev = rx2.recv().await.unwrap();
        assert!(matches!(
            ev,
            DomainEvent::SessionCreated { session, .. } if session == id
        ));
    }
}
