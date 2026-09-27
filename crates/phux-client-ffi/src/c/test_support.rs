//! Test fixtures that drive an ABI client through the real HELLO and ATTACH
//! lifecycle instead of seeding client fields.

use super::*;
use phux_protocol::caps::ServerCapabilities;
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{
    BootstrapId, BootstrapProfile, BootstrapStreamProfile, ResourceId, ServerFeature,
    ServerFeatureSet, StreamId, WindowId,
};

pub(crate) const fn limits() -> Limits {
    Limits {
        bootstrap_chunk: 1024,
        history_page: 1024,
        history_page_rows: 128,
        history_cache_bytes: 4096,
        history_materialized_rows: 1024,
        history_prefetch_rows: 64,
    }
}

/// A fresh, un-negotiated client; release it with `phux_client_free`.
pub(crate) fn new_client() -> *mut PhuxClient {
    Box::into_raw(Box::new(PhuxClient::new(Client::new(limits()))))
}

pub(crate) fn feed(client: *mut PhuxClient, frame: &FrameKind) -> PhuxClientResult {
    let mut encoded = bytes::BytesMut::new();
    frame.encode(&mut encoded);
    // SAFETY: the caller owns the live client; `encoded` outlives the call.
    unsafe { phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) }
}

pub(crate) fn caps(features: &[ServerFeature]) -> ServerCapabilities {
    ServerCapabilities::new().with_features(ServerFeatureSet::with(features))
}

pub(crate) fn hello_ok(server_caps: ServerCapabilities, profile: BootstrapProfile) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps,
        server_id: b"server".to_vec(),
        selected_profile: profile,
        bootstrap_limits: BootstrapLimits::new(1024, 1024).expect("valid test limits"),
    }
}

/// Queues HELLO and answers it with `hello`, leaving the outgoing queue empty.
pub(crate) fn negotiate_frame(client: *mut PhuxClient, hello: &FrameKind) {
    // SAFETY: the caller owns the live client; the literal name is readable.
    let queued = unsafe { phux_client_queue_hello(client, bytes_out(b"test")) };
    assert_eq!(queued, PhuxClientResult::Ok);
    assert_eq!(feed(client, hello), PhuxClientResult::Ok);
    // SAFETY: the caller owns the live client.
    unsafe { (*client).inner.outgoing.clear() };
}

pub(crate) fn negotiate_with(
    client: *mut PhuxClient,
    server_caps: ServerCapabilities,
    profile: BootstrapProfile,
) {
    negotiate_frame(client, &hello_ok(server_caps, profile));
}

/// A client negotiated with `features` and the raw VT profile.
pub(crate) fn negotiated_client(features: &[ServerFeature]) -> *mut PhuxClient {
    let client = new_client();
    negotiate_with(client, caps(features), BootstrapProfile::SynthesizedVtRaw);
    client
}

/// Queues an attach to the last session, leaving the outgoing queue empty.
pub(crate) fn queue_attach(client: *mut PhuxClient, attach_id: u32) {
    let options = PhuxAttachOptions {
        size: mem::size_of::<PhuxAttachOptions>(),
        version: ABI_VERSION,
        attach_id,
        target_kind: 0,
        session_id: 0,
        name: PhuxBytes::default(),
        cols: 80,
        rows: 24,
        has_pixel_size: false,
        pixel_width: 0,
        pixel_height: 0,
        request_scrollback: false,
        scrollback_limit_lines: 0,
    };
    // SAFETY: the caller owns the live client; `options` outlives the call.
    let queued = unsafe { phux_client_queue_attach(client, &raw const options) };
    assert_eq!(queued, PhuxClientResult::Ok);
    // SAFETY: the caller owns the live client.
    unsafe { (*client).inner.outgoing.clear() };
}

/// A client negotiated with `features` whose attach `attach_id` is in flight.
pub(crate) fn attaching(features: &[ServerFeature], attach_id: u32) -> *mut PhuxClient {
    let client = negotiated_client(features);
    queue_attach(client, attach_id);
    client
}

/// A client fully attached to one bootstrapped terminal, `ResourceId::local(1)`,
/// with its outgoing and effect queues empty.
pub(crate) fn attached_client(features: &[ServerFeature]) -> *mut PhuxClient {
    let client = attaching(features, 1);
    let terminal = ResourceId::local(1);
    let snapshot = single_terminal_snapshot(terminal.clone(), 80, 24);
    assert_eq!(
        feed(client, &attached_frame(1, snapshot)),
        PhuxClientResult::Ok
    );
    feed_bootstrap(client, &terminal, (1, 1), (80, 24), b"");
    assert_eq!(
        feed(client, &FrameKind::AttachReady { attach_id: 1 }),
        PhuxClientResult::Ok
    );
    // SAFETY: the caller owns the live client.
    unsafe {
        (*client).inner.outgoing.clear();
        assert_eq!(phux_client_effect_clear(client), PhuxClientResult::Ok);
    }
    client
}

/// Session 1, window 1, holding one `cols` x `rows` terminal.
pub(crate) fn single_terminal_snapshot(
    terminal: ResourceId,
    cols: u16,
    rows: u16,
) -> SessionSnapshot {
    let session = SessionId::new(1);
    let window = WindowId::new(1);
    SessionSnapshot::new(session, window, terminal.clone())
        .with_windows(vec![WindowInfo::new(window, session, "main")])
        .with_resources(vec![ResourceInfo::new(terminal, window, cols, rows)])
}

pub(crate) fn attached_frame(attach_id: u32, snapshot: SessionSnapshot) -> FrameKind {
    FrameKind::Attached {
        attach_id,
        snapshot,
        initial_client_id: phux_protocol::ClientId::new(9),
    }
}

/// Feeds BEGIN, one CHUNK of `payload`, and READY for a raw-VT generation.
pub(crate) fn feed_bootstrap(
    client: *mut PhuxClient,
    terminal_id: &ResourceId,
    (stream, bootstrap): (u64, u64),
    (cols, rows): (u16, u16),
    payload: &[u8],
) {
    let stream_id = StreamId::new(stream).expect("stream");
    let bootstrap_id = BootstrapId::new(bootstrap).expect("bootstrap");
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols,
            rows,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::copy_from_slice(payload),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        assert_eq!(feed(client, &frame), PhuxClientResult::Ok);
    }
}
