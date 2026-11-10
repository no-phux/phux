//! Non-Terminal layout leaves (docs/resource-kinds.md): another client puts
//! a document or surface into the session's arrangement, and the TUI adopts
//! it as a placeholder tile instead of refusing the attach.
//!
//! The kind comes from the server's graph: a server that knows this client
//! decodes no documents never bootstraps one to it, so only the fallback
//! test below feeds a document bootstrap.

use super::*;

/// A Terminal (`pane`) and, when given, a titled pixel surface in session 1.
fn graph(pane: &ResourceId, surface: Option<&ResourceId>) -> SessionSnapshot {
    let (window, session) = (WindowId::new(1), SessionId::new(1));
    let mut resources = vec![ResourceInfo::new(pane.clone(), window, 100, 30)];
    resources.extend(surface.map(|surface| {
        ResourceInfo::resource(surface.clone(), ResourceKind::PixelSurface)
            .with_session(Some(session))
            .with_title(Some("Safari".to_owned()))
    }));
    SessionSnapshot::new(session, window, pane.clone())
        .with_sessions(vec![SessionInfo::new(session, "work".to_owned())])
        .with_windows(vec![WindowInfo::new(window, session, "w0".to_owned())])
        .with_resources(resources)
}

/// Attached to [`graph`] with the Terminal's bootstrap delivered; nothing
/// is bootstrapped for the surface.
fn attached_to(pane: &ResourceId, surface: Option<&ResourceId>) -> H {
    let mut h = H::new().with_viewport((100, 30));
    h.send(attached(graph(pane, surface), 1));
    h.send(begin_frame(pane));
    h.send(chunk_frame(pane, b"terminal is usable"));
    h.send(ready_frame(pane));
    h.send(FrameKind::AttachReady { attach_id: 1 });
    h
}

/// `pane` beside `other`, focused on `pane`.
fn beside_layout(pane: &ResourceId, other: &ResourceId) -> ArrangementDoc {
    let mut layout = ArrangementDoc::single(pane.clone());
    beside(&mut layout, other);
    layout
}

fn encoded(layout: &ArrangementDoc) -> Vec<u8> {
    layout.encode_cbor().expect("encode")
}

fn spawned(id: &ResourceId, kind: ResourceKind, parent: &ResourceId) -> FrameKind {
    event(
        id,
        AgentEvent::ResourceSpawned {
            kind,
            parent: Some(parent.clone()),
        },
    )
}

/// A full-frame paint of the active window, CSI stripped.
fn paint(h: &mut H) -> String {
    let theme = crate::render::theme::Theme::default();
    let active = h
        .mirror
        .workspace
        .render_group(h.mirror.zoomed.as_ref())
        .expect("window")
        .into_owned();
    let mut chrome = crate::attach::chrome_ctx::ChromeCtx {
        viewport: h.viewport,
        sidebar: None,
        status_bar: None,
        sidebar_painter: None,
        session_name: "",
        theme: &theme,
        leaves: &h.mirror.leaves,
    };
    h.out.clear();
    crate::attach::paint::paint_full_frame(
        &mut h.out,
        &active,
        &mut h.mirror.panes,
        &h.mirror.engine_kernel,
        h.mirror.focused_resource.as_ref(),
        &mut chrome,
    );
    strip_csi(&h.out_str())
}

/// The layout read and a later broadcast both adopt a layout naming a
/// surface the snapshot listed: no refusal, no attach, focus kept.
#[test]
fn a_layout_naming_a_surface_is_adopted_not_refused() {
    let (pane, surface) = (tid(1), tid(4));
    let mut h = attached_to(&pane, Some(&surface));
    let layout = encoded(&beside_layout(&pane, &surface));
    h.layout_request = Some(7);
    let read = h.send(FrameKind::MetadataValue {
        request_id: 7,
        value: Some(layout.clone()),
    });
    assert!(read.layout_get_answered && read.layout_replaced);
    assert!(read.attach_panes.is_empty(), "{:?}", read.attach_panes);
    assert_eq!(h.leaves(), vec![pane.clone(), surface.clone()]);
    assert_eq!(h.mirror.focused_resource, Some(pane.clone()));
    assert!(!h.mirror.panes.contains_key(&surface));

    h.layout_request = None;
    let broadcast = h.send(layout_changed(1, Some(layout)));
    assert!(broadcast.layout_replaced && broadcast.attach_panes.is_empty());
    assert_eq!(h.leaves(), vec![pane, surface]);
}

/// The surface tiles a placeholder naming its kind and title, and the
/// Terminal beside it paints into exactly the rect a Terminal neighbour
/// would leave it.
#[test]
fn a_surface_leaf_paints_a_placeholder_beside_an_unchanged_terminal() {
    let (pane, surface) = (tid(1), tid(4));
    let mut h = attached_to(&pane, Some(&surface));
    let layout = beside_layout(&pane, &surface);
    h.send(layout_changed(1, Some(encoded(&layout))));
    let screen = paint(&mut h);
    for text in [
        "terminal is usable",
        "Pixel surface",
        "Safari",
        crate::attach::surface_leaf::PLACEHOLDER_HINT,
    ] {
        assert!(screen.contains(text), "missing {text:?} in {screen:?}");
    }

    let content = crate::layout::Rect {
        x: 0,
        y: 0,
        w: 100,
        h: 30,
    };
    let tile = |doc: &ArrangementDoc, id: &ResourceId| {
        let active = doc.active_group().expect("window");
        crate::multi_pane::compute_layout_in(active, content, (100, 30))
            .rects
            .get(id)
            .copied()
    };
    let all_terminals = beside_layout(&pane, &tid(5));
    assert_eq!(tile(&layout, &pane), tile(&all_terminals, &pane));
    assert_eq!(tile(&layout, &surface), tile(&all_terminals, &tid(5)));
}

/// A session holding only a document is not a stale layout: its
/// placeholder shows instead of the empty state.
#[test]
fn a_document_only_layout_is_kept() {
    let doc = tid(3);
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(0), doc.clone())
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "docs")])
        .with_resources(vec![
            ResourceInfo::resource(doc.clone(), ResourceKind::Document)
                .with_session(Some(SessionId::new(1))),
        ]);
    let mut h = H::new();
    h.send(attached(snapshot, 1));
    h.send(FrameKind::AttachReady { attach_id: 1 });
    h.layout_request = Some(7);
    let read = h.send(FrameKind::MetadataValue {
        request_id: 7,
        value: Some(encoded(&ArrangementDoc::single(doc.clone()))),
    });
    assert!(read.layout_replaced && read.attach_panes.is_empty());
    assert_eq!(h.leaves(), vec![doc.clone()]);
    assert_eq!(h.mirror.focused_resource, Some(doc));
    assert!(h.mirror.panes.is_empty());
    assert!(paint(&mut h).contains("Document"));
}

/// Closing an arranged surface folds its leaf out like a pane, without a
/// pane-exit notice; the Terminal takes the whole window back.
#[test]
fn a_closed_surface_folds_its_leaf_out() {
    let (pane, surface) = (tid(1), tid(4));
    let mut h = attached_to(&pane, Some(&surface));
    h.send(layout_changed(
        1,
        Some(encoded(&beside_layout(&pane, &surface))),
    ));
    let outcome = h.send(closed(&surface, None));
    assert!(!outcome.exit && outcome.layout_replaced && outcome.reflow_panes);
    assert!(outcome.emit_set_metadata && outcome.notices.is_empty());
    assert_eq!(h.leaves(), vec![pane.clone()]);
    assert_eq!(h.mirror.focused_resource, Some(pane.clone()));
    assert!(h.mirror.panes.contains_key(&pane));
}

/// Rune opens a document after this client attached, then places it: the
/// spawn event names its kind, so the placement attaches nothing and the
/// placeholder paints at once. The spawn asks for the graph (its title).
#[test]
fn a_document_placed_live_is_a_placeholder_from_its_spawn_event() {
    let (pane, doc) = (tid(1), tid(6));
    let mut h = attached_to(&pane, None);
    let spawn = h.send(spawned(&doc, ResourceKind::Document, &pane));
    assert!(spawn.members_dirty && !spawn.layout_replaced);
    let placed = h.send(layout_changed(
        1,
        Some(encoded(&beside_layout(&pane, &doc))),
    ));
    assert!(placed.layout_replaced && placed.attach_panes.is_empty());
    assert!(paint(&mut h).contains("Document"));
}

/// The placement can beat the spawn event: the leaf is unknown, so it
/// tiles blank and is attached as a Terminal would be (an older server
/// streams it; a newer one refuses). The event then repaints it as a
/// placeholder.
#[test]
fn a_spawn_event_after_the_placement_repaints_the_leaf_as_a_placeholder() {
    let (pane, doc) = (tid(1), tid(7));
    let mut h = attached_to(&pane, None);
    let placed = h.send(layout_changed(
        1,
        Some(encoded(&beside_layout(&pane, &doc))),
    ));
    assert_eq!(placed.attach_panes, vec![doc.clone()]);
    assert!(!paint(&mut h).contains("Document"));
    let spawn = h.send(spawned(&doc, ResourceKind::Document, &pane));
    assert!(spawn.layout_replaced, "the arranged leaf repaints");
    assert!(paint(&mut h).contains("Document"));
}

/// Fallback for a server that streams documents to any client: a leaf the
/// graph never named is classified by its bootstrap, which repaints it.
#[test]
fn a_document_bootstrap_classifies_a_leaf_the_graph_did_not_name() {
    let (pane, doc) = (tid(1), tid(8));
    let mut h = attached_to(&pane, None);
    h.send(layout_changed(
        1,
        Some(encoded(&beside_layout(&pane, &doc))),
    ));
    let begin = h.send(FrameKind::BootstrapBegin {
        terminal_id: doc.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        profile: phux_protocol::BootstrapStreamProfile::DocumentTypedV1,
        cols: 0,
        rows: 0,
        base_seq: 1,
    });
    assert!(begin.layout_replaced, "the first word repaints the tile");
    assert!(paint(&mut h).contains("Document"));
    // The rest of the stream never reaches the TTY or repaints again. The
    // generation is already open, so send only what follows the begin.
    h.send(chunk_frame(
        &doc,
        b"\x1b]52;c;bm90LXR0eQ==\x07untrusted document",
    ));
    h.send(ready_frame(&doc));
    h.send(output_frame(&doc, 2, b"\x1b[2Juntrusted live document"));
    assert!(!h.out_str().contains("untrusted"));
}
