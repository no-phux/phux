//! `ATTACH` target resolution (L1 §8, SPEC §14): `ByName`, `Last`, and
//! `CreateIfMissing`, including the wire `cwd`.

use std::time::Duration;

use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, ErrorCode, FrameKind, StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;
use tempfile::TempDir;
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, command, recv_typed, run_local, send_frame,
    spawn_server, spawn_server_connected, spawn_server_seed_pty_no_cmd, wait_for_socket,
};

use super::common::{
    attach, attached, canonical_cwd, create_if_missing, focused_name, with_attach_id,
};

async fn state(stream: &mut UnixStream, request_id: u32) -> SessionSnapshot {
    let get_state = Command::GetState {
        scope: StateScope::Server,
    };
    match command(stream, request_id, get_state).await {
        CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
        other => panic!("GET_STATE failed: {other:?}"),
    }
}

/// The `ERROR` an `ATTACH` fails with: no `request_id` (it is not a COMMAND).
async fn attach_error(stream: &mut UnixStream) -> (ErrorCode, String) {
    match recv_typed(stream).await.1 {
        FrameKind::Error {
            request_id: None,
            code,
            message,
        } => (code, message),
        other => panic!("expected an uncorrelated ERROR, got {other:?}"),
    }
}

/// On a seedless server, an unknown name and `Last` both fail with the
/// recoverable `SessionNotFound` (no stray frames, connection stays usable,
/// nothing created); `CreateIfMissing` then creates the session.
#[test]
fn failed_attach_is_recoverable_and_create_if_missing_creates() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;

        send_frame(&mut stream, &attach_by_name("does-not-exist")).await;
        let (code, message) = attach_error(&mut stream).await;
        assert_eq!(code, ErrorCode::SessionNotFound);
        assert_eq!(code as u16, 102, "SPEC §14 code");
        assert!(message.contains("does-not-exist"), "{message}");
        // Atomic failure: nothing else is queued behind the ERROR.
        let mut sink = [0u8; 16];
        let quiet = timeout(Duration::from_millis(100), stream.read(&mut sink)).await;
        assert!(
            quiet.is_err(),
            "unexpected bytes or close after ERROR: {quiet:?}"
        );

        send_frame(
            &mut stream,
            &with_attach_id(attach(AttachTarget::Last, 80, 24), 2),
        )
        .await;
        let (code, message) = attach_error(&mut stream).await;
        assert_eq!(code, ErrorCode::SessionNotFound);
        assert!(
            message.to_lowercase().contains("no live session"),
            "{message}"
        );
        assert!(
            state(&mut stream, 9).await.sessions.is_empty(),
            "Last never creates"
        );

        send_frame(
            &mut stream,
            &with_attach_id(create_if_missing("foo", None, None), 3),
        )
        .await;
        let snapshot = attached(&mut stream).await;
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].name, "foo");
        assert_eq!((snapshot.windows.len(), snapshot.resources.len()), (1, 1));
        let (_, begin) = recv_typed(&mut stream).await;
        assert!(matches!(
            begin,
            FrameKind::BootstrapBegin {
                cols: 80,
                rows: 24,
                ..
            }
        ));
    });
}

/// `Last` resolves the configured seed on a fresh server, then the most
/// recently *focused* session rather than the last-attached one.
/// `CreateIfMissing` on an existing name reuses it.
#[test]
fn last_follows_the_seed_then_focus_and_create_if_missing_reuses() {
    run_local(async {
        let seed = "phux-custom-project";
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (_shutdown, _server) = spawn_server(socket.clone(), Some(seed));
        let connect = || wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE);

        let mut first = connect().await;
        send_frame(&mut first, &attach(AttachTarget::Last, 80, 24)).await;
        let snapshot = attached(&mut first).await;
        assert_eq!(focused_name(&snapshot), seed);
        let seed_pane = snapshot.focused_resource;

        let mut reuse = connect().await;
        send_frame(&mut reuse, &create_if_missing(seed, None, None)).await;
        let snapshot = attached(&mut reuse).await;
        assert_eq!((snapshot.sessions.len(), snapshot.resources.len()), (1, 1));

        let mut other = connect().await;
        send_frame(&mut other, &create_if_missing("other", None, None)).await;
        assert_eq!(focused_name(&attached(&mut other).await), "other");

        // INPUT_FOCUS rides the input lane, so poll GET_STATE (which reads
        // the touched order itself) rather than trusting a PING barrier.
        let focus = FrameKind::InputFocus {
            terminal_id: seed_pane,
            event: FocusEvent::Gained,
        };
        send_frame(&mut first, &focus).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        for request_id in 100.. {
            if focused_name(&state(&mut first, request_id).await) == seed {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "focus touch never landed"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let mut last = connect().await;
        send_frame(&mut last, &attach(AttachTarget::Last, 80, 24)).await;
        assert_eq!(focused_name(&attached(&mut last).await), seed);
    });
}

/// The seed pane honours a wire `cwd`, falls back when it is not a directory,
/// and the `ATTACHED` snapshot reports a pane's live (post-`cd`) directory.
#[test]
fn attach_reports_wire_fallback_and_live_cwd() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (_shutdown, _server) = spawn_server_seed_pty_no_cmd(socket.clone(), None);
        let blocked = |script: &str| {
            Some(vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                script.to_owned(),
            ])
        };
        let dir = TempDir::new().unwrap();
        let dir_path = dir.path().canonicalize().unwrap();

        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let honored = create_if_missing(
            "honored",
            blocked("read _"),
            Some(dir_path.display().to_string()),
        );
        send_frame(&mut stream, &honored).await;
        assert_eq!(canonical_cwd(&attached(&mut stream).await, 0), dir_path);

        let bogus = tmp.path().join("does-not-exist");
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let fallback = create_if_missing(
            "fallback",
            blocked("read _"),
            Some(bogus.display().to_string()),
        );
        send_frame(&mut stream, &fallback).await;
        let snapshot = attached(&mut stream).await;
        let pane = snapshot
            .resources
            .iter()
            .position(|p| p.id == snapshot.focused_resource)
            .unwrap();
        let cwd = canonical_cwd(&snapshot, pane);
        assert!(
            cwd != bogus && cwd.is_dir(),
            "bogus cwd must fall back: {}",
            cwd.display()
        );

        // Spawned elsewhere, then `cd`s: the attach-time kernel refresh wins.
        let live = TempDir::new().unwrap();
        let live_path = live.path().canonicalize().unwrap();
        let script = format!("cd '{}' && read _", live_path.display());
        let mut creator = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(
            &mut creator,
            &create_if_missing(
                "live",
                blocked(&script),
                Some(dir_path.display().to_string()),
            ),
        )
        .await;
        attached(&mut creator).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut stream = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &attach_by_name("live")).await;
        let snapshot = attached(&mut stream).await;
        let pane = snapshot
            .resources
            .iter()
            .position(|p| p.id == snapshot.focused_resource)
            .unwrap();
        assert_eq!(canonical_cwd(&snapshot, pane), live_path);
    });
}
