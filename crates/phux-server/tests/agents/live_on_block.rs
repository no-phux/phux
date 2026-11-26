//! A closed phone's Lock Screen card hears its agents: a real server, a fake
//! `claude` painting a permission dialog into a real PTY, a phone that wrote
//! its Live Activity grant and left, and a loopback gateway.

use std::time::Duration;

use phux_protocol::wire::frame::{FrameKind, Scope};
use phux_server::LaunchCommand;
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_focused, recv_until, run_local, send_frame,
    spawn_server_with_seed_cmd, wait_for_socket,
};

use crate::push_on_block::{gateway, write_gated_agent};

const LIVE_DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn an_absent_phone_hears_the_counts_when_an_agent_blocks() {
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

        let mut phone = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        attach_focused(&mut phone, "demo").await;
        let key = "phux.live/v1/phone";
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

        let next = move |posted: std::sync::mpsc::Receiver<serde_json::Value>| async move {
            tokio::task::spawn_blocking(move || {
                let body = posted
                    .recv_timeout(LIVE_DEADLINE)
                    .expect("nothing was posted");
                (body, posted)
            })
            .await
            .unwrap()
        };
        // Leaving syncs the card to what is true now.
        let (body, posted) = next(posted).await;
        assert_eq!(
            (&body["host"], &body["waiting"]),
            (&"fr".into(), &0.into()),
            "{body}"
        );

        std::fs::write(tmp.path().join("go"), b"go").unwrap();
        let (mut body, mut posted) = next(posted).await;
        while body["waiting"] == 0 {
            (body, posted) = next(posted).await;
        }
        assert_eq!(body["waiting"], 1, "{body}");
        assert_eq!(body["schema_version"], 1, "{body}");
        let fields: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            fields,
            ["host", "schema_version", "waiting", "working"],
            "counts only"
        );
    });
}
