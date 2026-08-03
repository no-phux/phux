//! Non-retried deterministic release gates for bootstrap reconnect and load.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::future_not_send
)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::stream::{FuturesUnordered, StreamExt};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfile, ClientCapabilities, EngineCodec, EngineFeatureSet,
};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use phux_protocol::{BootstrapId, StreamId, TerminalId};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::Barrier;
use tokio::time::timeout;

use common::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, recv_typed,
    recv_until_detached, run_local, send_frame, spawn_server_with_seed_cmd, wait_for_raw_socket,
    wait_for_socket,
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

const LIVE_SENTINEL: &[u8] = b"PHUX-RELEASE-LIVE-ONCE-7C91";
const LIVE_BOUNDARY: &[u8] = b"PHUX-RELEASE-LIVE-BOUNDARY-2A4E";

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[derive(Debug)]
struct EstablishedClient {
    stream: UnixStream,
    terminal_id: TerminalId,
    live_bytes: Vec<u8>,
}

async fn wait_for_fullscreen_marker(path: &Path) -> EstablishedClient {
    let mut stream = wait_for_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &attach_frame_at(u32::MAX, false, ViewportInfo::new(80, 24)),
    )
    .await;
    let mut screen = common::screen::Screen::new(80, 24).expect("screen oracle");
    let mut terminal_id = None;
    let mut marker_seen = false;
    let mut attach_ready = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !(marker_seen && attach_ready) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (_, frame) = timeout(remaining, recv_typed(&mut stream))
            .await
            .expect("full-screen marker did not reach the terminal actor");
        match frame {
            FrameKind::Attached { snapshot, .. } => {
                terminal_id = snapshot.panes.first().map(|pane| pane.id.clone());
            }
            FrameKind::BootstrapChunk { payload, .. } => screen.write(&payload),
            FrameKind::TerminalOutput { bytes, .. } => screen.write(&bytes),
            FrameKind::AttachReady { attach_id } if attach_id == u32::MAX => {
                attach_ready = true;
            }
            _ => {}
        }
        marker_seen = screen.contains("RELEASE-FULLSCREEN");
    }
    EstablishedClient {
        stream,
        terminal_id: terminal_id.expect("established terminal id"),
        live_bytes: Vec::new(),
    }
}

async fn send_live_bytes(client: &mut EstablishedClient, bytes: &[u8]) {
    send_frame(
        &mut client.stream,
        &FrameKind::InputPaste {
            terminal_id: client.terminal_id.clone(),
            event: PasteEvent {
                trust: PasteTrust::Trusted,
                data: bytes.to_vec(),
            },
        },
    )
    .await;
}

async fn receive_established_until(client: &mut EstablishedClient, marker: &[u8]) {
    let deadline = Instant::now() + WIRE_RECV_TIMEOUT;
    while !client
        .live_bytes
        .windows(marker.len())
        .any(|window| window == marker)
    {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (_, frame) = timeout(remaining, recv_typed(&mut client.stream))
            .await
            .expect("established client was delayed by a stalled bootstrap owner");
        if let FrameKind::TerminalOutput { bytes, .. } = frame {
            client.live_bytes.extend_from_slice(&bytes);
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
struct BootstrappingClient {
    attach_id: u32,
    stream: UnixStream,
    terminal_id: TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
}

#[derive(Debug)]
struct AttachedClient {
    attach_id: u32,
    stream: UnixStream,
    terminal_id: TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    cursor: Bytes,
    bootstrap_records: Vec<Bytes>,
    live_bytes: Vec<u8>,
    pending_cursor: Option<Bytes>,
    expected_page_seq: u64,
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

async fn attach_through_begin(
    mut stream: UnixStream,
    attach_id: u32,
    attach_barrier: Arc<Barrier>,
) -> BootstrappingClient {
    attach_barrier.wait().await;
    send_frame(
        &mut stream,
        &attach_frame_at(attach_id, true, ViewportInfo::new(80, 24)),
    )
    .await;
    let mut terminal_id = None;
    loop {
        let (_, frame) = recv_typed(&mut stream).await;
        match frame {
            FrameKind::Attached { snapshot, .. } => {
                terminal_id = snapshot.panes.first().map(|pane| pane.id.clone());
            }
            FrameKind::BootstrapBegin {
                terminal_id: begin_terminal,
                stream_id,
                bootstrap_id,
                ..
            } => {
                let terminal_id = terminal_id
                    .take()
                    .unwrap_or_else(|| panic!("client {attach_id}: BEGIN preceded ATTACHED"));
                assert_eq!(
                    begin_terminal, terminal_id,
                    "client {attach_id}: BEGIN terminal identity"
                );
                return BootstrappingClient {
                    attach_id,
                    stream,
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                };
            }
            other => panic!("client {attach_id}: unexpected pre-BEGIN frame: {other:?}"),
        }
    }
}

fn record_live_output(
    client: &mut AttachedClient,
    terminal_id: TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    bytes: &Bytes,
) {
    assert_eq!(
        terminal_id, client.terminal_id,
        "client {}: live terminal identity",
        client.attach_id
    );
    assert_eq!(
        stream_id, client.stream_id,
        "client {}: live stream identity",
        client.attach_id
    );
    assert_eq!(
        bootstrap_id, client.bootstrap_id,
        "client {}: live generation identity",
        client.attach_id
    );
    client.live_bytes.extend_from_slice(bytes);
}

async fn finish_attach(
    partial: BootstrappingClient,
    required_live_marker: Option<&[u8]>,
) -> AttachedClient {
    let BootstrappingClient {
        attach_id,
        stream,
        terminal_id,
        stream_id,
        bootstrap_id,
    } = partial;
    let mut client = AttachedClient {
        attach_id,
        stream,
        terminal_id,
        stream_id,
        bootstrap_id,
        cursor: Bytes::new(),
        bootstrap_records: Vec::new(),
        live_bytes: Vec::new(),
        pending_cursor: None,
        expected_page_seq: 1,
    };
    let mut ready = false;
    let mut attach_ready = false;
    loop {
        let (_, frame) = recv_typed(&mut client.stream).await;
        match frame {
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => {
                assert_eq!(terminal_id, client.terminal_id);
                assert_eq!(stream_id, client.stream_id);
                assert_eq!(bootstrap_id, client.bootstrap_id);
                assert_eq!(
                    count_occurrences(&payload, LIVE_SENTINEL),
                    0,
                    "client {attach_id}: post-fence sentinel leaked into checkpoint"
                );
                assert_eq!(
                    usize::try_from(chunk_seq).expect("chunk sequence fits usize"),
                    client.bootstrap_records.len(),
                    "client {attach_id}: bootstrap records are contiguous"
                );
                client.bootstrap_records.push(payload);
            }
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => {
                assert!(!ready, "client {attach_id}: duplicate READY");
                assert_eq!(terminal_id, client.terminal_id);
                assert_eq!(stream_id, client.stream_id);
                assert_eq!(bootstrap_id, client.bootstrap_id);
                client.cursor = history_cursor
                    .unwrap_or_else(|| panic!("client {attach_id}: missing warm history cursor"));
                ready = true;
            }
            FrameKind::AttachReady { attach_id: id } if id == attach_id => {
                assert!(ready, "client {attach_id}: ATTACH_READY preceded READY");
                attach_ready = true;
            }
            FrameKind::TerminalOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                bytes,
                ..
            } => {
                assert!(
                    ready && attach_ready,
                    "client {attach_id}: live output overtook READY/ATTACH_READY"
                );
                record_live_output(&mut client, terminal_id, stream_id, bootstrap_id, &bytes);
            }
            other => panic!("client {attach_id}: unexpected attach frame: {other:?}"),
        }
        let marker_seen = required_live_marker.is_none_or(|marker| {
            client
                .live_bytes
                .windows(marker.len())
                .any(|window| window == marker)
        });
        if ready && attach_ready && marker_seen {
            return client;
        }
    }
}

async fn request_page(client: &mut AttachedClient, cursor: Bytes) {
    assert!(
        client.pending_cursor.is_none(),
        "client {}: overlapping history requests",
        client.attach_id
    );
    client.pending_cursor = Some(cursor.clone());
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

fn accept_history_page(
    client: &mut AttachedClient,
    terminal_id: TerminalId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    page_seq: u64,
    cursor: Bytes,
    next_cursor: Option<Bytes>,
    payload: Bytes,
    rows: u32,
) -> (u32, Option<Bytes>) {
    assert_eq!(terminal_id, client.terminal_id);
    assert_eq!(stream_id, client.stream_id);
    assert_eq!(bootstrap_id, client.bootstrap_id);
    assert_eq!(
        page_seq, client.expected_page_seq,
        "client {}: page lineage must start at 1 and advance independently",
        client.attach_id
    );
    let requested = client
        .pending_cursor
        .take()
        .unwrap_or_else(|| panic!("client {}: unsolicited history page", client.attach_id));
    assert_eq!(
        cursor, requested,
        "client {}: response consumed another owner's cursor",
        client.attach_id
    );
    assert!(
        !payload.is_empty(),
        "client {}: native history page cannot advance empty",
        client.attach_id
    );
    client.expected_page_seq += 1;
    (rows, next_cursor)
}

async fn receive_page(client: &mut AttachedClient) -> (u32, Option<Bytes>) {
    let deadline = Instant::now() + WIRE_RECV_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (_, frame) = timeout(remaining, recv_typed(&mut client.stream))
            .await
            .unwrap_or_else(|_| panic!("client {}: history page timeout", client.attach_id));
        match frame {
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                cursor,
                next_cursor,
                payload,
                rows,
            } => {
                return accept_history_page(
                    client,
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    page_seq,
                    cursor,
                    next_cursor,
                    payload,
                    rows,
                );
            }
            FrameKind::TerminalOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                bytes,
                ..
            } => record_live_output(client, terminal_id, stream_id, bootstrap_id, &bytes),
            other => panic!(
                "client {}: expected history page, got {other:?}",
                client.attach_id
            ),
        }
    }
}

async fn finish_history(
    mut client: AttachedClient,
    request_barrier: Arc<Barrier>,
) -> (AttachedClient, u32, u64) {
    request_barrier.wait().await;
    let mut cursor = Some(client.cursor.clone());
    let mut total_rows = 0_u32;
    while let Some(requested) = cursor {
        request_page(&mut client, requested).await;
        let (rows, next) = receive_page(&mut client).await;
        total_rows = total_rows
            .checked_add(rows)
            .expect("50k history row count fits u32");
        cursor = next;
    }
    assert!(
        total_rows > 0,
        "client {}: 50k corpus reached FINISH without history rows",
        client.attach_id
    );
    let pages = client.expected_page_seq - 1;
    (client, total_rows, pages)
}

async fn receive_until_live_marker(client: &mut AttachedClient, marker: &[u8]) {
    let deadline = Instant::now() + WIRE_RECV_TIMEOUT;
    while !client
        .live_bytes
        .windows(marker.len())
        .any(|window| window == marker)
    {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let (_, frame) = timeout(remaining, recv_typed(&mut client.stream))
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "client {}: live boundary delayed by stalled owner",
                    client.attach_id
                )
            });
        match frame {
            FrameKind::TerminalOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                bytes,
                ..
            } => record_live_output(client, terminal_id, stream_id, bootstrap_id, &bytes),
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                cursor,
                next_cursor,
                payload,
                rows,
            } => {
                let _ = accept_history_page(
                    client,
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    page_seq,
                    cursor,
                    next_cursor,
                    payload,
                    rows,
                );
            }
            other => panic!(
                "client {}: expected live boundary, got {other:?}",
                client.attach_id
            ),
        }
    }
}

async fn detach_attached(mut client: AttachedClient) {
    send_frame(&mut client.stream, &FrameKind::Detach).await;
    assert!(matches!(
        recv_until_detached(&mut client.stream).await,
        FrameKind::Detached
    ));
}

#[test]
fn warm_50k_fullscreen_eight_clients_one_stalled_history_cache() {
    run_local(async {
        let _capture =
            common::tracing_capture::TracingCapture::install("release-bootstrap-milestones");
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
        timeout(Duration::from_secs(60), async {
            while !warm.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("50k-line PTY producer did not become warm");

        // This already-published client is the control: its live stream must
        // remain responsive while the eight native generations are captured.
        let mut established = wait_for_fullscreen_marker(&socket).await;

        // HELLO negotiation is intentionally complete on every socket before
        // any ATTACH is allowed onto the wire.
        let mut negotiated = Vec::with_capacity(8);
        for attach_id in 1..=8 {
            negotiated.push((attach_id, wait_for_native_socket(&socket).await));
        }
        let attach_barrier = Arc::new(Barrier::new(8));
        let mut attaching = FuturesUnordered::new();
        for (attach_id, stream) in negotiated {
            attaching.push(attach_through_begin(
                stream,
                attach_id,
                Arc::clone(&attach_barrier),
            ));
        }

        // All eight must publish READY inside one existing wire deadline. The
        // eighth socket stops reading immediately after BEGIN while clients
        // 1-7 continue concurrently, so a socket-local stall cannot hide
        // behind sequential attach or drain order.
        let mut clients = timeout(WIRE_RECV_TIMEOUT, async {
            let mut beginnings = Vec::with_capacity(8);
            while let Some(client) = attaching.next().await {
                beginnings.push(client);
            }
            beginnings.sort_by_key(|client| client.attach_id);
            let stalled = beginnings.pop().expect("eight BEGIN clients");
            assert_eq!(stalled.attach_id, 8);

            // BEGIN is the live-output fence. Emit while the native generation
            // is building and prove the established client is not held behind
            // capture publication.
            send_live_bytes(&mut established, LIVE_SENTINEL).await;
            receive_established_until(&mut established, LIVE_SENTINEL).await;

            let mut active_attaches = FuturesUnordered::new();
            for client in beginnings {
                active_attaches.push(finish_attach(client, Some(LIVE_SENTINEL)));
            }
            let mut ready = Vec::with_capacity(8);
            while let Some(client) = active_attaches.next().await {
                ready.push(client);
            }

            // Only after all seven active sockets have reached READY and
            // received the fenced replay do we resume the intentionally
            // stalled socket.
            ready.push(finish_attach(stalled, Some(LIVE_SENTINEL)).await);
            ready.sort_by_key(|client| client.attach_id);
            ready
        })
        .await
        .expect("eight simultaneous native owners did not reach READY within release gate");

        assert_eq!(clients.len(), 8);
        let first_prefix = &clients[0].bootstrap_records;
        let shared_terminal = clients[0].terminal_id.clone();
        let shared_cursor = clients[0].cursor.clone();
        assert!(
            !first_prefix.is_empty(),
            "full-screen TUI produced an empty native bootstrap"
        );
        for client in &clients[1..] {
            assert_eq!(
                client.terminal_id, shared_terminal,
                "client {}: simultaneous attach resolved another terminal",
                client.attach_id
            );
            assert_eq!(
                &client.bootstrap_records, first_prefix,
                "client {}: shared generation prefix differs",
                client.attach_id
            );
            assert_eq!(
                client.cursor, shared_cursor,
                "client {}: opaque cursor, not last_seq, identifies the shared generation",
                client.attach_id
            );
        }

        // Client 8 takes its own cursor lease and then stops consuming again.
        // Every active owner starts from its own copy of the shared opaque
        // cursor and independently advances page_seq from 1 through FINISH.
        let mut stalled = clients.pop().expect("stalled client");
        assert_eq!(stalled.attach_id, 8);
        let stalled_cursor = stalled.cursor.clone();
        request_page(&mut stalled, stalled_cursor).await;

        let history_barrier = Arc::new(Barrier::new(7));
        let mut histories = FuturesUnordered::new();
        for client in clients {
            histories.push(finish_history(client, Arc::clone(&history_barrier)));
        }
        let completed_histories = timeout(WIRE_RECV_TIMEOUT, async {
            let mut completed = Vec::with_capacity(7);
            while let Some(result) = histories.next().await {
                completed.push(result);
            }
            completed
        })
        .await
        .expect("seven independent owners did not page to FINISH within release gate");
        let mut active = Vec::with_capacity(7);
        let mut expected_rows = None;
        let mut expected_pages = None;
        for (client, rows, pages) in completed_histories {
            assert_eq!(
                rows,
                *expected_rows.get_or_insert(rows),
                "client {}: shared immutable history row count",
                client.attach_id
            );
            assert_eq!(
                pages,
                *expected_pages.get_or_insert(pages),
                "client {}: independent lineage reached a different FINISH",
                client.attach_id
            );
            active.push(client);
        }
        active.sort_by_key(|client| client.attach_id);
        assert_eq!(active.len(), 7);

        // A later live boundary closes the exact-once observation window for
        // the first sentinel. Client 8 remains unread with a history response
        // in its outbound pump while both the established and seven active
        // clients receive this boundary.
        send_live_bytes(&mut established, LIVE_BOUNDARY).await;
        receive_established_until(&mut established, LIVE_BOUNDARY).await;
        let mut active_boundaries = FuturesUnordered::new();
        for mut client in active {
            active_boundaries.push(async move {
                receive_until_live_marker(&mut client, LIVE_BOUNDARY).await;
                client
            });
        }
        let mut drained = Vec::with_capacity(8);
        while let Some(client) = active_boundaries.next().await {
            drained.push(client);
        }
        receive_until_live_marker(&mut stalled, LIVE_BOUNDARY).await;
        drained.push(stalled);
        drained.sort_by_key(|client| client.attach_id);

        assert_eq!(
            count_occurrences(&established.live_bytes, LIVE_SENTINEL),
            1,
            "established peer saw fenced sentinel more than once"
        );
        for client in &drained {
            assert_eq!(
                count_occurrences(&client.live_bytes, LIVE_SENTINEL),
                1,
                "client {}: sentinel must appear after READY exactly once",
                client.attach_id
            );
        }

        // Explicit DETACH acknowledgements make owner teardown observable.
        // A fresh owner must then acquire and finish the same history without
        // colliding with any abandoned cursor lineage.
        let mut detaches = FuturesUnordered::new();
        for client in drained {
            detaches.push(detach_attached(client));
        }
        let mut detached_count = 0;
        while detaches.next().await.is_some() {
            detached_count += 1;
        }
        assert_eq!(detached_count, 8);
        send_frame(&mut established.stream, &FrameKind::Detach).await;
        assert!(matches!(
            recv_until_detached(&mut established.stream).await,
            FrameKind::Detached
        ));

        let probe_stream = wait_for_native_socket(&socket).await;
        let probe = attach_through_begin(probe_stream, 9, Arc::new(Barrier::new(1))).await;
        let probe = finish_attach(probe, None).await;
        let (probe, _, _) = timeout(
            WIRE_RECV_TIMEOUT,
            finish_history(probe, Arc::new(Barrier::new(1))),
        )
        .await
        .expect("fresh owner did not reach FINISH after teardown");
        detach_attached(probe).await;

        shutdown.send(()).ok();
        timeout(SERVER_JOIN_DEADLINE, server)
            .await
            .expect("server shutdown timeout")
            .expect("server task")
            .expect("server result");
        assert!(!socket.exists(), "server leaked UDS after load gate");
    });
}
