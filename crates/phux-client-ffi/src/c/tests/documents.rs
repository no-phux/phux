//! Whole published documents reach a C host as `PHUX_CLIENT_EFFECT_DOCUMENT`.

use super::*;
use phux_protocol::wire::frame::typed_document;
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{BootstrapId, BootstrapStreamProfile, ResourceId, StreamId, WindowId};

const TERMINAL: u32 = 40;
const SURFACE: u32 = 41;
const NOTES: u32 = 42;

/// Session 1: a terminal, a surface and a typed document its producer runs.
fn document_snapshot() -> SessionSnapshot {
    let session = SessionId::new(1);
    let window = WindowId::new(10);
    let terminal = ResourceId::local(TERMINAL);
    let document = |id, kind| {
        ResourceInfo::new(ResourceId::local(id), WindowId::new(0), 0, 0)
            .with_kind(kind)
            .with_session(Some(session))
            .with_parent(Some(terminal.clone()))
    };
    SessionSnapshot::new(session, window, terminal.clone())
        .with_sessions(vec![SessionInfo::new(session, "docs")])
        .with_windows(vec![WindowInfo::new(window, session, "docs")])
        .with_resources(vec![
            ResourceInfo::new(terminal.clone(), window, 80, 24),
            document(SURFACE, ResourceKind::PixelSurface),
            document(NOTES, ResourceKind::Document),
        ])
}

/// Attaches `id` and bootstraps it as a document stream whose cut is
/// `payload` at `seq`.
fn open(
    client: *mut PhuxClient,
    id: u32,
    profile: BootstrapStreamProfile,
    seq: u64,
    payload: &[u8],
) {
    let terminal_id = ResourceId::local(id);
    let options = PhuxAttachResourceOptions {
        size: mem::size_of::<PhuxAttachResourceOptions>(),
        version: ABI_VERSION,
        request_id: id,
        terminal_id: terminal_id_out(&terminal_id),
    };
    // SAFETY: the test owns the live client; `options` outlives the call.
    let queued = unsafe { phux_client_queue_attach_resource(client, &raw const options) };
    assert_eq!(queued, PhuxClientResult::Ok);
    let stream_id = StreamId::new(u64::from(id)).expect("stream");
    let bootstrap_id = BootstrapId::new(1).expect("bootstrap");
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile,
            cols: 0,
            rows: 0,
            base_seq: seq,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::copy_from_slice(payload),
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        assert_eq!(feed(client, &frame), PhuxClientResult::Ok);
    }
}

fn publish(client: *mut PhuxClient, id: u32, seq: u64, payload: &'static [u8]) {
    let frame = FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(id),
        stream_id: StreamId::new(u64::from(id)).expect("stream"),
        bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
        seq,
        bytes: payload.into(),
    };
    assert_eq!(feed(client, &frame), PhuxClientResult::Ok);
}

/// One staged document effect: resource, kind tag, seq, content type, document.
type Staged = (u32, u32, u64, Vec<u8>, Vec<u8>);

fn documents(client: *mut PhuxClient) -> Vec<Staged> {
    // SAFETY: the test owns the live client.
    let count = unsafe { phux_client_effect_count(client) };
    (0..count)
        .map(|index| effect_at(client, index))
        .filter(|effect| effect.kind == EFFECT_DOCUMENT)
        .map(|effect| {
            assert_eq!((effect.stream_id, effect.bootstrap_id), (0, 0));
            let bytes = effect_bytes(&effect);
            let (content_type, document) = bytes.split_at(effect.status_code as usize);
            (
                effect.terminal_id.id,
                effect.detail,
                effect.seq,
                content_type.to_vec(),
                document.to_vec(),
            )
        })
        .collect()
}

#[test]
fn documents_publish_their_kind_seq_content_type_and_bytes() {
    let client = attaching(&[], 7);
    complete_resource_attach(client, document_snapshot());
    open(
        client,
        SURFACE,
        BootstrapStreamProfile::PixelSurfacePngV1,
        2,
        b"\x89PNG-cut",
    );
    let typed = typed_document::encode_bootstrap("text/markdown", b"# hi");
    open(
        client,
        NOTES,
        BootstrapStreamProfile::DocumentTypedV1,
        4,
        &typed,
    );
    assert_eq!(
        documents(client),
        [
            (
                SURFACE,
                RESOURCE_KIND_PIXEL_SURFACE,
                2,
                Vec::new(),
                b"\x89PNG-cut".to_vec()
            ),
            (
                NOTES,
                RESOURCE_KIND_DOCUMENT,
                4,
                b"text/markdown".to_vec(),
                b"# hi".to_vec()
            ),
        ]
    );

    // Newest wins: a staged older document of the same resource is
    // superseded, not queued behind the newer one.
    publish(client, NOTES, 5, b"# two");
    publish(client, NOTES, 6, b"# three");
    assert_eq!(
        documents(client),
        [
            (
                SURFACE,
                RESOURCE_KIND_PIXEL_SURFACE,
                2,
                Vec::new(),
                b"\x89PNG-cut".to_vec()
            ),
            (
                NOTES,
                RESOURCE_KIND_DOCUMENT,
                6,
                b"text/markdown".to_vec(),
                b"# three".to_vec()
            ),
        ]
    );
    // SAFETY: the test owns the live client.
    unsafe {
        assert_eq!(phux_client_effect_clear(client), PhuxClientResult::Ok);
        assert_eq!(phux_client_effect_count(client), 0);
        phux_client_free(client);
    }
}

/// Feed `frame` into the runtime without the C projection, so the wakes it
/// raises stay undrained.
fn feed_undrained(client: *mut PhuxClient, frame: FrameKind) {
    // SAFETY: the test owns the live client and no ABI call is in flight.
    unsafe { (*client).inner.control().feed(frame) }.expect("feed");
}

fn live(id: u32, seq: u64, payload: &'static [u8]) -> FrameKind {
    FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(id),
        stream_id: StreamId::new(u64::from(id)).expect("stream"),
        bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
        seq,
        bytes: payload.into(),
    }
}

fn opened_surface() -> *mut PhuxClient {
    let client = attaching(&[], 7);
    complete_resource_attach(client, document_snapshot());
    open(
        client,
        SURFACE,
        BootstrapStreamProfile::PixelSurfacePngV1,
        2,
        b"\x89PNG-cut",
    );
    // SAFETY: the test owns the live client.
    assert_eq!(
        unsafe { phux_client_effect_clear(client) },
        PhuxClientResult::Ok
    );
    client
}

#[test]
fn a_stale_wake_copies_nothing_and_the_newer_wake_copies_once() {
    let client = opened_surface();
    feed_undrained(client, live(SURFACE, 3, b"\x89PNG-live"));
    // SAFETY: the test owns the live client and no ABI call is in flight.
    unsafe {
        // The wake for the cut, drained after the slot moved on.
        (*client)
            .inner
            .process_runtime_event(phux_client_runtime::control::Event::DocumentPublished {
                resource_id: ResourceId::local(SURFACE),
                generation: 1,
            })
            .expect("stale wake");
        (*client).inner.publish_effects();
    }
    assert!(
        documents(client).is_empty(),
        "the stale wake copies nothing"
    );
    // SAFETY: as above.
    unsafe { (*client).inner.process_runtime_events() }.expect("drain");
    assert_eq!(
        documents(client),
        [(
            SURFACE,
            RESOURCE_KIND_PIXEL_SURFACE,
            3,
            Vec::new(),
            b"\x89PNG-live".to_vec()
        )]
    );
    // SAFETY: the test owns the live client.
    unsafe { phux_client_free(client) };
}

#[test]
fn an_undrained_wake_stages_nothing_after_close_or_detach() {
    let client = opened_surface();
    feed_undrained(client, live(SURFACE, 3, b"\x89PNG-live"));
    let closed = FrameKind::ResourceClosed {
        terminal_id: ResourceId::local(SURFACE),
        exit_status: None,
        reason: phux_protocol::wire::frame::CloseReason::Exited,
        signal: None,
    };
    assert_eq!(feed(client, &closed), PhuxClientResult::Ok);
    assert!(documents(client).is_empty(), "closed");
    // SAFETY: the test owns the live client.
    unsafe { phux_client_free(client) };

    let client = opened_surface();
    feed_undrained(client, live(SURFACE, 3, b"\x89PNG-live"));
    // SAFETY: the test owns the live client and no ABI call is in flight.
    unsafe {
        assert!(
            (*client)
                .inner
                .control()
                .release_terminal(&ResourceId::local(SURFACE))
        );
        (*client).inner.process_runtime_events().expect("drain");
    }
    assert!(documents(client).is_empty(), "detached");
    // SAFETY: the test owns the live client.
    unsafe { phux_client_free(client) };
}
