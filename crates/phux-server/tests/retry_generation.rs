//! Regression for a late-started configured seed attaching through the strict
//! native FFI kernel, then resizing while first-generation history is in flight.

#![cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(clippy::too_many_lines, reason = "one end-to-end retry transcript")]
#![allow(unsafe_code, reason = "the test intentionally drives the public C ABI")]

use std::mem;
use std::ptr;
use std::slice;

use bytes::BytesMut;
use phux_client_ffi::{
    ABI_VERSION, PhuxAttachOptions, PhuxBytes, PhuxClient, PhuxClientOptions, PhuxClientResult,
    PhuxClientState, PhuxTerminalGridView, phux_client_feed_frame, phux_client_free,
    phux_client_last_error, phux_client_new, phux_client_outgoing_clear,
    phux_client_outgoing_count, phux_client_outgoing_get, phux_client_queue_attach,
    phux_client_queue_hello, phux_client_state, phux_client_terminal_grid,
    phux_client_terminal_resize, terminal_id_out,
};
use phux_protocol::caps::{BootstrapProfile, EngineCodec};
use phux_protocol::ids::{BootstrapId, StreamId, TerminalId};
use phux_protocol::wire::frame::{FrameKind, TombstoneReason};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, recv_typed, run_local,
    send_frame, spawn_server_with_seed_cmd, wait_for_raw_socket,
};

const SESSION: &str = "configured-seed";
const MARKER: &[u8] = b"retry-generation";

fn new_client() -> *mut PhuxClient {
    let options = PhuxClientOptions {
        size: mem::size_of::<PhuxClientOptions>(),
        version: ABI_VERSION,
        max_bootstrap_chunk_bytes: 1024 * 1024,
        max_history_page_bytes: 1024 * 1024,
        max_history_page_rows: 4096,
        max_history_cache_bytes: 8 * 1024 * 1024,
        max_history_materialized_rows: 50_000,
        history_prefetch_rows: 256,
    };
    let mut client = ptr::null_mut();
    assert_eq!(
        unsafe { phux_client_new(&raw const options, &raw mut client) },
        PhuxClientResult::Ok,
    );
    assert!(!client.is_null());
    client
}

fn last_error(client: *const PhuxClient) -> String {
    let mut error = PhuxBytes::default();
    assert_eq!(
        unsafe { phux_client_last_error(client, &raw mut error) },
        PhuxClientResult::Ok,
    );
    let bytes = unsafe { slice::from_raw_parts(error.data, error.len) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn feed(client: *mut PhuxClient, frame: &FrameKind) -> PhuxClientResult {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    unsafe { phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) }
}

fn assert_feed_ok(client: *mut PhuxClient, frame: &FrameKind) {
    let result = feed(client, frame);
    assert_eq!(
        result,
        PhuxClientResult::Ok,
        "strict FFI kernel rejected {frame:?}: {}",
        last_error(client),
    );
}

fn drain_outgoing(client: *mut PhuxClient) -> Vec<FrameKind> {
    let count = unsafe { phux_client_outgoing_count(client) };
    let mut outgoing = Vec::with_capacity(count);
    for index in 0..count {
        let mut frame = PhuxBytes::default();
        assert_eq!(
            unsafe { phux_client_outgoing_get(client, index, &raw mut frame) },
            PhuxClientResult::Ok,
        );
        let encoded = unsafe { slice::from_raw_parts(frame.data, frame.len) };
        let (frame, remaining) = FrameKind::decode(encoded).expect("FFI outgoing frame decodes");
        assert!(remaining.is_empty(), "FFI emitted trailing frame bytes");
        outgoing.push(frame);
    }
    assert_eq!(
        unsafe { phux_client_outgoing_clear(client) },
        PhuxClientResult::Ok,
    );
    outgoing
}

async fn flush_outgoing(frames: Vec<FrameKind>, stream: &mut UnixStream) {
    for frame in frames {
        send_frame(stream, &frame).await;
    }
}

fn focused_terminal(frame: &FrameKind) -> Option<TerminalId> {
    let FrameKind::Attached { snapshot, .. } = frame else {
        return None;
    };
    let focused_windows: Vec<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == snapshot.focused_session)
        .map(|window| window.id)
        .collect();
    snapshot
        .panes
        .iter()
        .find(|pane| focused_windows.contains(&pane.window_id))
        .map(|pane| pane.id.clone())
}

fn current_grid_contains(
    client: *mut PhuxClient,
    terminal_id: &TerminalId,
    marker: &[u8],
) -> Option<PhuxTerminalGridView> {
    let terminal = terminal_id_out(terminal_id);
    let mut view = PhuxTerminalGridView::default();
    if unsafe { phux_client_terminal_grid(client, &raw const terminal, &raw mut view) }
        != PhuxClientResult::Ok
    {
        return None;
    }
    if view.utf8.len == 0 {
        return None;
    }
    let text = unsafe { slice::from_raw_parts(view.utf8.data, view.utf8.len) };
    text.windows(marker.len())
        .any(|window| window == marker)
        .then_some(view)
}

#[test]
fn late_server_retry_keeps_the_fresh_seed_on_its_current_generation() {
    run_local(async {
        let tmp = TempDir::new().expect("tempdir");
        let socket_path = tmp.path().join("phux.sock");

        let absent = UnixStream::connect(&socket_path).await;
        assert!(
            absent.is_err(),
            "the first Retry attempt must observe an absent server socket",
        );

        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "while :; do printf 'retry-generation\\r\\n'; sleep 0.05; done",
        ]);
        let (shutdown_tx, server_handle) =
            spawn_server_with_seed_cmd(socket_path.clone(), SESSION, command);
        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        let client = new_client();

        let name = b"cockpit-retry-regression";
        assert_eq!(
            unsafe {
                phux_client_queue_hello(
                    client,
                    PhuxBytes {
                        data: name.as_ptr(),
                        len: name.len(),
                    },
                )
            },
            PhuxClientResult::Ok,
        );
        flush_outgoing(drain_outgoing(client), &mut stream).await;
        let (_, hello_ok) = recv_typed(&mut stream).await;
        assert!(
            matches!(
                &hello_ok,
                FrameKind::HelloOk {
                    selected_profile: BootstrapProfile::NativeState {
                        codec: EngineCodec::LibghosttyCheckpointV2,
                        ..
                    },
                    ..
                }
            ),
            "retry regression requires the native history path; got {hello_ok:?}",
        );
        assert_feed_ok(client, &hello_ok);

        let attach = PhuxAttachOptions {
            size: mem::size_of::<PhuxAttachOptions>(),
            version: ABI_VERSION,
            attach_id: 1,
            target_kind: 0,
            session_id: 0,
            name: PhuxBytes::default(),
            cols: 100,
            rows: 30,
            has_pixel_size: false,
            pixel_width: 0,
            pixel_height: 0,
            request_scrollback: true,
            scrollback_limit_lines: 50_000,
        };
        assert_eq!(
            unsafe { phux_client_queue_attach(client, &raw const attach) },
            PhuxClientResult::Ok,
        );
        flush_outgoing(drain_outgoing(client), &mut stream).await;

        let mut terminal_id = None;
        let mut initial_generation: Option<(StreamId, BootstrapId)> = None;
        loop {
            let (_, frame) = recv_typed(&mut stream).await;
            terminal_id = terminal_id.or_else(|| focused_terminal(&frame));
            if let FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                ..
            } = &frame
                && initial_generation.is_none()
            {
                initial_generation = Some((*stream_id, *bootstrap_id));
            }
            let ready = matches!(frame, FrameKind::AttachReady { attach_id: 1 });
            assert_feed_ok(client, &frame);
            flush_outgoing(drain_outgoing(client), &mut stream).await;
            if ready {
                break;
            }
        }
        assert_eq!(
            unsafe { phux_client_state(client) },
            PhuxClientState::Attached,
        );
        assert_eq!(
            initial_generation,
            Some((
                StreamId::new(2).expect("attach 1 stream"),
                BootstrapId::new(1).expect("initial bootstrap"),
            )),
            "ATTACH 1 must publish the reported (StreamId(2), BootstrapId(1)) generation",
        );
        let initial_generation = initial_generation.expect("initial generation");

        let terminal_id = terminal_id.expect("ATTACHED names the configured seed pane");
        assert_eq!(
            terminal_id,
            TerminalId::local(1),
            "fresh configured seed must be TerminalId(1)",
        );
        let terminal = terminal_id_out(&terminal_id);
        assert_eq!(
            unsafe { phux_client_terminal_resize(client, &raw const terminal, 120, 40) },
            PhuxClientResult::Ok,
        );
        flush_outgoing(drain_outgoing(client), &mut stream).await;

        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let mut saw_initial_tombstone = false;
        let mut saw_replacement_begin = false;
        let current = loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            assert!(
                !remaining.is_zero(),
                "current replacement grid did not publish"
            );
            let (_, frame) = timeout(remaining, recv_typed(&mut stream))
                .await
                .expect("timed out waiting for replacement generation");
            match &frame {
                FrameKind::BootstrapTombstone {
                    terminal_id: retired_terminal,
                    stream_id,
                    bootstrap_id,
                    reason,
                    ..
                } if retired_terminal == &terminal_id
                    && (*stream_id, *bootstrap_id) == initial_generation =>
                {
                    assert_eq!(*reason, TombstoneReason::Resize);
                    saw_initial_tombstone = true;
                }
                FrameKind::BootstrapBegin {
                    terminal_id: replacement_terminal,
                    stream_id,
                    bootstrap_id,
                    ..
                } if replacement_terminal == &terminal_id
                    && (*stream_id, *bootstrap_id) != initial_generation =>
                {
                    assert!(
                        saw_initial_tombstone,
                        "replacement BEGIN must follow the initial generation tombstone",
                    );
                    saw_replacement_begin = true;
                }
                _ => {}
            }
            assert_feed_ok(client, &frame);
            flush_outgoing(drain_outgoing(client), &mut stream).await;
            if let Some(view) = current_grid_contains(client, &terminal_id, MARKER)
                && saw_initial_tombstone
                && saw_replacement_begin
                && view.cols == 120
                && view.rows == 40
                && view.bootstrap_id != initial_generation.1.get()
            {
                break view;
            }
        };
        assert!(current.document_revision > 0);
        assert_eq!(
            unsafe { phux_client_state(client) },
            PhuxClientState::Attached,
        );

        let (stream_id, bootstrap_id) = initial_generation;
        let stale = FrameKind::TerminalOutput {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            seq: 1,
            bytes: b"genuinely stale".as_slice().into(),
        };
        assert_eq!(feed(client, &stale), PhuxClientResult::InvalidState);
        assert_eq!(
            last_error(client),
            "generation (StreamId(2), BootstrapId(1)) is retired for TerminalId(1)",
            "strict stale-frame rejection must remain visible",
        );
        assert_eq!(
            unsafe { phux_client_state(client) },
            PhuxClientState::Attached,
        );

        unsafe { phux_client_free(client) };
        drop(stream);
        shutdown_tx.send(()).ok();
        timeout(SERVER_JOIN_DEADLINE, server_handle)
            .await
            .expect("server did not stop")
            .expect("server join")
            .expect("server run");
    });
}
