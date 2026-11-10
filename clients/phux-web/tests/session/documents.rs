//! Document resources draw as read-only tiles: advertised in `HELLO`,
//! laid out after the terminals, decoded by content type, and never sent
//! input or a resize.

use super::*;
use phux_client_core::document_view::DocumentContent;
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::wire::frame::CloseReason;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot decoded here";

fn resource(id: u32, kind: ResourceKind, title: Option<&str>) -> ResourceInfo {
    ResourceInfo::new(ResourceId::local(id), WindowId::new(1), 0, 0)
        .with_kind(kind)
        .with_title(title.map(str::to_owned))
}

/// A connection attached to `resources`, the first focused.
async fn attached_to(resources: Vec<ResourceInfo>) -> Connection {
    let vt = Vt::load().await.expect("load engine");
    let mut session = Connection::new(&vt, 41, 10);
    session.on_frame(hello_ok(
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    ));
    let focused = resources[0].id.clone();
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), focused)
        .with_resources(resources);
    let outcome = session.on_frame(FrameKind::Attached {
        attach_id: 1,
        snapshot,
        initial_client_id: ClientId::new(1),
    });
    assert!(outcome.fatal.is_none(), "{:?}", outcome.fatal);
    session
}

/// One whole bootstrap; `bytes` is the payload as the wire carries it.
fn publish(
    session: &mut Connection,
    id: u32,
    serial: u64,
    profile: BootstrapStreamProfile,
    bytes: &[u8],
) -> Vec<Vec<u8>> {
    let (terminal_id, stream_id, bootstrap_id) =
        (ResourceId::local(id), stream(serial), bootstrap(serial));
    let (cols, rows) = if profile == BootstrapStreamProfile::SynthesizedVtRaw {
        (41, 10)
    } else {
        (0, 0)
    };
    let mut sent = Vec::new();
    for frame in [
        begin(
            terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            0,
        ),
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: Bytes::copy_from_slice(bytes),
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        let outcome = session.on_frame(frame);
        assert!(outcome.fatal.is_none(), "{:?}", outcome.fatal);
        sent.extend(outcome.send);
    }
    sent
}

fn typed(content_type: &str, bytes: &[u8]) -> Vec<u8> {
    let mut payload = vec![u8::try_from(content_type.len()).unwrap()];
    payload.extend_from_slice(content_type.as_bytes());
    payload.extend_from_slice(bytes);
    payload
}

fn resized(sent: &[Vec<u8>]) -> Vec<(ResourceId, u16)> {
    sent.iter()
        .filter_map(|frame| match decode_one(frame) {
            FrameKind::ResizeTerminal {
                terminal_id, cols, ..
            } => Some((terminal_id, cols)),
            _ => None,
        })
        .collect()
}

fn tiles(session: &Connection) -> Vec<(ResourceId, String, Option<DocumentContent>)> {
    session
        .document_tiles()
        .into_iter()
        .map(|(id, _, document)| (id, document.caption(), document.content.clone()))
        .collect()
}

fn text(text: &str) -> Option<DocumentContent> {
    Some(DocumentContent::Text {
        text: text.to_owned(),
        cut: None,
    })
}

#[wasm_bindgen_test]
async fn hello_advertises_every_document_kind_it_draws() {
    let vt = Vt::load().await.expect("load engine");
    let kinds = Connection::new(&vt, 20, 3)
        .advertised_capabilities()
        .resource_kinds;
    for kind in [
        ResourceKind::Terminal,
        ResourceKind::AgentSession,
        ResourceKind::AccessibilityTree,
        ResourceKind::PixelSurface,
        ResourceKind::Document,
    ] {
        assert!(kinds.contains(kind), "{kind}");
    }
}

#[wasm_bindgen_test]
async fn a_document_only_session_draws_each_document_by_content_type() {
    let mut session = attached_to(vec![
        resource(7, ResourceKind::AccessibilityTree, Some("Safari")),
        resource(8, ResourceKind::Document, Some("notes")),
    ])
    .await;
    assert_eq!(session.pane_rects().len(), 2, "each document has a tile");
    let mut sent = publish(
        &mut session,
        7,
        1,
        BootstrapStreamProfile::AccessibilityTreeJsonV1,
        br#"{"a":[1]}"#,
    );
    sent.extend(publish(
        &mut session,
        8,
        2,
        BootstrapStreamProfile::DocumentTypedV1,
        &typed("text/html", b"<script>alert(1)</script>"),
    ));
    sent.extend(
        session
            .on_frame(FrameKind::AttachReady { attach_id: 1 })
            .send,
    );
    assert!(
        session.render_visible(),
        "a publication is something to paint"
    );
    assert_eq!(
        tiles(&session),
        [
            (
                ResourceId::local(7),
                "Safari · accessibility tree".to_owned(),
                text("{\n  \"a\": [\n    1\n  ]\n}"),
            ),
            (
                ResourceId::local(8),
                "notes · text/html".to_owned(),
                text("<script>alert(1)</script>"),
            ),
        ]
    );
    assert!(resized(&sent).is_empty(), "a tile is never resized");
    assert_eq!(session.key_frame(key()), None, "a tile takes no input");
    assert!(session.focus_pane(&ResourceId::local(7)).is_err());
    assert!(session.focus_next_pane().is_err());
}

#[wasm_bindgen_test]
async fn images_opaque_and_invalid_publications_and_their_lines() {
    let mut session = attached_to(vec![
        resource(2, ResourceKind::PixelSurface, Some("display")),
        resource(3, ResourceKind::Document, None),
        resource(4, ResourceKind::Document, None),
    ])
    .await;
    assert_eq!(
        session.document(&ResourceId::local(2)).unwrap().notice(),
        Some("Waiting for the producer’s first document…".to_owned())
    );
    publish(
        &mut session,
        2,
        1,
        BootstrapStreamProfile::PixelSurfacePngV1,
        PNG,
    );
    publish(
        &mut session,
        3,
        2,
        BootstrapStreamProfile::DocumentTypedV1,
        &typed("application/pdf", &[0; 2048]),
    );
    publish(
        &mut session,
        4,
        3,
        BootstrapStreamProfile::DocumentTypedV1,
        &typed("IMAGE/WEBP", b"RIFF"),
    );
    let surface = session.document(&ResourceId::local(2)).unwrap();
    assert_eq!(
        surface.content,
        Some(DocumentContent::Image("image/png".to_owned()))
    );
    assert_eq!((surface.image.as_slice(), surface.notice()), (PNG, None));
    let opaque = session.document(&ResourceId::local(3)).unwrap();
    assert_eq!(
        opaque.notice().as_deref(),
        Some("application/pdf · 2.0 KiB")
    );
    assert!(opaque.image.is_empty());
    let webp = session.document(&ResourceId::local(4)).unwrap();
    assert_eq!(
        webp.content,
        Some(DocumentContent::Image("image/webp".to_owned()))
    );
    assert_eq!(webp.caption(), "IMAGE/WEBP");
    let outcome = session.on_frame(FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(2),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        seq: 1,
        bytes: Bytes::from_static(b"GIF89a"),
    });
    assert!(outcome.fatal.is_none() && outcome.render);
    assert_eq!(
        session
            .document(&ResourceId::local(2))
            .unwrap()
            .notice()
            .as_deref(),
        Some("Invalid frame: not a PNG or JPEG")
    );
}

#[wasm_bindgen_test]
async fn documents_tile_after_terminals_and_only_terminals_are_resized() {
    let mut session = attached_to(vec![
        ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 41, 10),
        resource(2, ResourceKind::PixelSurface, Some("display")),
    ])
    .await;
    let rects: Vec<_> = session.pane_rects().into_iter().map(|(id, _)| id).collect();
    assert_eq!(rects, [ResourceId::local(1), ResourceId::local(2)]);
    publish(
        &mut session,
        1,
        1,
        BootstrapStreamProfile::SynthesizedVtRaw,
        b"$ ",
    );
    publish(
        &mut session,
        2,
        2,
        BootstrapStreamProfile::PixelSurfacePngV1,
        PNG,
    );
    let ready = session.on_frame(FrameKind::AttachReady { attach_id: 1 });
    assert_eq!(resized(&ready.send), [(ResourceId::local(1), 20)]);
    assert_eq!(session.focused_pane(), Some(ResourceId::local(1)));

    // A document that arrives later takes a tile, and the terminals give it room.
    let sent = publish(
        &mut session,
        5,
        3,
        BootstrapStreamProfile::DocumentTypedV1,
        &typed("text/plain", b"later"),
    );
    assert_eq!(session.pane_rects().len(), 3);
    assert_eq!(tiles(&session)[1].2, text("later"));
    assert!(
        resized(&sent)
            .iter()
            .all(|(id, _)| id == &ResourceId::local(1))
    );

    for id in [2, 5] {
        let closed = session.on_frame(FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(id),
            exit_status: None,
            reason: CloseReason::Unknown,
            signal: None,
        });
        assert!(closed.fatal.is_none() && closed.panes);
    }
    assert!(session.document_tiles().is_empty());
    assert!(session.document(&ResourceId::local(2)).is_none());
    assert_eq!(session.pane_rects().len(), 1);
    assert_eq!(session.focused_pane(), Some(ResourceId::local(1)));
}
