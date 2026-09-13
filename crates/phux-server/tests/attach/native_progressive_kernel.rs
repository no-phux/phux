//! Real HELLO/ATTACH progressive-history flow through the strict native kernel.

#![cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#![allow(unsafe_code, reason = "the acceptance drives the public C ABI")]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertion-bearing integration test"
)]
#![allow(
    clippy::future_not_send,
    clippy::too_many_lines,
    reason = "each LocalSet flow owns a non-Send native kernel across the complete wire transcript"
)]

use std::mem;
use std::path::Path;
use std::ptr;
use std::slice;

use bytes::BytesMut;
use phux_client_ffi::{
    ABI_VERSION, PhuxAttachOptions, PhuxBytes, PhuxClient, PhuxClientOptions, PhuxClientResult,
    PhuxClientState, phux_client_feed_frame, phux_client_free, phux_client_last_error,
    phux_client_new, phux_client_outgoing_clear, phux_client_outgoing_count,
    phux_client_outgoing_get, phux_client_queue_attach, phux_client_queue_hello, phux_client_state,
};
use phux_protocol::caps::{BootstrapProfile, EngineCodec};
use phux_protocol::wire::frame::FrameKind;
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SERVER_JOIN_DEADLINE, SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, recv_typed, run_local,
    send_frame, spawn_server_with_seed_cmd, wait_for_raw_socket,
};

#[derive(Debug, Default)]
struct FlowMetrics {
    history_units: usize,
    authenticated_rows: usize,
    saw_finish: bool,
    saw_live_between_pages: bool,
}

fn new_client(max_materialized_rows: usize) -> *mut PhuxClient {
    let options = PhuxClientOptions {
        size: mem::size_of::<PhuxClientOptions>(),
        version: ABI_VERSION,
        max_bootstrap_chunk_bytes: 1024 * 1024,
        max_history_page_bytes: 1024 * 1024,
        max_history_page_rows: 1024,
        max_history_cache_bytes: 16 * 1024 * 1024,
        max_history_materialized_rows: max_materialized_rows,
        history_prefetch_rows: max_materialized_rows.saturating_add(1),
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
    if error.len == 0 {
        return String::new();
    }
    assert!(!error.data.is_null(), "nonempty FFI error has a pointer");
    let bytes = unsafe { slice::from_raw_parts(error.data, error.len) };
    String::from_utf8_lossy(bytes).into_owned()
}

fn feed(client: *mut PhuxClient, frame: &FrameKind) {
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    assert_eq!(
        unsafe { phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) },
        PhuxClientResult::Ok,
        "strict kernel rejected {frame:?}: {}",
        last_error(client),
    );
}

fn drain_outgoing(client: *mut PhuxClient) -> Vec<FrameKind> {
    let count = unsafe { phux_client_outgoing_count(client) };
    let mut outgoing = Vec::with_capacity(count);
    for index in 0..count {
        let mut bytes = PhuxBytes::default();
        assert_eq!(
            unsafe { phux_client_outgoing_get(client, index, &raw mut bytes) },
            PhuxClientResult::Ok,
        );
        assert!(bytes.len != 0 && !bytes.data.is_null());
        let encoded = unsafe { slice::from_raw_parts(bytes.data, bytes.len) };
        let (frame, remaining) = FrameKind::decode(encoded).expect("outgoing wire frame");
        assert!(remaining.is_empty());
        outgoing.push(frame);
    }
    assert_eq!(
        unsafe { phux_client_outgoing_clear(client) },
        PhuxClientResult::Ok,
    );
    outgoing
}

async fn flush_outgoing(client: *mut PhuxClient, stream: &mut UnixStream) {
    for frame in drain_outgoing(client) {
        send_frame(stream, &frame).await;
    }
}

async fn wait_for_marker(path: &Path) {
    timeout(WIRE_RECV_TIMEOUT, async {
        while !path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("seed history marker");
}

async fn run_flow(
    command: CommandBuilder,
    marker: Option<&Path>,
    live_trigger: Option<&Path>,
    max_materialized_rows: usize,
) -> FlowMetrics {
    let tmp = TempDir::new().expect("tempdir");
    let socket_path = tmp.path().join("phux.sock");
    let (shutdown_tx, server_handle) =
        spawn_server_with_seed_cmd(socket_path.clone(), "native-progressive", command);
    let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
    if let Some(marker) = marker {
        wait_for_marker(marker).await;
    }
    let client = new_client(max_materialized_rows);

    let name = b"native-progressive-kernel";
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
    flush_outgoing(client, &mut stream).await;
    let (_, hello) = recv_typed(&mut stream).await;
    assert!(matches!(
        hello,
        FrameKind::HelloOk {
            selected_profile: BootstrapProfile::NativeState {
                codec: EngineCodec::LibghosttySnapshotV1,
                ..
            },
            ..
        }
    ));
    feed(client, &hello);

    let attach = PhuxAttachOptions {
        size: mem::size_of::<PhuxAttachOptions>(),
        version: ABI_VERSION,
        attach_id: 1,
        target_kind: 0,
        session_id: 0,
        name: PhuxBytes::default(),
        cols: 200,
        rows: 3,
        has_pixel_size: false,
        pixel_width: 0,
        pixel_height: 0,
        request_scrollback: true,
        scrollback_limit_lines: 10_000,
    };
    assert_eq!(
        unsafe { phux_client_queue_attach(client, &raw const attach) },
        PhuxClientResult::Ok,
    );
    flush_outgoing(client, &mut stream).await;

    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    let mut metrics = FlowMetrics::default();
    while !metrics.saw_finish {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "progressive history did not reach FINISH"
        );
        let (_, frame) = timeout(remaining, recv_typed(&mut stream))
            .await
            .expect("progressive history frame");
        match &frame {
            FrameKind::HistoryPage {
                rows, next_cursor, ..
            } => {
                metrics.history_units += 1;
                metrics.authenticated_rows += *rows as usize;
                metrics.saw_finish = next_cursor.is_none();
            }
            FrameKind::HistoryRejected { reason, .. } => {
                panic!("server leaked cooperative history progress as {reason:?}")
            }
            FrameKind::ResourceOutput { .. }
                if metrics.history_units != 0 && !metrics.saw_finish =>
            {
                metrics.saw_live_between_pages = true;
            }
            _ => {}
        }
        feed(client, &frame);
        if metrics.history_units == 1
            && !metrics.saw_finish
            && let Some(trigger) = live_trigger
        {
            std::fs::write(trigger, []).expect("release live output");
            while !metrics.saw_live_between_pages {
                let (_, live_frame) = recv_typed(&mut stream).await;
                if matches!(live_frame, FrameKind::ResourceOutput { .. }) {
                    metrics.saw_live_between_pages = true;
                }
                feed(client, &live_frame);
            }
        }
        flush_outgoing(client, &mut stream).await;
    }
    assert_eq!(
        unsafe { phux_client_state(client) },
        PhuxClientState::Attached
    );

    unsafe { phux_client_free(client) };
    drop(stream);
    shutdown_tx.send(()).ok();
    timeout(SERVER_JOIN_DEADLINE, server_handle)
        .await
        .expect("server did not stop")
        .expect("server join")
        .expect("server run");
    metrics
}

#[test]
fn hello_attach_progresses_zero_and_tiny_retention_history_through_finish() {
    run_local(async {
        let empty = run_flow(CommandBuilder::new("/bin/cat"), None, None, 16).await;
        assert_eq!(empty.history_units, 1, "zero history still carries FINISH");
        assert_eq!(empty.authenticated_rows, 0);

        let marker_tmp = TempDir::new().expect("marker tempdir");
        let marker = marker_tmp.path().join("history-ready");
        let live_trigger = marker_tmp.path().join("emit-live");
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "i=0; while [ $i -lt 3000 ]; do printf 'connection-history-%04d\\r\\n' \"$i\"; i=$((i+1)); done; : > \"$1\"; while [ ! -e \"$2\" ]; do sleep 0.001; done; printf 'connection-live\\r\\n'; while :; do sleep 1; done",
            "phux-native-test",
            marker.to_str().expect("UTF-8 marker"),
            live_trigger.to_str().expect("UTF-8 live trigger"),
        ]);
        let history = run_flow(command, Some(&marker), Some(&live_trigger), 1).await;
        assert!(
            history.history_units >= 3,
            "two pages plus FINISH are required"
        );
        assert!(history.authenticated_rows > 1);
        assert!(history.saw_live_between_pages);
    });
}
