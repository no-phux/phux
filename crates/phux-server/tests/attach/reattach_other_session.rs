//! One connection can ATTACH, DETACH, and ATTACH a different session: DETACH
//! frees consumer state without closing the transport, the re-ATTACH
//! bootstraps the new session's pane, and the old session's output pump no
//! longer feeds this connection (phux-lskb).

use std::time::Duration;

use phux_protocol::wire::frame::{DetachReason, FrameKind};
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, attach_by_name_with_id, join_after_shutdown,
    recv_typed, recv_until_deadline, recv_until_detached, run_local, send_frame,
    spawn_server_seed_pty_no_cmd, wait_for_socket,
};

use super::common::{attached, contains, create_if_missing, focused_name, output_until};

fn counter(label: &str) -> Vec<String> {
    let script =
        format!("i=0; while :; do i=$((i+1)); printf '{label}-%d\\n' \"$i\"; sleep 0.05; done");
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script]
}

async fn detach(stream: &mut tokio::net::UnixStream) {
    send_frame(stream, &FrameKind::Detach).await;
    assert!(matches!(
        recv_until_detached(stream).await,
        FrameKind::Detached {
            reason: Some(DetachReason::Requested),
            ..
        }
    ));
}

#[test]
fn reattach_to_other_session_on_same_connection() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server_seed_pty_no_cmd(socket.clone(), None);
        for name in ["ALPHA", "BETA"] {
            let mut seed = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
            send_frame(
                &mut seed,
                &create_if_missing(&name.to_lowercase(), Some(counter(name)), None),
            )
            .await;
            attached(&mut seed).await;
            detach(&mut seed).await;
        }

        let mut client = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut client, &attach_by_name("alpha")).await;
        let alpha = attached(&mut client).await;
        assert_eq!(focused_name(&alpha), "alpha");
        output_until(&mut client, b"ALPHA").await;
        detach(&mut client).await;

        send_frame(&mut client, &attach_by_name_with_id("beta", 2)).await;
        // Strict read: nothing (such as stale ALPHA output) may precede it.
        let FrameKind::Attached { snapshot: beta, .. } = recv_typed(&mut client).await.1 else {
            panic!("re-ATTACH on the same connection must succeed");
        };
        assert_eq!(focused_name(&beta), "beta");
        assert_ne!(alpha.focused_resource, beta.focused_resource);
        match recv_typed(&mut client).await.1 {
            FrameKind::BootstrapBegin { terminal_id, .. } => {
                assert_eq!(terminal_id, beta.focused_resource);
            }
            other => panic!("expected BOOTSTRAP_BEGIN for beta, got {other:?}"),
        }

        let mut saw_beta = false;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(700);
        recv_until_deadline(&mut client, deadline, |_, frame| {
            if let FrameKind::ResourceOutput { bytes, .. } = frame {
                assert!(!contains(&bytes, b"ALPHA"), "old pump still forwarding");
                saw_beta |= contains(&bytes, b"BETA");
            }
            None::<()>
        })
        .await;
        assert!(saw_beta, "expected live BETA output after reattach");

        drop(client);
        join_after_shutdown(shutdown, server).await;
    });
}
