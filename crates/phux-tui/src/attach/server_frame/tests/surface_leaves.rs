//! Non-Terminal layout leaves (docs/resource-kinds.md): another client puts
//! a document or surface into the session's arrangement, and the TUI adopts
//! it as a placeholder tile instead of refusing the attach.

use super::resource_kinds::document;
use super::*;

/// A Terminal (`pane`) and a titled pixel surface (`surface`) in session 1.
fn surface_snapshot(pane: &ResourceId, surface: &ResourceId) -> SessionSnapshot {
    let (window, session) = (WindowId::new(1), SessionId::new(1));
    SessionSnapshot::new(session, window, pane.clone())
        .with_sessions(vec![SessionInfo::new(session, "work".to_owned())])
        .with_windows(vec![WindowInfo::new(window, session, "w0".to_owned())])
        .with_resources(vec![
            ResourceInfo::new(pane.clone(), window, 100, 30),
            ResourceInfo::resource(surface.clone(), ResourceKind::PixelSurface)
                .with_session(Some(session))
                .with_title(Some("Safari".to_owned())),
        ])
}

/// Attached to [`surface_snapshot`] with both bootstraps delivered.
fn attached_with_surface(pane: &ResourceId, surface: &ResourceId) -> H {
    let mut h = H::new().with_viewport((100, 30));
    h.send(attached(surface_snapshot(pane, surface), 1));
    h.send(begin_frame(pane));
    h.send(chunk_frame(pane, b"terminal is usable"));
    h.send(ready_frame(pane));
    document(
        &mut h,
        surface,
        phux_protocol::BootstrapStreamProfile::PixelSurfacePngV1,
    );
    h.send(FrameKind::AttachReady { attach_id: 1 });
    h
}

/// `pane` beside `other`, focused on `pane`.
fn beside_layout(pane: &ResourceId, other: &ResourceId) -> ArrangementDoc {
    let mut layout = ArrangementDoc::single(pane.clone());
    beside(&mut layout, other);
    layout
}

/// A full-frame paint of the active window, CSI stripped.
fn paint(h: &mut H, resources: &[ResourceInfo]) -> String {
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
        resources,
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
/// surface: no refusal, no Terminal attach for it, focus kept on the pane.
#[test]
fn a_layout_naming_a_surface_is_adopted_not_refused() {
    let (pane, surface) = (tid(1), tid(4));
    let mut h = attached_with_surface(&pane, &surface);
    let layout = beside_layout(&pane, &surface)
        .encode_cbor()
        .expect("encode");
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
    let mut h = attached_with_surface(&pane, &surface);
    let layout = beside_layout(&pane, &surface);
    h.send(layout_changed(
        1,
        Some(layout.encode_cbor().expect("encode")),
    ));
    let resources = surface_snapshot(&pane, &surface).resources;
    let screen = paint(&mut h, &resources);
    for text in [
        "terminal is usable",
        "Pixel surface",
        "Safari",
        crate::attach::surface_leaf::PLACEHOLDER_HINT,
    ] {
        assert!(screen.contains(text), "missing {text:?} in {screen:?}");
    }
    assert!(!screen.contains("untrusted"), "document bytes never paint");

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
    document(
        &mut h,
        &doc,
        phux_protocol::BootstrapStreamProfile::DocumentTypedV1,
    );
    h.send(FrameKind::AttachReady { attach_id: 1 });
    h.layout_request = Some(7);
    let read = h.send(FrameKind::MetadataValue {
        request_id: 7,
        value: Some(
            ArrangementDoc::single(doc.clone())
                .encode_cbor()
                .expect("encode"),
        ),
    });
    assert!(read.layout_replaced && read.attach_panes.is_empty());
    assert_eq!(h.leaves(), vec![doc.clone()]);
    assert_eq!(h.mirror.focused_resource, Some(doc));
    assert!(h.mirror.panes.is_empty());
    assert!(paint(&mut h, &[]).contains("Document"));
}

/// Closing an arranged surface folds its leaf out like a pane, without a
/// pane-exit notice; the Terminal takes the whole window back.
#[test]
fn a_closed_surface_folds_its_leaf_out() {
    let (pane, surface) = (tid(1), tid(4));
    let mut h = attached_with_surface(&pane, &surface);
    let layout = beside_layout(&pane, &surface)
        .encode_cbor()
        .expect("encode");
    h.send(layout_changed(1, Some(layout)));
    let outcome = h.send(closed(&surface, None));
    assert!(!outcome.exit && outcome.layout_replaced && outcome.reflow_panes);
    assert!(outcome.emit_set_metadata && outcome.notices.is_empty());
    assert_eq!(h.leaves(), vec![pane.clone()]);
    assert_eq!(h.mirror.focused_resource, Some(pane.clone()));
    assert!(h.mirror.panes.contains_key(&pane));
}
