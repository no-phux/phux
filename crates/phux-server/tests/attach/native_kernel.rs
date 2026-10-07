//! The production server driven through the strict native session kernel's
//! public C ABI (the path Cockpit uses).

#![cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
#![allow(unsafe_code, reason = "the tests drive the public C ABI")]
#![allow(clippy::too_many_lines, reason = "one wire transcript per test")]

use std::mem;
use std::path::Path;
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
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{FrameKind, TombstoneReason};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, WIRE_RECV_TIMEOUT, join_after_shutdown, recv_typed, recv_typed_before,
    run_local, send_frame, spawn_server_with_seed_cmd, wait_for_raw_socket,
};

use super::common::sh;

/// An owned FFI kernel.
struct Kernel(*mut PhuxClient);

impl Drop for Kernel {
    fn drop(&mut self) {
        unsafe { phux_client_free(self.0) };
    }
}

impl Kernel {
    fn new(max_page_rows: u32, max_materialized_rows: usize, prefetch_rows: usize) -> Self {
        let options = PhuxClientOptions {
            size: mem::size_of::<PhuxClientOptions>(),
            version: ABI_VERSION,
            max_bootstrap_chunk_bytes: 1024 * 1024,
            max_history_page_bytes: 1024 * 1024,
            max_history_page_rows: max_page_rows,
            max_history_cache_bytes: 16 * 1024 * 1024,
            max_history_materialized_rows: max_materialized_rows,
            history_prefetch_rows: prefetch_rows,
        };
        let mut client = ptr::null_mut();
        assert_eq!(
            unsafe { phux_client_new(&raw const options, &raw mut client) },
            PhuxClientResult::Ok,
        );
        assert!(!client.is_null());
        Self(client)
    }

    fn last_error(&self) -> String {
        let mut error = PhuxBytes::default();
        assert_eq!(
            unsafe { phux_client_last_error(self.0, &raw mut error) },
            PhuxClientResult::Ok,
        );
        if error.len == 0 {
            return String::new();
        }
        let bytes = unsafe { slice::from_raw_parts(error.data, error.len) };
        String::from_utf8_lossy(bytes).into_owned()
    }

    fn feed(&self, frame: &FrameKind) -> PhuxClientResult {
        let mut encoded = BytesMut::new();
        frame.encode(&mut encoded);
        unsafe { phux_client_feed_frame(self.0, encoded.as_ptr(), encoded.len()) }
    }

    fn feed_ok(&self, frame: &FrameKind) {
        let result = self.feed(frame);
        assert_eq!(
            result,
            PhuxClientResult::Ok,
            "strict kernel rejected {frame:?}: {}",
            self.last_error()
        );
    }

    fn state(&self) -> PhuxClientState {
        unsafe { phux_client_state(self.0) }
    }

    /// Send every queued outgoing frame to the server.
    async fn flush(&self, stream: &mut UnixStream) {
        let count = unsafe { phux_client_outgoing_count(self.0) };
        for index in 0..count {
            let mut bytes = PhuxBytes::default();
            assert_eq!(
                unsafe { phux_client_outgoing_get(self.0, index, &raw mut bytes) },
                PhuxClientResult::Ok,
            );
            let encoded = unsafe { slice::from_raw_parts(bytes.data, bytes.len) };
            let (frame, rest) = FrameKind::decode(encoded).expect("outgoing frame decodes");
            assert!(rest.is_empty());
            send_frame(stream, &frame).await;
        }
        assert_eq!(
            unsafe { phux_client_outgoing_clear(self.0) },
            PhuxClientResult::Ok
        );
    }

    /// HELLO (asserting the native profile), then queue an ATTACH with history.
    async fn hello_and_attach(&self, stream: &mut UnixStream, cols: u16, rows: u16, limit: u32) {
        let name = b"native-kernel-test";
        let name = PhuxBytes {
            data: name.as_ptr(),
            len: name.len(),
        };
        assert_eq!(
            unsafe { phux_client_queue_hello(self.0, name) },
            PhuxClientResult::Ok
        );
        self.flush(stream).await;
        let (_, hello) = recv_typed(stream).await;
        assert!(
            matches!(
                hello,
                FrameKind::HelloOk {
                    selected_profile: BootstrapProfile::NativeState {
                        codec: EngineCodec::LibghosttySnapshotV1,
                        ..
                    },
                    ..
                }
            ),
            "native history path required; got {hello:?}"
        );
        self.feed_ok(&hello);
        let attach = PhuxAttachOptions {
            size: mem::size_of::<PhuxAttachOptions>(),
            version: ABI_VERSION,
            attach_id: 1,
            target_kind: 0,
            session_id: 0,
            name: PhuxBytes::default(),
            cols,
            rows,
            has_pixel_size: false,
            pixel_width: 0,
            pixel_height: 0,
            request_scrollback: true,
            scrollback_limit_lines: limit,
        };
        assert_eq!(
            unsafe { phux_client_queue_attach(self.0, &raw const attach) },
            PhuxClientResult::Ok,
        );
        self.flush(stream).await;
    }

    /// The published grid of `terminal`, if it contains `marker`.
    fn grid_containing(
        &self,
        terminal: &ResourceId,
        marker: &[u8],
    ) -> Option<PhuxTerminalGridView> {
        let terminal = terminal_id_out(terminal);
        let mut view = PhuxTerminalGridView::default();
        let ok = unsafe { phux_client_terminal_grid(self.0, &raw const terminal, &raw mut view) };
        if ok != PhuxClientResult::Ok || view.utf8.len == 0 {
            return None;
        }
        let text = unsafe { slice::from_raw_parts(view.utf8.data, view.utf8.len) };
        text.windows(marker.len())
            .any(|w| w == marker)
            .then_some(view)
    }
}

#[derive(Debug, Default)]
struct FlowMetrics {
    history_units: usize,
    authenticated_rows: usize,
    saw_finish: bool,
    saw_live_between_pages: bool,
}

/// Attach at 200x3 and page history to FINISH, optionally releasing live
/// output after the first page so it must interleave between pages.
async fn run_flow(
    command: CommandBuilder,
    marker: Option<&Path>,
    live_trigger: Option<&Path>,
    max_materialized_rows: usize,
) -> FlowMetrics {
    let tmp = TempDir::new().unwrap();
    let socket = tmp.path().join("phux.sock");
    let (shutdown, server) =
        spawn_server_with_seed_cmd(socket.clone(), "native-progressive", command);
    let mut stream = wait_for_raw_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
    if let Some(marker) = marker {
        timeout(WIRE_RECV_TIMEOUT, async {
            while !marker.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("seed history marker");
    }
    let kernel = Kernel::new(
        1024,
        max_materialized_rows,
        max_materialized_rows.saturating_add(1),
    );
    kernel.hello_and_attach(&mut stream, 200, 3, 10_000).await;

    let mut metrics = FlowMetrics::default();
    let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
    while !metrics.saw_finish {
        let (_, frame) = recv_typed_before(&mut stream, deadline)
            .await
            .expect("progressive history did not reach FINISH");
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
        kernel.feed_ok(&frame);
        if metrics.history_units == 1
            && !metrics.saw_finish
            && let Some(trigger) = live_trigger
        {
            std::fs::write(trigger, []).expect("release live output");
            while !metrics.saw_live_between_pages {
                let (_, live) = recv_typed(&mut stream).await;
                metrics.saw_live_between_pages = matches!(live, FrameKind::ResourceOutput { .. });
                kernel.feed_ok(&live);
            }
        }
        kernel.flush(&mut stream).await;
    }
    assert_eq!(kernel.state(), PhuxClientState::Attached);
    drop((kernel, stream));
    join_after_shutdown(shutdown, server).await;
    metrics
}

#[test]
fn hello_attach_progresses_zero_and_tiny_retention_history_through_finish() {
    run_local(async {
        let empty = run_flow(CommandBuilder::new("/bin/cat"), None, None, 16).await;
        assert_eq!(empty.history_units, 1, "zero history still carries FINISH");
        assert_eq!(empty.authenticated_rows, 0);

        let tmp = TempDir::new().unwrap();
        let marker = tmp.path().join("history-ready");
        let live = tmp.path().join("emit-live");
        let script = format!(
            "i=0; while [ $i -lt 3000 ]; do printf 'connection-history-%04d\\r\\n' \"$i\"; i=$((i+1)); done; \
             : > '{}'; while [ ! -e '{}' ]; do sleep 0.001; done; printf 'connection-live\\r\\n'; \
             while :; do sleep 1; done",
            marker.display(),
            live.display()
        );
        let history = run_flow(sh(&script), Some(&marker), Some(&live), 1).await;
        assert!(history.history_units >= 3, "two pages plus FINISH");
        assert!(history.authenticated_rows > 1);
        assert!(history.saw_live_between_pages);
    });
}

/// A configured seed that starts only after the client's first connect
/// attempt, resized while its first generation's history is in flight:
/// the kernel lands on the replacement generation and still rejects frames
/// from the retired one.
#[test]
fn late_server_retry_keeps_the_fresh_seed_on_its_current_generation() {
    const MARKER: &[u8] = b"retry-generation";
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        assert!(
            UnixStream::connect(&socket).await.is_err(),
            "first attempt sees no server"
        );
        let seed = sh("while :; do printf 'retry-generation\\r\\n'; sleep 0.05; done");
        let (shutdown, server) =
            spawn_server_with_seed_cmd(socket.clone(), "configured-seed", seed);
        let mut stream = wait_for_raw_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let kernel = Kernel::new(4096, 50_000, 256);
        kernel.hello_and_attach(&mut stream, 100, 30, 50_000).await;

        let mut initial = None;
        loop {
            let (_, frame) = recv_typed(&mut stream).await;
            if let FrameKind::BootstrapBegin {
                stream_id,
                bootstrap_id,
                ..
            } = &frame
            {
                initial.get_or_insert((*stream_id, *bootstrap_id));
            }
            let ready = matches!(frame, FrameKind::AttachReady { attach_id: 1 });
            kernel.feed_ok(&frame);
            kernel.flush(&mut stream).await;
            if ready {
                break;
            }
        }
        let initial = initial.expect("initial generation");
        assert_eq!(
            initial,
            (StreamId::new(2).unwrap(), BootstrapId::new(1).unwrap()),
            "ATTACH 1 publishes (StreamId(2), BootstrapId(1))"
        );
        let terminal_id = ResourceId::local(1);

        let terminal = terminal_id_out(&terminal_id);
        assert_eq!(
            unsafe { phux_client_terminal_resize(kernel.0, &raw const terminal, 120, 40) },
            PhuxClientResult::Ok,
        );
        kernel.flush(&mut stream).await;

        let deadline = tokio::time::Instant::now() + WIRE_RECV_TIMEOUT;
        let mut saw_tombstone = false;
        let mut saw_replacement = false;
        let current = loop {
            let (_, frame) = recv_typed_before(&mut stream, deadline)
                .await
                .expect("replacement generation never published");
            match &frame {
                FrameKind::BootstrapTombstone {
                    terminal_id: retired,
                    stream_id,
                    bootstrap_id,
                    reason,
                    ..
                } if *retired == terminal_id && (*stream_id, *bootstrap_id) == initial => {
                    assert_eq!(*reason, TombstoneReason::Resize);
                    saw_tombstone = true;
                }
                FrameKind::BootstrapBegin {
                    terminal_id: replacement,
                    stream_id,
                    bootstrap_id,
                    ..
                } if *replacement == terminal_id && (*stream_id, *bootstrap_id) != initial => {
                    assert!(saw_tombstone, "replacement BEGIN follows the tombstone");
                    saw_replacement = true;
                }
                _ => {}
            }
            kernel.feed_ok(&frame);
            kernel.flush(&mut stream).await;
            if let Some(view) = kernel.grid_containing(&terminal_id, MARKER)
                && saw_replacement
                && (view.cols, view.rows) == (120, 40)
                && view.bootstrap_id != initial.1.get()
            {
                break view;
            }
        };
        assert!(current.document_revision > 0);
        assert_eq!(kernel.state(), PhuxClientState::Attached);

        let stale = FrameKind::ResourceOutput {
            terminal_id,
            stream_id: initial.0,
            bootstrap_id: initial.1,
            seq: 1,
            bytes: b"genuinely stale".as_slice().into(),
        };
        assert_eq!(kernel.feed(&stale), PhuxClientResult::InvalidState);
        assert_eq!(
            kernel.last_error(),
            "generation (StreamId(2), BootstrapId(1)) is retired for @1",
        );
        assert_eq!(kernel.state(), PhuxClientState::Attached);

        drop((kernel, stream));
        join_after_shutdown(shutdown, server).await;
    });
}
