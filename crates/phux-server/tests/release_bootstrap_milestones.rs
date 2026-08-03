//! Non-retried deterministic release gates for bootstrap reconnect and load.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::future_not_send
)]

use std::path::Path;
use std::time::{Duration, Instant};

use bytes::Bytes;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfile, ClientCapabilities, EngineCodec, EngineFeatureSet,
};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use phux_protocol::{BootstrapId, StreamId, TerminalId};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::{sleep, timeout};

use phux_server_testkit::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, recv_typed, run_local,
    send_frame, spawn_server_with_seed_cmd, wait_for_raw_socket, wait_for_socket,
};

#[derive(Clone, Copy, Debug)]
enum Milestone {
    HelloOk,
    Attached,
    Begin,
    Chunk,
    Ready,
    AttachReady,
}

impl Milestone {
    const ALL: [Self; 6] = [
        Self::HelloOk,
        Self::Attached,
        Self::Begin,
        Self::Chunk,
        Self::Ready,
        Self::AttachReady,
    ];

    const fn reached(self, frame: &FrameKind) -> bool {
        matches!(
            (self, frame),
            (Self::Attached, FrameKind::Attached { .. })
                | (Self::Begin, FrameKind::BootstrapBegin { .. })
                | (Self::Chunk, FrameKind::BootstrapChunk { .. })
                | (Self::Ready, FrameKind::BootstrapReady { .. })
                | (Self::AttachReady, FrameKind::AttachReady { .. })
        )
    }
}

fn attach_frame_at(id: u32, history: bool, viewport: ViewportInfo) -> FrameKind {
    FrameKind::Attach {
        attach_id: id,
        target: AttachTarget::ByName("release".to_owned()),
        viewport,
        request_scrollback: history,
        scrollback_limit_lines: if history { 50_000 } else { 0 },
    }
}

fn attach_frame(id: u32, history: bool) -> FrameKind {
    attach_frame_at(id, history, ViewportInfo::new(200, 60))
}

async fn wait_for_fullscreen_marker(path: &Path) {
    let mut stream = wait_for_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &attach_frame_at(u32::MAX, false, ViewportInfo::new(80, 24)),
    )
    .await;
    let mut screen = phux_server_testkit::screen::Screen::new(80, 24).expect("screen oracle");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (_, frame) = timeout(remaining, recv_typed(&mut stream))
            .await
            .expect("full-screen marker did not reach the terminal actor");
        match frame {
            FrameKind::BootstrapChunk { payload, .. } => screen.write(&payload),
            FrameKind::TerminalOutput { bytes, .. } => screen.write(&bytes),
            _ => {}
        }
        if screen.contains("RELEASE-FULLSCREEN") {
            return;
        }
    }
}

async fn disconnect_at(path: &Path, milestone: Milestone, attach_id: u32) {
    let mut stream = wait_for_socket(path, SOCKET_CONNECT_DEADLINE).await;
    if matches!(milestone, Milestone::HelloOk) {
        drop(stream);
        return;
    }
    send_frame(&mut stream, &attach_frame(attach_id, false)).await;
    loop {
        let (_, frame) = recv_typed(&mut stream).await;
        if milestone.reached(&frame) {
            drop(stream);
            return;
        }
    }
}

async fn attach_ready(path: &Path, attach_id: u32) -> UnixStream {
    let mut stream = wait_for_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &attach_frame(attach_id, false)).await;
    loop {
        let (_, frame) = recv_typed(&mut stream).await;
        if matches!(frame, FrameKind::AttachReady { attach_id: id } if id == attach_id) {
            return stream;
        }
    }
}

#[test]
fn reconnect_succeeds_after_every_bootstrap_milestone() {
    run_local(async {
        let tmp = TempDir::new().expect("tempdir");
        let socket = tmp.path().join("release.sock");
        let mut command = CommandBuilder::new("/bin/sh");
        command.arg("-c");
        command
            .arg("printf '\x1b[?1049h\x1b[2J\x1b[1;1Hrelease-reconnect\x1b[?1049l'; exec /bin/cat");
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "release", command);

        for (index, milestone) in Milestone::ALL.into_iter().enumerate() {
            let id = u32::try_from(index).expect("six milestones fit in u32");
            disconnect_at(&socket, milestone, id + 1).await;
            // Every cut is followed immediately by a complete fresh attach;
            // this proves cleanup, not just that the disconnect itself returns.
            let probe = attach_ready(&socket, id + 101).await;
            drop(probe);
        }

        shutdown.send(()).ok();
        timeout(SERVER_JOIN_DEADLINE, server)
            .await
            .expect("server shutdown timeout")
            .expect("server task")
            .expect("server result");
        assert!(
            !socket.exists(),
            "server leaked UDS after milestone reconnects"
        );
    });
}

#[derive(Debug)]
struct AttachedClient {
    stream: UnixStream,
    terminal_id: TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    cursor: Bytes,
    bootstrap_bytes: Vec<u8>,
}

async fn wait_for_native_socket(path: &Path) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "release-native-history-gate".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new().with_bootstrap(
                BootstrapCapabilities::new().with_native(
                    EngineCodec::LibghosttyCheckpointV2,
                    EngineFeatureSet::required_native(),
                ),
            ),
        },
    )
    .await;
    let (_, reply) = recv_typed(&mut stream).await;
    assert!(matches!(
        reply,
        FrameKind::HelloOk {
            selected_profile: BootstrapProfile::NativeState { .. },
            ..
        }
    ));
    stream
}

async fn attach_with_history(path: &Path, attach_id: u32) -> AttachedClient {
    let mut stream = wait_for_native_socket(path).await;
    send_frame(
        &mut stream,
        &attach_frame_at(attach_id, true, ViewportInfo::new(80, 24)),
    )
    .await;
    let mut terminal_id = None;
    let mut stream_id = None;
    let mut bootstrap_id = None;
    let mut cursor = None;
    let mut bootstrap_bytes = Vec::new();
    let mut ready = false;
    loop {
        let (_, frame) = recv_typed(&mut stream).await;
        match frame {
            FrameKind::Attached { snapshot, .. } => {
                terminal_id = snapshot.panes.first().map(|pane| pane.id.clone());
            }
            FrameKind::BootstrapBegin {
                stream_id: stream,
                bootstrap_id: bootstrap,
                ..
            } => {
                stream_id = Some(stream);
                bootstrap_id = Some(bootstrap);
            }
            FrameKind::BootstrapChunk { payload, .. } => {
                bootstrap_bytes.extend_from_slice(&payload);
            }
            FrameKind::BootstrapReady { history_cursor, .. } => {
                cursor = history_cursor;
                ready = true;
            }
            FrameKind::AttachReady { attach_id: id } if id == attach_id && ready => break,
            FrameKind::TerminalOutput { .. } => {}
            other => panic!("unexpected attach frame: {other:?}"),
        }
    }
    AttachedClient {
        stream,
        terminal_id: terminal_id.expect("attached terminal"),
        stream_id: stream_id.expect("bootstrap stream"),
        bootstrap_id: bootstrap_id.expect("bootstrap generation"),
        cursor: cursor.expect("warm 50k history cursor"),
        bootstrap_bytes,
    }
}

async fn request_page(client: &mut AttachedClient, cursor: Bytes) {
    send_frame(
        &mut client.stream,
        &FrameKind::HistoryRequest {
            terminal_id: client.terminal_id.clone(),
            stream_id: client.stream_id,
            bootstrap_id: client.bootstrap_id,
            cursor,
            max_bytes: 1024 * 1024,
            max_rows: 512,
        },
    )
    .await;
}

async fn receive_page(client: &mut AttachedClient) -> (u32, Option<Bytes>) {
    let deadline = Instant::now() + WIRE_RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (_, frame) = timeout(remaining, recv_typed(&mut client.stream))
            .await
            .expect("history page timeout");
        match frame {
            FrameKind::HistoryPage {
                stream_id,
                bootstrap_id,
                payload,
                rows,
                next_cursor,
                ..
            } => {
                assert_eq!(stream_id, client.stream_id);
                assert_eq!(bootstrap_id, client.bootstrap_id);
                assert!(
                    !payload.is_empty(),
                    "native history page cannot advance with an empty payload",
                );
                return (rows, next_cursor);
            }
            FrameKind::TerminalOutput { .. } => {}
            other => panic!("expected history page, got {other:?}"),
        }
    }
}

#[test]
fn warm_50k_fullscreen_eight_clients_one_stalled_history_cache() {
    run_local(async {
        let tmp = TempDir::new().expect("tempdir");
        let socket = tmp.path().join("release-load.sock");
        let warm = tmp.path().join("warm.ready");
        let script = format!(
            "i=1; while [ $i -le 50000 ]; do printf 'warm-%08d α界\\r\\n' \"$i\"; i=$((i+1)); done; \
             printf '\\033[?1049h\\033[?2026h\\033[2J\\033[1;1H\\033[38;2;70;210;130mRELEASE-FULLSCREEN\\033[0m\\033[?2026l'; \
             : > '{}'; exec /bin/cat",
            warm.display(),
        );
        let mut command = CommandBuilder::new("/bin/sh");
        command.arg("-c");
        command.arg(script);
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "release", command);
        let began = Instant::now();
        while !warm.exists() && began.elapsed() < Duration::from_secs(60) {
            sleep(Duration::from_millis(10)).await;
        }
        assert!(warm.exists(), "50k-line PTY producer did not become warm");
        // Observe the synthesized grid plus live stream until the marker at
        // the end of the corpus is rendered. The native cuts below therefore
        // cannot race bytes that the PTY reader accepted but the actor has not
        // yet applied.
        wait_for_fullscreen_marker(&socket).await;

        let mut clients = Vec::with_capacity(8);
        for attach_id in 1..=8 {
            clients.push(attach_with_history(&socket, attach_id).await);
        }
        // Native records are opaque codec data, so the server gate must not
        // search their encoded representation for application text. The
        // browser/client parity gates decode this same profile and assert the
        // visible marker; this gate proves the warm TUI cut was published.
        assert!(
            !clients[0].bootstrap_bytes.is_empty(),
            "full-screen TUI produced an empty native bootstrap",
        );

        // Client 8 takes a lease/request and then stops consuming. The other
        // seven must remain independently pageable.
        let stalled_cursor = clients[7].cursor.clone();
        request_page(&mut clients[7], stalled_cursor).await;
        for client in &mut clients[..7] {
            let cursor = client.cursor.clone();
            request_page(client, cursor).await;
        }
        for client in &mut clients[1..7] {
            let _ = receive_page(client).await;
        }

        // Fully page one active cache. Most rows in this corpus live in the
        // fragmented pre-READY SCREEN record; this counts the additional
        // predecessor pages published progressively after READY.
        let mut total_rows = 0_u32;
        let mut next = {
            let (rows, next) = receive_page(&mut clients[0]).await;
            total_rows += rows;
            next
        };
        while let Some(cursor) = next {
            request_page(&mut clients[0], cursor).await;
            let (rows, following) = receive_page(&mut clients[0]).await;
            total_rows += rows;
            next = following;
        }
        assert!(
            total_rows > 0,
            "50k corpus produced no progressive post-READY history",
        );

        drop(clients);
        shutdown.send(()).ok();
        timeout(SERVER_JOIN_DEADLINE, server)
            .await
            .expect("server shutdown timeout")
            .expect("server task")
            .expect("server result");
        assert!(!socket.exists(), "server leaked UDS after load gate");
    });
}
