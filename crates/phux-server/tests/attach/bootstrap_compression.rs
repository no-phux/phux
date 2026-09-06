//! `docs/spec/proto.md` §6.4 — negotiated frame compression, end to end.
//!
//! Two attaches against one real server over its real socket: one offering
//! DEFLATE and one offering nothing. Both must reach the same decoded
//! bootstrap, and the offering one must put dramatically fewer bytes on the
//! wire — that byte count is the metric the whole feature exists to move,
//! since a remote first paint is bandwidth-bound on the native prefix.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
// The measured numbers are the point of this gate: `--no-capture` prints them,
// and a ratio computed in floating point is precise enough to read.
#![allow(clippy::print_stderr, clippy::cast_precision_loss)]

use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, Compression, CompressionSet, LayerSet};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use portable_pty::CommandBuilder;
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, recv_framed, run_local, send_frame, spawn_server_with_seed_cmd,
    wait_for_raw_socket,
};

/// A pane wide and tall enough that libghostty's capture emits a real page.
const COLS: u16 = 200;
const ROWS: u16 = 50;

/// What one attach put on the wire and what it decoded to.
struct AttachTrace {
    /// Bytes read off the socket from `ATTACH` to `BOOTSTRAP_READY`, inclusive
    /// of every frame header — the number a remote link actually pays.
    wire_bytes: usize,
    /// Concatenated `BOOTSTRAP_CHUNK` payloads, in order.
    prefix: Vec<u8>,
    /// Whether any frame arrived inside a `FRAME_COMPRESSED` envelope.
    saw_envelope: bool,
}

async fn hello(stream: &mut UnixStream, compression: CompressionSet) -> Compression {
    send_frame(
        stream,
        &FrameKind::Hello {
            client_name: "compression-gate".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_layers(LayerSet::all())
                .with_compression(compression),
        },
    )
    .await;
    let framed = recv_framed(stream).await;
    match FrameKind::decode(&framed).expect("HELLO_OK decodes").0 {
        FrameKind::HelloOk { server_caps, .. } => server_caps.compression,
        other => panic!("expected HELLO_OK, got {other:?}"),
    }
}

/// Attach and read to `BOOTSTRAP_READY`, accounting every byte.
async fn trace_attach(stream: &mut UnixStream) -> AttachTrace {
    send_frame(
        stream,
        &FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::ByName("bench".to_owned()),
            viewport: ViewportInfo::new(COLS, ROWS),
            request_scrollback: false,
            scrollback_limit_lines: 0,
        },
    )
    .await;

    let mut trace = AttachTrace {
        wire_bytes: 0,
        prefix: Vec::new(),
        saw_envelope: false,
    };
    for _ in 0..512 {
        let framed = recv_framed(stream).await;
        trace.wire_bytes += framed.len();
        // The envelope is invisible to `decode`, so the type byte on the wire
        // is the only place the transform shows up at all.
        if framed[4] == phux_protocol::wire::frame::TYPE_FRAME_COMPRESSED {
            trace.saw_envelope = true;
        }
        let (frame, _) = FrameKind::decode(&framed).expect("frame decodes");
        match frame {
            FrameKind::BootstrapChunk { payload, .. } => trace.prefix.extend_from_slice(&payload),
            FrameKind::BootstrapReady { .. } => return trace,
            _ => {}
        }
    }
    panic!("never reached BOOTSTRAP_READY");
}

/// The gate. A client that offers DEFLATE gets a byte-identical bootstrap
/// prefix for a fraction of the wire bytes; a client that offers nothing gets
/// exactly the bytes it always got.
#[test]
fn compression_shrinks_the_bootstrap_without_changing_it() {
    let dir = TempDir::new().expect("tempdir");
    let socket = dir.path().join("phux.sock");
    // A pane with enough scrolled output that the capture is a real page
    // rather than a nearly-empty one.
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", "sleep 0.4; seq 1 400; sleep 60"]);

    run_local(async move {
        let (shutdown, handle) = spawn_server_with_seed_cmd(socket.clone(), "bench", cmd);

        // A first attach at the target geometry, discarded: the seed pane is
        // spawned at the server's default size and it is the attach viewport
        // that resizes it, so measuring the very first bootstrap would measure
        // an 80-column pane. Everything after this sees the real 200x50 grid.
        {
            let mut warmup = wait_for_raw_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
            hello(&mut warmup, CompressionSet::new()).await;
            tokio::time::sleep(Duration::from_millis(600)).await;
            let _ = trace_attach(&mut warmup).await;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;

        let mut plain_stream = wait_for_raw_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        assert_eq!(
            hello(&mut plain_stream, CompressionSet::new()).await,
            Compression::None,
            "a client that offers nothing must be told nothing was selected",
        );
        let plain = trace_attach(&mut plain_stream).await;

        let mut deflate_stream = wait_for_raw_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        assert_eq!(
            hello(&mut deflate_stream, CompressionSet::all()).await,
            Compression::Deflate,
            "an offer of DEFLATE must come back selected",
        );
        let deflated = trace_attach(&mut deflate_stream).await;

        assert!(
            !plain.saw_envelope,
            "a client that offered nothing must never receive an envelope",
        );
        assert!(
            deflated.saw_envelope,
            "a {}-byte bootstrap must have been worth wrapping",
            plain.wire_bytes,
        );
        assert_eq!(
            deflated.prefix, plain.prefix,
            "the engine's opaque bytes must survive the transform exactly \
             (docs/spec/proto.md §6.2)",
        );
        assert!(
            !plain.prefix.is_empty(),
            "the fixture must produce a real native prefix",
        );
        assert!(
            deflated.wire_bytes * 3 < plain.wire_bytes,
            "expected a large reduction on the wire, got {} from {}",
            deflated.wire_bytes,
            plain.wire_bytes,
        );
        eprintln!(
            "first-paint wire bytes: plain={} deflated={} ({:.1}x), prefix={} bytes",
            plain.wire_bytes,
            deflated.wire_bytes,
            plain.wire_bytes as f64 / deflated.wire_bytes as f64,
            plain.prefix.len(),
        );

        drop(plain_stream);
        drop(deflate_stream);
        let _ = shutdown.send(());
        let _ = handle.await;
    });
}
