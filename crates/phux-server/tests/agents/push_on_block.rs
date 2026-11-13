//! A block known only from the screen reaches an absent phone: a real
//! server, a fake `claude` painting a real permission dialog into a real PTY,
//! a phone that wrote its push grant and left, and a loopback gateway. No
//! `asked` event is ever raised here, so before the blocked notice existed
//! nothing was sent.

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
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_until, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

/// The pane can be slow to get scheduled on a loaded box (see `agent_detect`).
const PUSH_DEADLINE: Duration = Duration::from_secs(60);

/// A fake `claude` that waits for `go`, then paints the permission dialog
/// the `claude` rules match, then idles.
fn write_gated_agent(dir: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("claude");
    let rule = "\\342\\224\\200".repeat(20);
    let script = format!(
        "#!/bin/sh\n\
         while [ ! -f '{go}' ]; do sleep 0.1; done\n\
         printf '\\033[2J\\033[H'\n\
         echo 'some transcript output above the live chrome'\n\
         echo ''\n\
         printf '{rule}\\n'\n\
         echo ' Bash command'\n\
         echo ''\n\
         echo '   touch /tmp/probe.txt'\n\
         echo ''\n\
         echo ' Do you want to proceed?'\n\
         printf ' \\342\\235\\257 1. Yes\\n'\n\
         echo '   2. Yes, and always allow access'\n\
         echo '   3. No'\n\
         echo ''\n\
         echo ' Esc to cancel'\n\
         sleep 60\n",
        go = dir.join("go").display(),
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A loopback gateway: every JSON body posted to it, answered 202.
fn gateway() -> (String, mpsc::Receiver<Value>) {
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
        send_frame(&mut phone, &attach_by_name("demo")).await;
        let terminal: ResourceId = recv_until(&mut phone, |_, frame| match frame {
            FrameKind::Attached { snapshot, .. } => Some(snapshot.focused_resource),
            _ => None,
        })
        .await;
        let key = "phux.push/v1/phone";
        let grant = format!(r#"{{"gateway":"{url}","grant":"0123456789abcdef","host":"fr"}}"#);
        send_frame(
            &mut phone,
            &FrameKind::SetMetadata {
                request_id: 1,
                scope: Scope::Global,
                key: key.to_owned(),
                value: grant.into_bytes(),
            },
        )
        .await;
        send_frame(
            &mut phone,
            &FrameKind::GetMetadata {
                request_id: 2,
                scope: Scope::Global,
                key: key.to_owned(),
            },
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
