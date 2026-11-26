//! A block known only from the screen reaches an absent phone: a real
//! server, a fake `claude` painting a real permission dialog into a real PTY,
//! a phone that wrote its push grant and left, and a loopback gateway. No
//! `asked` event is ever raised here, so before the blocked notice existed
//! nothing was sent.

#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` demands pub(crate) in a test-binary module"
)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, Scope};
use phux_server::LaunchCommand;
use serde_json::Value;
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_focused, recv_until, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// The pane can be slow to get scheduled on a loaded box (see `agent_detect`).
const PUSH_DEADLINE: Duration = Duration::from_secs(60);

/// A fake `claude` that waits for `go`, then paints the permission dialog
/// the `claude` rules match, then idles.
pub(crate) fn write_gated_agent(dir: &Path) -> std::path::PathBuf {
    let gate = format!(
        "while [ ! -f '{}' ]; do sleep 0.1; done",
        dir.join("go").display()
    );
    crate::fake_agent::write_dialog_agent(dir, &gate, "sleep 60")
}

/// A loopback gateway: every JSON body posted to it, answered 202.
pub(crate) fn gateway() -> (String, mpsc::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://127.0.0.1:{}/v1/push",
        listener.local_addr().unwrap().port()
    );
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { return };
            let mut buf = vec![0u8; 8192];
            let n = conn.read(&mut buf).unwrap_or(0);
            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
            if let Some(body) = text.split("\r\n\r\n").nth(1)
                && let Ok(json) = serde_json::from_str(body)
            {
                let _ = tx.send(json);
            }
            let _ = conn.write_all(b"HTTP/1.1 202 Accepted\r\nConnection: close\r\n\r\n");
        }
    });
    (url, rx)
}

#[test]
fn a_screen_detected_block_pushes_an_absent_phone() {
    // As agent_detect: shorter detector timers, read once per process.
    unsafe {
        std::env::set_var("PHUX_AGENT_STARTUP_GRACE_MS", "200");
        std::env::set_var("PHUX_AGENT_IDENTIFY_RECHECK_MS", "200");
    }
    let (url, posted) = gateway();
    run_local(async move {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let agent = write_gated_agent(tmp.path());
        let _server = spawn_server_with_seed_cmd(socket.clone(), "demo", LaunchCommand::new(agent));

        // The phone: attach, write its grant, read it back as the barrier,
        // and leave.
        let mut phone = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let terminal = attach_focused(&mut phone, "demo").await;
        let key = "phux.push/v1/phone";
        let grant = format!(r#"{{"gateway":"{url}","grant":"0123456789abcdef","host":"fr"}}"#);
        send_frame(
            &mut phone,
            &phux_server_testkit::set_metadata(1, Scope::Global, key, grant.into_bytes()),
        )
        .await;
        send_frame(
            &mut phone,
            &phux_server_testkit::get_metadata(2, Scope::Global, key),
        )
        .await;
        let stored = recv_until(&mut phone, |_, frame| match frame {
            FrameKind::MetadataValue {
                request_id: 2,
                value,
            } => Some(value),
            _ => None,
        })
        .await;
        assert!(stored.is_some(), "the server refused the grant");
        drop(phone);
        tokio::time::sleep(Duration::from_millis(300)).await;

        // The agent asks for permission on screen only.
        std::fs::write(tmp.path().join("go"), b"go").unwrap();
        let body = tokio::task::spawn_blocking(move || posted.recv_timeout(PUSH_DEADLINE))
            .await
            .unwrap()
            .expect("the absent phone was never pushed for the block");
        let ResourceId::Local { id } = terminal else {
            panic!("the seeded pane is local");
        };
        assert_eq!(body["action"], "open", "{body}");
        assert_eq!(body["question"], "blocked", "{body}");
        assert_eq!(body["host"], "fr", "{body}");
        assert_eq!(body["pane"], format!("local:{id}"), "{body}");
    });
}
