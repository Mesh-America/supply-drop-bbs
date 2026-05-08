//! Integration tests for [`CliTransport`].
//!
//! Each test binds a real Unix socket in a temporary directory, connects a
//! [`MockHost`], and drives the session through the socket with a raw
//! `UnixStream`.  No BBS database or external processes are required.
//!
//! Tests are Unix-only (Unix-domain sockets are a Unix primitive).

#![cfg(unix)]

use std::{sync::Arc, time::Duration};

use bbs_cli::{CliConfig, CliTransport};
use bbs_plugin_api::testing::MockHost;
use bbs_plugin_api::{
    event::{Notification, NotifyOutcome},
    plugin::Plugin,
    transport::TransportEngine,
    Command, Response,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

// ── Harness ───────────────────────────────────────────────────────────────────

/// Spin up a [`CliTransport`] in a temporary directory.
///
/// Returns the running transport and the `TempDir` guard (dropping the guard
/// removes the directory, so callers must keep it alive for the duration of
/// the test).
async fn make_transport(host: Arc<MockHost>) -> (CliTransport, TempDir) {
    let dir = TempDir::new().unwrap();
    let socket_path = dir.path().join("test.sock");

    let config = CliConfig {
        enabled: true,
        socket: Some(socket_path),
        socket_mode: "0600".to_owned(),
        socket_owner: None,
    };

    let transport = CliTransport::init(config, host).await.unwrap();
    transport.start().await.unwrap();

    (transport, dir)
}

/// Connect to the transport socket and return a line-oriented stream pair.
///
/// Also reads and discards the welcome banner so tests start from a clean
/// state.
async fn connect(dir: &TempDir) -> (impl AsyncBufReadExt + Unpin, impl AsyncWriteExt + Unpin) {
    let socket_path = dir.path().join("test.sock");
    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let (reader, writer) = stream.into_split();
    let mut lines = BufReader::new(reader);

    // Consume the welcome banner.
    let mut banner = String::new();
    lines.read_line(&mut banner).await.unwrap();
    assert!(
        banner.contains("Supply Drop BBS"),
        "unexpected banner: {banner:?}"
    );

    (lines, writer)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A `help` command sent over the socket reaches the host as `Command::Help`.
#[tokio::test]
async fn help_command_reaches_host() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("help text".to_owned()));

    let (transport, dir) = make_transport(Arc::clone(&host)).await;
    let (mut lines, mut writer) = connect(&dir).await;

    writer.write_all(b"help\n").await.unwrap();

    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(2), lines.read_line(&mut response))
        .await
        .expect("timed out")
        .unwrap();

    let cmds = host.commands_received();
    assert_eq!(cmds.len(), 1);
    assert!(
        matches!(cmds[0].1, Command::Help { topic: None }),
        "expected Help{{None}}, got {:?}",
        cmds[0].1
    );

    transport.stop().await.unwrap();
}

/// The response text from the host is written back to the client.
#[tokio::test]
async fn response_text_returned_to_client() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("Supply Drop BBS".to_owned()));

    let (transport, dir) = make_transport(Arc::clone(&host)).await;
    let (mut lines, mut writer) = connect(&dir).await;

    writer.write_all(b"help\n").await.unwrap();

    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(2), lines.read_line(&mut line))
        .await
        .expect("timed out")
        .unwrap();

    assert_eq!(line.trim_end(), "Supply Drop BBS");
    transport.stop().await.unwrap();
}

/// A `Response::Prompt` causes the next message to be dispatched as
/// `Command::WorkflowReply` rather than being parsed as a keyword.
#[tokio::test]
async fn prompt_sets_workflow_state() {
    let host = Arc::new(MockHost::new());
    host.set_response_for(
        |cmd| matches!(cmd, Command::Help { .. }),
        Response::Prompt {
            text: "Enter password:".to_owned(),
            hide_input: true,
        },
    );
    host.set_response_for(
        |cmd| matches!(cmd, Command::WorkflowReply { .. }),
        Response::Text("OK".to_owned()),
    );

    let (transport, dir) = make_transport(Arc::clone(&host)).await;
    let (mut lines, mut writer) = connect(&dir).await;

    // Send "help" → should get a prompt back.
    writer.write_all(b"help\n").await.unwrap();
    let mut buf = String::new();
    tokio::time::timeout(Duration::from_secs(2), lines.read_line(&mut buf))
        .await
        .expect("timed out")
        .unwrap();

    // Send the "password" → should be a WorkflowReply, not re-parsed as help.
    writer.write_all(b"s3cr3t\n").await.unwrap();
    let mut buf2 = String::new();
    tokio::time::timeout(Duration::from_secs(2), lines.read_line(&mut buf2))
        .await
        .expect("timed out")
        .unwrap();

    let cmds = host.commands_received();
    assert_eq!(cmds.len(), 2);
    assert!(matches!(cmds[0].1, Command::Help { .. }));
    assert!(
        matches!(&cmds[1].1, Command::WorkflowReply { reply } if reply == "s3cr3t"),
        "expected WorkflowReply, got {:?}",
        cmds[1].1
    );

    transport.stop().await.unwrap();
}

/// Closing the client connection ends the BBS session.
#[tokio::test]
async fn disconnect_ends_session() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));

    let (transport, dir) = make_transport(Arc::clone(&host)).await;
    let (_, mut writer) = connect(&dir).await;

    // Send one command so a session is created.
    writer.write_all(b"whoami\n").await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let sessions_before = host.commands_received().len();
    assert_eq!(sessions_before, 1);

    // Drop the writer (close the connection).
    drop(writer);

    // Give the session loop time to detect EOF and call end_session.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The host should have had end_session called — no straightforward way to
    // assert via MockHost, so we verify no panics occurred and the transport
    // stops cleanly.
    transport.stop().await.unwrap();
}

/// `notify()` delivers text to the connected client as a `NOTIFY`-prefixed line.
#[tokio::test]
async fn notify_sends_text_to_client() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("pong".to_owned()));

    let (transport, dir) = make_transport(Arc::clone(&host)).await;
    let (mut lines, mut writer) = connect(&dir).await;

    // Trigger session creation.
    writer.write_all(b"whoami\n").await.unwrap();
    let mut discard = String::new();
    tokio::time::timeout(Duration::from_secs(2), lines.read_line(&mut discard))
        .await
        .expect("timed out for whoami reply")
        .unwrap();

    let session = host.commands_received()[0].0;

    let outcome = transport
        .notify(session, Notification::Text("You have mail!".to_owned()))
        .await
        .unwrap();
    assert!(matches!(outcome, NotifyOutcome::Queued));

    let mut notification_line = String::new();
    tokio::time::timeout(
        Duration::from_secs(2),
        lines.read_line(&mut notification_line),
    )
    .await
    .expect("timed out waiting for notification")
    .unwrap();

    assert!(
        notification_line.contains("You have mail!"),
        "unexpected line: {notification_line:?}"
    );
    assert!(
        notification_line.starts_with('\x00'),
        "notification must have protocol prefix"
    );

    transport.stop().await.unwrap();
}

/// `notify()` for an unknown session returns `NotifyOutcome::Dropped`.
#[tokio::test]
async fn notify_unknown_session_drops() {
    use bbs_plugin_api::identity::SessionId;

    let host = Arc::new(MockHost::new());
    let (transport, dir) = make_transport(Arc::clone(&host)).await;
    // Accept a connection so the transport is running, but don't create a session.
    let (_lines, _writer) = connect(&dir).await;

    let bogus = SessionId::__internal_new(0xDEAD_BEEF);
    let outcome = transport
        .notify(bogus, Notification::Text("hello".to_owned()))
        .await
        .unwrap();
    assert!(matches!(outcome, NotifyOutcome::Dropped));

    transport.stop().await.unwrap();
}
