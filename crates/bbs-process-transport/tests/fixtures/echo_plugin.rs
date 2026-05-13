//! Echo plugin — a minimal Supply Drop process transport used in integration tests.
//!
//! Accepts a `--mode` argument that controls which IPC sequence it emits:
//!
//! - `scripted` (default): ready → open → recv("help") → recv("hunter2") → close → wait shutdown
//! - `hold`:               ready → open → recv("whoami") → *hold until shutdown* → close
//!
//! The `scripted` mode is used to verify the full IPC dispatch pipeline.
//! The `hold` mode is used to keep a session alive so tests can call
//! `notify()` while the session is still open.

use std::io::{self, BufRead, Write};

use bbs_process_transport::{HostMsg, PluginMsg};

fn emit(msg: &PluginMsg) {
    println!("{}", serde_json::to_string(msg).unwrap());
    io::stdout().flush().unwrap();
}

/// Block on stdin until a `shutdown` message arrives.  Returns true when
/// shutdown was received, false when stdin is closed (host crashed).
fn wait_for_shutdown() -> bool {
    let stdin = io::stdin();
    for raw in stdin.lock().lines() {
        let line = match raw {
            Ok(l) => l,
            Err(_) => return false,
        };
        if let Ok(HostMsg::Shutdown) = serde_json::from_str(&line) {
            return true;
        }
    }
    false
}

fn main() {
    let mode = std::env::args()
        .skip_while(|a| a != "--mode")
        .nth(1)
        .unwrap_or_else(|| "scripted".to_owned());

    // Signal readiness (no payload limit, report version for tests).
    emit(&PluginMsg::Ready {
        payload_limit: 0,
        version: Some("test-echo-plugin".to_owned()),
    });

    // Open one connection.
    emit(&PluginMsg::Open {
        id: "c1".to_owned(),
    });

    match mode.as_str() {
        "hold" => {
            // Send one recv so tests can obtain the session ID from
            // MockHost::commands_received(), then hold the connection open.
            emit(&PluginMsg::Recv {
                id: "c1".to_owned(),
                line: "whoami".to_owned(),
            });

            // Block until the host sends shutdown.
            wait_for_shutdown();

            // Close our connection before exiting.
            emit(&PluginMsg::Close {
                id: "c1".to_owned(),
            });
        }
        _ => {
            // scripted: two recv messages, then close.
            emit(&PluginMsg::Recv {
                id: "c1".to_owned(),
                line: "help".to_owned(),
            });
            emit(&PluginMsg::Recv {
                id: "c1".to_owned(),
                line: "hunter2".to_owned(),
            });
            emit(&PluginMsg::Close {
                id: "c1".to_owned(),
            });

            // Wait so we don't exit while the host is still writing responses.
            wait_for_shutdown();
        }
    }
}
