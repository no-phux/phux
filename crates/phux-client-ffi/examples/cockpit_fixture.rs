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
    PhuxClientState, PhuxKeyEvent, PhuxResourceId, PhuxTerminalGridView,
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
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{
    BootstrapId, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientId,
    PROTOCOL_VERSION, ResourceId, ServerFeature, ServerFeatureSet, SessionId, StreamId, WindowId,
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
    let terminal_id = ResourceId::local(7);
    let session_id = SessionId::new(1);
    let window_id = WindowId::new(1);
    let stream_id = StreamId::new(7).expect("nonzero stream");
    let bootstrap_id = BootstrapId::new(1).expect("nonzero bootstrap");
    let snapshot = SessionSnapshot::new(session_id, window_id, terminal_id.clone())
        .with_sessions(vec![SessionInfo::new(session_id, "fixture")])
        .with_windows(vec![WindowInfo::new(window_id, session_id, "fixture")])
        .with_resources(vec![ResourceInfo::new(
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
            // ATTACH_READY starts the shared workspace's automatic read:
            // exactly one GET_METADATA and one GET_STATE, nothing else.
            assert_eq!(phux_client_outgoing_count(self.0), 2);
            for index in 0..2 {
                let mut bytes = PhuxBytes::default();
                assert_eq!(
                    phux_client_outgoing_get(self.0, index, &raw mut bytes),
                    PhuxClientResult::Ok
                );
                let (frame, _) = FrameKind::decode(slice::from_raw_parts(bytes.data, bytes.len))
                    .expect("FFI emits a canonical frame");
                assert!(matches!(
                    frame,
                    FrameKind::GetMetadata { .. } | FrameKind::Command { .. }
                ));
            }
            assert_eq!(phux_client_outgoing_clear(self.0), PhuxClientResult::Ok);
        }
    }

    fn verify_grid(&self, terminal: &PhuxResourceId) {
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

    fn verify_input(&self, terminal: &PhuxResourceId) {
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
                if terminal_id == ResourceId::local(7) && event.text.as_deref() == Some("a"))
            );
            assert_eq!(
                phux_client_send_paste(self.0, terminal, b"paste".as_ptr(), 5, false),
                PhuxClientResult::Ok
            );
            assert!(
                matches!(self.take_outgoing(), FrameKind::InputPaste { terminal_id, event }
                if terminal_id == ResourceId::local(7) && event.data == b"paste")
            );
            assert_eq!(
                phux_client_send_focus(self.0, terminal, true),
                PhuxClientResult::Ok
            );
            assert!(
                matches!(self.take_outgoing(), FrameKind::InputFocus { terminal_id, event: FocusEvent::Gained }
                if terminal_id == ResourceId::local(7))
            );
            assert_eq!(
                phux_client_terminal_resize(self.0, terminal, 100, 30),
                PhuxClientResult::Ok
            );
            assert!(
                matches!(self.take_outgoing(), FrameKind::ResizeTerminal { terminal_id, cols: 100, rows: 30 }
                if terminal_id == ResourceId::local(7))
            );
        }
    }
}

/// The same handshake, from a federation hub that also lists its satellites'
/// directories (`LIST_DIRECTORY_HOST`).
fn hello_with_directory_host() -> FrameKind {
    let FrameKind::HelloOk {
        protocol_major,
        protocol_minor,
        protocol_patch,
        server_id,
        selected_profile,
        bootstrap_limits,
        ..
    } = hello()
    else {
        unreachable!("hello() builds HELLO_OK");
    };
    FrameKind::HelloOk {
        protocol_major,
        protocol_minor,
        protocol_patch,
        server_caps: ServerCapabilities::new().with_features(ServerFeatureSet::with(&[
            ServerFeature::TerminalReply,
            ServerFeature::ListDirectory,
            ServerFeature::ListDirectoryHost,
        ])),
        server_id,
        selected_profile,
        bootstrap_limits,
    }
}

/// A satellite listing through the C ABI: the host rides on the frame, and
/// the reply reads back as an ordinary listing.
fn verify_directory_host(hello: &[u8], attached: &[u8], listing: &[u8]) {
    use phux_client_ffi::{
        PhuxDirectoryRequest, phux_client_directory_host_supported, phux_client_list_directory_on,
    };
    use phux_protocol::ids::SatelliteHost;
    let client = Client::new();
    client.negotiate_and_attach(hello, attached);
    // SAFETY: live same-thread handle, valid spans, and writable outputs.
    unsafe {
        let mut supported = false;
        assert_eq!(
            phux_client_directory_host_supported(client.0, &raw mut supported),
            PhuxClientResult::Ok
        );
        assert!(supported);
        let request = PhuxDirectoryRequest {
            size: size_of::<PhuxDirectoryRequest>(),
            version: ABI_VERSION,
            request_id: 1,
            path: span(b"/work"),
            host: span(b"fixture-host"),
        };
        assert_eq!(
            phux_client_list_directory_on(client.0, &raw const request),
            PhuxClientResult::Ok
        );
        assert!(matches!(client.take_outgoing(),
            FrameKind::ListDirectory { request_id: 1, ref path, host: Some(ref host) }
            if path == "/work" && *host == SatelliteHost::new("fixture-host")));
        client.feed(listing);
        assert_eq!(phux_client_outgoing_count(client.0), 0);
    }
}

/// The same handshake, from a server that also answers `LIST_DIRECTORY`.
fn hello_with_directory() -> FrameKind {
    let FrameKind::HelloOk {
        protocol_major,
        protocol_minor,
        protocol_patch,
        server_id,
        selected_profile,
        bootstrap_limits,
        ..
    } = hello()
    else {
        unreachable!("hello() builds HELLO_OK");
    };
    FrameKind::HelloOk {
        protocol_major,
        protocol_minor,
        protocol_patch,
        server_caps: ServerCapabilities::new().with_features(ServerFeatureSet::with(&[
            ServerFeature::TerminalReply,
            ServerFeature::ListDirectory,
        ])),
        server_id,
        selected_profile,
        bootstrap_limits,
    }
}

/// The reply to the first host request (ID 1) after attach: `/work`, with a
/// hidden directory, a plain one, and a symlink to a directory.
fn directory_listing() -> FrameKind {
    use phux_protocol::wire::frame::{DirectoryEntry, DirectoryListing};
    let entry = |name: &str, is_symlink| DirectoryEntry {
        name: name.to_owned(),
        is_symlink,
    };
    FrameKind::DirectoryListing {
        request_id: 1,
        result: Ok(DirectoryListing {
            path: "/work".to_owned(),
            parent: Some("/".to_owned()),
            entries: vec![
                entry(".config", false),
                entry("cockpit", false),
                entry("phux", true),
            ],
            truncated: false,
        }),
    }
}

/// Request `/work` through the C ABI and read the fixture reply back.
fn verify_directory(hello: &[u8], attached: &[u8], listing: &[u8]) {
    use phux_client_ffi::{
        PhuxDirectoryEntry, PhuxDirectoryListingInfo, phux_client_directory_entry_get,
        phux_client_directory_info, phux_client_list_directory,
    };
    let client = Client::new();
    client.negotiate_and_attach(hello, attached);
    // SAFETY: live same-thread handle, valid spans, and writable outputs; the
    // borrowed name is copied before any further mutable call.
    unsafe {
        assert_eq!(
            phux_client_list_directory(client.0, 1, span(b"/work")),
            PhuxClientResult::Ok
        );
        assert!(matches!(client.take_outgoing(),
            FrameKind::ListDirectory { request_id: 1, ref path, host: None } if path == "/work"));
        client.feed(listing);
        let mut info = PhuxDirectoryListingInfo {
            size: size_of::<PhuxDirectoryListingInfo>(),
            version: ABI_VERSION,
            supported: false,
            truncated: false,
            has_parent: false,
            request_id: 0,
            status: 0,
            error_code: 0,
            entry_count: 0,
            path: PhuxBytes::default(),
            parent: PhuxBytes::default(),
            message: PhuxBytes::default(),
        };
        assert_eq!(
            phux_client_directory_info(client.0, &raw mut info),
            PhuxClientResult::Ok
        );
        assert!(info.supported && info.has_parent);
        assert_eq!((info.request_id, info.status, info.entry_count), (1, 2, 3));
        let mut entry = PhuxDirectoryEntry {
            size: size_of::<PhuxDirectoryEntry>(),
            version: ABI_VERSION,
            flags: 0,
            name: PhuxBytes::default(),
        };
        assert_eq!(
            phux_client_directory_entry_get(client.0, 2, &raw mut entry),
            PhuxClientResult::Ok
        );
        assert_eq!(
            slice::from_raw_parts(entry.name.data, entry.name.len),
            b"phux"
        );
        assert_eq!(entry.flags, 1);
    }
}

/// The reply to a standby's first host request (ID 1): `GET_STATE` listing two
/// sessions, `build` (1) and `deploy` (2), for a client that never attaches.
fn standby_state() -> FrameKind {
    use phux_protocol::wire::frame::{CommandResult, CommandValue};
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(10), ResourceId::local(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "build"),
            SessionInfo::new(SessionId::new(2), "deploy"),
        ]);
    FrameKind::CommandResult {
        request_id: 1,
        result: CommandResult::OkWith(CommandValue::State(snapshot)),
    }
}

/// Negotiate without attaching, query sessions, and read the fixture back.
fn verify_standby(hello: &[u8], state: &[u8]) {
    use phux_client_ffi::{phux_client_query_sessions, phux_client_session_count};
    use phux_protocol::wire::frame::{Command, StateScope};
    let client = Client::new();
    // SAFETY: live same-thread handle with valid spans throughout.
    unsafe {
        assert_eq!(
            phux_client_queue_hello(client.0, span(b"cockpit-fixture")),
            PhuxClientResult::Ok
        );
        assert!(matches!(client.take_outgoing(), FrameKind::Hello { .. }));
        client.feed(hello);
        assert_eq!(phux_client_state(client.0), PhuxClientState::Negotiated);
        assert_eq!(
            phux_client_query_sessions(client.0, 1),
            PhuxClientResult::Ok
        );
        assert!(matches!(
            client.take_outgoing(),
            FrameKind::Command {
                request_id: 1,
                command: Command::GetState {
                    scope: StateScope::Server
                }
            }
        ));
        client.feed(state);
        assert_eq!(phux_client_session_count(client.0), 2);
        assert_eq!(phux_client_state(client.0), PhuxClientState::Negotiated);
        assert_eq!(phux_client_outgoing_count(client.0), 0, "no ATTACH, ever");
    }
}

/// A server with keep-empty sessions (ADR-0105), otherwise `hello()`.
fn hello_keep_empty() -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new().with_features(ServerFeatureSet::with(&[
            ServerFeature::TerminalReply,
            ServerFeature::KeepEmptySessions,
        ])),
        server_id: b"cockpit-fixture".to_vec(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::new(1024, 1024).expect("valid limits"),
    }
}

/// `build` (1) with a window, and `scratch` (3), keep-empty with none.
fn keep_empty_sessions() -> Vec<SessionInfo> {
    vec![
        SessionInfo::new(SessionId::new(1), "build").with_window_count(1),
        SessionInfo::new(SessionId::new(3), "scratch").with_keep_empty(true),
    ]
}

/// A listing client's first query (ID 1) on a server holding `scratch`.
fn standby_keep_empty_state() -> FrameKind {
    use phux_protocol::wire::frame::{CommandResult, CommandValue};
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(10), ResourceId::local(1))
        .with_sessions(keep_empty_sessions());
    FrameKind::CommandResult {
        request_id: 1,
        result: CommandResult::OkWith(CommandValue::State(snapshot)),
    }
}

/// The registry as seen attached to `scratch`: no windows and no resources,
/// with the server's sentinel focus ids.
fn empty_session_snapshot() -> SessionSnapshot {
    SessionSnapshot::new(SessionId::new(3), WindowId::new(0), ResourceId::local(0))
        .with_sessions(keep_empty_sessions())
}

/// `ATTACH` (id 1) to `scratch`: nothing to bootstrap, then `ATTACH_READY`.
fn attached_empty() -> [FrameKind; 2] {
    [
        FrameKind::Attached {
            attach_id: 1,
            snapshot: empty_session_snapshot(),
            initial_client_id: ClientId::new(1),
        },
        FrameKind::AttachReady { attach_id: 1 },
    ]
}

/// The automatic workspace read that follows: no layout metadata (the server
/// deletes it with a keep-empty session's last window), then the registry.
/// Correlated as a fresh client's first internal requests are.
fn workspace_empty() -> [FrameKind; 2] {
    use phux_protocol::wire::frame::{CommandResult, CommandValue};
    [
        FrameKind::MetadataValue {
            request_id: 0x8000_0001,
            value: None,
        },
        FrameKind::CommandResult {
            request_id: 0x8000_0000,
            result: CommandResult::OkWith(CommandValue::State(empty_session_snapshot())),
        },
    ]
}

/// A listing client reads `scratch` as keep-empty and empty; an attached one
/// attaches it with no terminals and reads its empty workspace.
fn verify_keep_empty(hello: &[u8], standby: &[u8], attached: &[u8], workspace: &[u8]) {
    use phux_client_ffi::{
        PHUX_SESSION_FLAG_EMPTY, PHUX_SESSION_FLAG_KEEP_EMPTY, phux_client_keep_empty_supported,
        phux_client_query_sessions, phux_client_session_flags,
    };
    let listing = Client::new();
    let flags = |client: &Client, index: usize| {
        let mut flags = u32::MAX;
        // SAFETY: live same-thread handle and a writable output.
        assert_eq!(
            unsafe { phux_client_session_flags(client.0, index, &raw mut flags) },
            PhuxClientResult::Ok
        );
        flags
    };
    // SAFETY: live same-thread handles with valid spans throughout.
    unsafe {
        assert_eq!(
            phux_client_queue_hello(listing.0, span(b"cockpit-fixture")),
            PhuxClientResult::Ok
        );
        assert!(matches!(listing.take_outgoing(), FrameKind::Hello { .. }));
        listing.feed(hello);
        let mut supported = false;
        assert_eq!(
            phux_client_keep_empty_supported(listing.0, &raw mut supported),
            PhuxClientResult::Ok
        );
        assert!(supported);
        assert_eq!(
            phux_client_query_sessions(listing.0, 1),
            PhuxClientResult::Ok
        );
        assert!(matches!(listing.take_outgoing(), FrameKind::Command { .. }));
        listing.feed(standby);
        assert_eq!(phux_client_outgoing_count(listing.0), 0, "no ATTACH, ever");
    }
    assert_eq!(flags(&listing, 0), 0);
    assert_eq!(
        flags(&listing, 1),
        PHUX_SESSION_FLAG_KEEP_EMPTY | PHUX_SESSION_FLAG_EMPTY
    );

    let client = Client::new();
    let options = PhuxAttachOptions {
        size: size_of::<PhuxAttachOptions>(),
        version: ABI_VERSION,
        attach_id: 1,
        target_kind: 2,
        session_id: 3,
        name: PhuxBytes::default(),
        cols: 80,
        rows: 24,
        has_pixel_size: false,
        pixel_width: 0,
        pixel_height: 0,
        request_scrollback: false,
        scrollback_limit_lines: 0,
    };
    // SAFETY: as above.
    unsafe {
        assert_eq!(
            phux_client_queue_hello(client.0, span(b"cockpit-fixture")),
            PhuxClientResult::Ok
        );
        assert!(matches!(client.take_outgoing(), FrameKind::Hello { .. }));
        client.feed(hello);
        assert_eq!(
            phux_client_queue_attach(client.0, &raw const options),
            PhuxClientResult::Ok
        );
        assert!(matches!(client.take_outgoing(), FrameKind::Attach { .. }));
        client.feed(attached);
        assert_eq!(phux_client_state(client.0), PhuxClientState::Attached);
        // The automatic workspace read: metadata, then registry.
        assert_eq!(phux_client_outgoing_count(client.0), 2);
        assert_eq!(phux_client_outgoing_clear(client.0), PhuxClientResult::Ok);
        client.feed(workspace);
        assert_eq!(phux_client_state(client.0), PhuxClientState::Attached);
    }
    assert_eq!(
        flags(&client, 1),
        PHUX_SESSION_FLAG_KEEP_EMPTY | PHUX_SESSION_FLAG_EMPTY
    );
}

/// The server's broadcast of an applied rename (`phux.session.name/v1`):
/// the attached fixture session `fixture` is now `renamed`.
fn session_renamed() -> FrameKind {
    use phux_protocol::wire::frame::{SESSION_NAME_KEY, Scope};
    FrameKind::MetadataChanged {
        scope: Scope::Global,
        key: SESSION_NAME_KEY.to_owned(),
        value: Some(b"fixture\0renamed".to_vec()),
    }
}

/// An attached client reads the broadcast into its session list in place.
fn verify_session_renamed(hello: &[u8], attached: &[u8], renamed: &[u8]) {
    use phux_client_ffi::{PhuxSessionInfo, phux_client_session_count, phux_client_session_get};
    let client = Client::new();
    client.negotiate_and_attach(hello, attached);
    client.feed(renamed);
    let mut session = PhuxSessionInfo::default();
    // SAFETY: live same-thread handle; the name borrows the client until the
    // next mutable call, and none happens before it is read.
    unsafe {
        assert_eq!(phux_client_session_count(client.0), 1);
        assert_eq!(
            phux_client_session_get(client.0, 0, &raw mut session),
            PhuxClientResult::Ok
        );
        assert_eq!(
            slice::from_raw_parts(session.name.data, session.name.len),
            b"renamed"
        );
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
    let terminal = PhuxResourceId {
        id: 7,
        ..PhuxResourceId::default()
    };
    client.verify_grid(&terminal);
    client.verify_input(&terminal);
    let hello_directory = encode(&[hello_with_directory()]);
    let listing = encode(&[directory_listing()]);
    verify_directory(&hello_directory, &attached, &listing);
    let hello_directory_host = encode(&[hello_with_directory_host()]);
    verify_directory_host(&hello_directory_host, &attached, &listing);
    let standby = encode(&[standby_state()]);
    verify_standby(&hello, &standby);
    let renamed = encode(&[session_renamed()]);
    verify_session_renamed(&hello, &attached, &renamed);
    let hello_keep_empty = encode(&[hello_keep_empty()]);
    let standby_keep_empty = encode(&[standby_keep_empty_state()]);
    let attached_empty = encode(&attached_empty());
    let workspace_empty = encode(&workspace_empty());
    verify_keep_empty(
        &hello_keep_empty,
        &standby_keep_empty,
        &attached_empty,
        &workspace_empty,
    );
    std::fs::create_dir_all(&output)?;
    std::fs::write(output.join("session_renamed.bin"), &renamed)?;
    std::fs::write(output.join("hello_keep_empty.bin"), &hello_keep_empty)?;
    std::fs::write(
        output.join("standby_keep_empty_state.bin"),
        &standby_keep_empty,
    )?;
    std::fs::write(output.join("attached_empty.bin"), &attached_empty)?;
    std::fs::write(output.join("workspace_empty.bin"), &workspace_empty)?;
    std::fs::write(output.join("hello.bin"), &hello)?;
    std::fs::write(output.join("attached.bin"), &attached)?;
    std::fs::write(output.join("hello_directory.bin"), &hello_directory)?;
    std::fs::write(
        output.join("hello_directory_host.bin"),
        &hello_directory_host,
    )?;
    std::fs::write(output.join("directory_listing.bin"), &listing)?;
    std::fs::write(output.join("standby_state.bin"), &standby)?;
    println!(
        "Validated C ABI lifecycle, grid, key/paste/focus/resize, directory listing (serving host and satellite), standby session query, session rename and keep-empty sessions; wrote hello.bin ({} bytes), attached.bin ({} bytes), hello_directory.bin ({} bytes), hello_directory_host.bin ({} bytes), directory_listing.bin ({} bytes), standby_state.bin ({} bytes), session_renamed.bin ({} bytes), hello_keep_empty.bin, standby_keep_empty_state.bin, attached_empty.bin and workspace_empty.bin to {}",
        hello.len(),
        attached.len(),
        hello_directory.len(),
        hello_directory_host.len(),
        listing.len(),
        standby.len(),
        renamed.len(),
        output.display()
    );
    Ok(())
}
