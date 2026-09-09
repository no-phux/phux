//! Generate canonical Cockpit integration fixtures and validate them through the C ABI.
//!
//! Run from the repository root:
//! `cargo run --locked -p phux-client-ffi --example cockpit_fixture --profile ffi-dev`
//! An optional directory argument overrides `clients/cockpit/src/tests/fixtures`.

#![allow(
    clippy::expect_used,
    clippy::print_stdout,
    reason = "standalone fixture generator uses test-grade assertions and reports its output"
)]

use std::{error::Error, mem::size_of, path::PathBuf, ptr, slice};

use bytes::{Bytes, BytesMut};
use phux_client_ffi::{
    ABI_VERSION, PhuxAttachOptions, PhuxBytes, PhuxClient, PhuxClientOptions, PhuxClientResult,
    PhuxClientState, PhuxKeyEvent, PhuxTerminalGridView, PhuxTerminalId,
    phux_client_anchor_release, phux_client_feed_frame, phux_client_free, phux_client_new,
    phux_client_outgoing_clear, phux_client_outgoing_count, phux_client_outgoing_get,
    phux_client_queue_attach, phux_client_queue_hello, phux_client_send_focus,
    phux_client_send_key, phux_client_send_paste, phux_client_state, phux_client_terminal_grid,
    phux_client_terminal_resize,
};
use phux_protocol::caps::ServerCapabilities;
use phux_protocol::input::{
    focus::FocusEvent,
    key::{KeyAction, PhysicalKey},
};
use phux_protocol::wire::frame::{AttachTarget, FrameKind};
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot, TerminalInfo, WindowInfo};
use phux_protocol::{
    BootstrapId, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientId,
    PROTOCOL_VERSION, ServerFeature, ServerFeatureSet, SessionId, StreamId, TerminalId, WindowId,
};

const MARKER: &[u8] = b"COCKPIT FIXTURE";
// Clear/home, printable marker, bracketed paste, focus reporting, Kitty disambiguation.
const VT: &[u8] = b"\x1b[2J\x1b[HCOCKPIT FIXTURE\x1b[?2004h\x1b[?1004h\x1b[>1u";

fn hello() -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new()
            .with_features(ServerFeatureSet::with(&[ServerFeature::TerminalReply])),
        server_id: b"cockpit-fixture".to_vec(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::new(1024, 1024).expect("valid limits"),
    }
}

fn attached() -> [FrameKind; 5] {
    let terminal_id = TerminalId::local(7);
    let session_id = SessionId::new(1);
    let window_id = WindowId::new(1);
    let stream_id = StreamId::new(7).expect("nonzero stream");
    let bootstrap_id = BootstrapId::new(1).expect("nonzero bootstrap");
    let snapshot = SessionSnapshot::new(session_id, window_id, terminal_id.clone())
        .with_sessions(vec![SessionInfo::new(session_id, "fixture")])
        .with_windows(vec![WindowInfo::new(window_id, session_id, "fixture")])
        .with_panes(vec![TerminalInfo::new(
            terminal_id.clone(),
            window_id,
            80,
            24,
        )]);
    [
        FrameKind::Attached {
            attach_id: 1,
            snapshot,
            initial_client_id: ClientId::new(1),
        },
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: Bytes::from_static(VT),
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::AttachReady { attach_id: 1 },
    ]
}

fn encode(frames: &[FrameKind]) -> BytesMut {
    let mut bytes = BytesMut::new();
    for frame in frames {
        frame.encode(&mut bytes);
    }
    bytes
}

const fn span(bytes: &[u8]) -> PhuxBytes {
    PhuxBytes {
        data: bytes.as_ptr(),
        len: bytes.len(),
    }
}

/// Own the handle so assertions also release the native engine on unwinding.
struct Client(*mut PhuxClient);

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: this handle is uniquely owned and used only on the creating thread.
        unsafe { phux_client_free(self.0) };
    }
}

impl Client {
    fn new() -> Self {
        let options = PhuxClientOptions {
            size: size_of::<PhuxClientOptions>(),
            version: ABI_VERSION,
            max_bootstrap_chunk_bytes: 1024,
            max_history_page_bytes: 1024,
            max_history_page_rows: 128,
            max_history_cache_bytes: 4096,
            max_history_materialized_rows: 1024,
            history_prefetch_rows: 64,
        };
        let mut client = ptr::null_mut();
        // SAFETY: options and the independent output pointer live for the call.
        assert_eq!(
            unsafe { phux_client_new(&raw const options, &raw mut client) },
            PhuxClientResult::Ok
        );
        assert!(!client.is_null());
        Self(client)
    }

    fn feed(&self, mut bytes: &[u8]) {
        // The C ABI takes one frame; split concatenated wire data with the Rust decoder.
        while !bytes.is_empty() {
            let (_, rest) = FrameKind::decode(bytes).expect("canonical fixture frame");
            let len = bytes.len() - rest.len();
            assert!(len > 0);
            // SAFETY: live same-thread handle and a valid complete frame span.
            assert_eq!(
                unsafe { phux_client_feed_frame(self.0, bytes.as_ptr(), len) },
                PhuxClientResult::Ok
            );
            bytes = rest;
        }
    }

    fn take_outgoing(&self) -> FrameKind {
        let mut bytes = PhuxBytes::default();
        // SAFETY: live same-thread handle and writable output. Copy/decode the
        // borrowed frame before clearing the queue, which invalidates the borrow.
        unsafe {
            assert_eq!(phux_client_outgoing_count(self.0), 1);
            assert_eq!(
                phux_client_outgoing_get(self.0, 0, &raw mut bytes),
                PhuxClientResult::Ok
            );
            let (frame, rest) = FrameKind::decode(slice::from_raw_parts(bytes.data, bytes.len))
                .expect("FFI emits a canonical frame");
            assert!(rest.is_empty());
            assert_eq!(phux_client_outgoing_clear(self.0), PhuxClientResult::Ok);
            frame
        }
    }

    fn negotiate_and_attach(&self, hello: &[u8], attached: &[u8]) {
        let options = PhuxAttachOptions {
            size: size_of::<PhuxAttachOptions>(),
            version: ABI_VERSION,
            attach_id: 1,
            target_kind: 2,
            session_id: 1,
            name: PhuxBytes::default(),
            cols: 80,
            rows: 24,
            has_pixel_size: false,
            pixel_width: 0,
            pixel_height: 0,
            request_scrollback: false,
            scrollback_limit_lines: 0,
        };
        // SAFETY: handle stays live on this thread, with valid input spans and options.
        unsafe {
            assert_eq!(
                phux_client_queue_hello(self.0, span(b"cockpit-fixture")),
                PhuxClientResult::Ok
            );
            assert!(matches!(self.take_outgoing(), FrameKind::Hello { .. }));
            self.feed(hello);
            assert_eq!(phux_client_state(self.0), PhuxClientState::Negotiated);
            assert_eq!(
                phux_client_queue_attach(self.0, &raw const options),
                PhuxClientResult::Ok
            );
            assert!(matches!(self.take_outgoing(), FrameKind::Attach {
                attach_id: 1, target: AttachTarget::ById(id), ..
            } if id == SessionId::new(1)));
            self.feed(attached);
            assert_eq!(phux_client_state(self.0), PhuxClientState::Attached);
            assert_eq!(phux_client_outgoing_count(self.0), 0);
        }
    }

    fn verify_grid(&self, terminal: &PhuxTerminalId) {
        let mut grid = PhuxTerminalGridView::default();
        // SAFETY: handle, terminal and output are valid; read borrowed cells/text
        // before releasing the anchor or making any mutating call.
        unsafe {
            assert_eq!(
                phux_client_terminal_grid(self.0, terminal, &raw mut grid),
                PhuxClientResult::Ok
            );
            assert_eq!((grid.cols, grid.rows), (80, 24));
            assert_eq!(
                (grid.stream_id, grid.bootstrap_id, grid.last_seq),
                (7, 1, 0)
            );
            assert_eq!(grid.cell_count, 80 * 24);
            let text = slice::from_raw_parts(grid.utf8.data, grid.utf8.len);
            let cells = slice::from_raw_parts(grid.cells, grid.cell_count);
            let rendered: Vec<u8> = cells[..MARKER.len()]
                .iter()
                .flat_map(|cell| {
                    let start = cell.utf8_offset as usize;
                    text[start..start + usize::from(cell.utf8_len)]
                        .iter()
                        .copied()
                })
                .collect();
            assert_eq!(rendered, MARKER);
            if grid.top_anchor.opaque_id != 0 {
                assert_eq!(
                    phux_client_anchor_release(self.0, terminal, grid.top_anchor),
                    PhuxClientResult::Ok
                );
            }
        }
    }

    fn verify_input(&self, terminal: &PhuxTerminalId) {
        let key = PhuxKeyEvent {
            size: size_of::<PhuxKeyEvent>(),
            version: ABI_VERSION,
            action: KeyAction::Press.to_u32(),
            key: PhysicalKey::A as u32,
            modifiers: 0,
            consumed_modifiers: 0,
            composing: false,
            has_text: true,
            text: span(b"a"),
            has_unshifted_codepoint: true,
            unshifted_codepoint: u32::from('a'),
        };
        // SAFETY: all input pointers/spans are valid throughout each same-thread call.
        unsafe {
            assert_eq!(
                phux_client_send_key(self.0, terminal, &raw const key),
                PhuxClientResult::Ok
            );
            assert!(
                matches!(self.take_outgoing(), FrameKind::InputKey { terminal_id, event }
                if terminal_id == TerminalId::local(7) && event.text.as_deref() == Some("a"))
            );
            assert_eq!(
                phux_client_send_paste(self.0, terminal, b"paste".as_ptr(), 5, false),
                PhuxClientResult::Ok
            );
            assert!(
                matches!(self.take_outgoing(), FrameKind::InputPaste { terminal_id, event }
                if terminal_id == TerminalId::local(7) && event.data == b"paste")
            );
            assert_eq!(
                phux_client_send_focus(self.0, terminal, true),
                PhuxClientResult::Ok
            );
            assert!(
                matches!(self.take_outgoing(), FrameKind::InputFocus { terminal_id, event: FocusEvent::Gained }
                if terminal_id == TerminalId::local(7))
            );
            assert_eq!(
                phux_client_terminal_resize(self.0, terminal, 100, 30),
                PhuxClientResult::Ok
            );
            assert!(
                matches!(self.take_outgoing(), FrameKind::TerminalResize { terminal_id, cols: 100, rows: 30 }
                if terminal_id == TerminalId::local(7))
            );
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let output = std::env::args_os().nth(1).map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../clients/cockpit/src/tests/fixtures")
        },
        PathBuf::from,
    );
    let hello = encode(&[hello()]);
    let attached = encode(&attached());
    let client = Client::new();
    client.negotiate_and_attach(&hello, &attached);
    let terminal = PhuxTerminalId {
        id: 7,
        ..PhuxTerminalId::default()
    };
    client.verify_grid(&terminal);
    client.verify_input(&terminal);
    std::fs::create_dir_all(&output)?;
    std::fs::write(output.join("hello.bin"), &hello)?;
    std::fs::write(output.join("attached.bin"), &attached)?;
    println!(
        "Validated C ABI lifecycle, grid, key/paste/focus/resize; wrote hello.bin ({} bytes), attached.bin ({} bytes) to {}",
        hello.len(),
        attached.len(),
        output.display()
    );
    Ok(())
}
