use super::*;
use crate::attach::render::ReplicaWalk;

/// The hub's unreachable wording names a host, and only
/// that host's panes go grey. A later return clears them for replay.
#[test]
fn satellite_unreachable_marks_one_host_and_a_return_clears_it() {
    let local = ResourceId::local(1);
    let edge = ResourceId::satellite("edge", 9);
    let other = ResourceId::satellite("gpubox", 3);
    let mut panes = HashMap::from([
        (local.clone(), PaneSlot::new().expect("slot")),
        (edge.clone(), PaneSlot::new().expect("slot")),
        (other.clone(), PaneSlot::new().expect("slot")),
    ]);
    assert_eq!(
        satellite_host_in_unreachable("satellite edge is unreachable: link is down"),
        Some("edge")
    );
    assert!(satellite_host_in_unreachable("the link dropped").is_none());
    assert!(note_satellite_unreachable(
        &mut panes,
        "satellite edge is unreachable: link is down"
    ));
    assert!(pane_satellite_down(&panes, &edge));
    assert!(!pane_satellite_down(&panes, &local));
    assert!(!pane_satellite_down(&panes, &other));
    assert_eq!(
        satellite_panes_returned(&mut panes, "edge"),
        vec![edge.clone()]
    );
    assert!(!pane_satellite_down(&panes, &edge));
}

/// Every `SatelliteUnreachable` wording the hub emits names its host;
/// other prose names none.
#[test]
fn every_hub_unreachable_shape_names_its_host() {
    for message in [
        "satellite edge is unreachable: link is down",
        "satellite edge is unreachable",
        "satellite edge link is down",
        "satellite edge link dropped before the reply",
        "satellite edge link dropped before the spawn reply",
        "satellite edge did not answer within 30s",
        "satellite edge did not answer the spawn within 30s",
        "satellite edge proxy withdrawal cancelled before application after 30s; \
             subscription preserved",
    ] {
        assert_eq!(
            satellite_host_in_unreachable(message),
            Some("edge"),
            "{message}"
        );
    }
    for message in [
        "the link dropped",
        "satellite edge",
        "satellite  is unreachable",
        "satellite edge link is saturated; retry",
        "satellite edge has no such terminal",
        "offline",
    ] {
        assert_eq!(satellite_host_in_unreachable(message), None, "{message}");
    }
}

#[test]
fn first_live_kitty_render_restores_a_visible_image() {
    let mut slot = PaneSlot::new_with_size(10, 5).expect("slot");
    slot.terminal
        .vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=77,q=2;/wAA/w==\x1b\\");
    let mut out = Vec::new();
    slot.renderer
        .render(ReplicaWalk::for_test(&slot.terminal), &mut out)
        .expect("render");
    let mut receiver = PaneSlot::new_with_size(10, 5).expect("receiver");
    receiver.terminal.vt_write(&out);
    let graphics = receiver.terminal.kitty_graphics().unwrap();
    let image = graphics.image(77).expect("replayed image");
    assert_eq!(image.data().unwrap().unwrap(), &[255, 0, 0, 255]);
    let mut iter = libghostty_vt::kitty::graphics::PlacementIterator::new().unwrap();
    let mut placements = iter.update(&graphics).unwrap();
    let placement = placements.next().expect("visible placement");
    let size = placement.pixel_size(&image, &receiver.terminal).unwrap();
    assert_eq!((size.width, size.height), (1, 1));
    assert_eq!(
        placement
            .viewport_pos(&image, &receiver.terminal)
            .unwrap()
            .map(|p| (p.col, p.row)),
        Some((0, 0))
    );
    assert!(placements.next().is_none());
}

#[test]
fn a_fresh_input_origin_badges_the_label_until_it_expires() {
    let id = ResourceId::local(1);
    let mut slot = PaneSlot::new().expect("slot");
    slot.last_title = "shell".to_owned();
    let started = Instant::now();
    slot.note_input_origin("ada".to_owned(), started);
    let mut panes = HashMap::from([(id.clone(), slot)]);
    let (kernel, _effects, _panes) = published_test_state(&[]);
    let label = pane_label(&kernel, &panes, &id).expect("label");
    let origin = label.origin.expect("badge");
    assert_eq!(origin.text, "ada");
    assert!(!origin.dim);
    panes.get_mut(&id).expect("slot").input_origin = Some(InputOrigin {
        label: "ada".to_owned(),
        since: Instant::now()
            .checked_sub(INPUT_ORIGIN_LIFE)
            .expect("test clock"),
    });
    assert!(
        pane_label(&kernel, &panes, &id)
            .expect("label")
            .origin
            .is_none()
    );
}

/// A republish at unchanged geometry must serve fresh rows through an
/// incremental paint: the kernel's new token reaches the renderer. (The
/// deterministic guard lives in `phux_protocol::render_pool::tests`.)
#[test]
fn republish_at_same_geometry_serves_fresh_rows_without_force_full() {
    use phux_client_core::engine::CanonicalGeometry;
    use phux_client_core::session::KernelInput;
    use phux_protocol::{BootstrapId, BootstrapStreamProfile, StreamId};

    let id = ResourceId::local(1);
    let (mut kernel, mut effects, mut panes) = published_test_state(&[(&id, 10, 2, b"AA")]);
    let slot = panes.get_mut(&id).expect("slot");

    let walk_1 = published_replica(&kernel, &id).expect("generation 1");
    let generation_1 = walk_1.generation;
    let mut first = Vec::new();
    let _ = slot
        .renderer
        .render_at(walk_1, &mut first, (0, 0), (10, 2))
        .expect("paint generation 1");
    assert!(
        String::from_utf8_lossy(&first).contains("AA"),
        "first incremental paint serves generation 1's rows"
    );

    // A second bootstrap generation, same geometry, new content.
    let stream_id = StreamId::new(1).expect("stream");
    let bootstrap_id = BootstrapId::new(2).expect("bootstrap 2");
    kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id: &id,
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                geometry: CanonicalGeometry::new(10, 2).expect("geometry"),
                base_seq: 0,
            },
            &mut effects,
        )
        .expect("republish BEGIN");
    kernel
        .update(
            KernelInput::BootstrapChunk {
                terminal_id: &id,
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: b"ZZ",
            },
            &mut effects,
        )
        .expect("republish CHUNK");
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            &mut effects,
        )
        .expect("republish READY");

    let walk_2 = published_replica(&kernel, &id).expect("generation 2");
    let generation_2 = walk_2.generation;
    assert_ne!(
        generation_1, generation_2,
        "a republish must change the walk-identity token"
    );

    // Another walk consumes the fresh replica's dirty state first.
    let mut thief = libghostty_vt::RenderState::new().expect("thief state");
    let _ = thief.update(walk_2.terminal).expect("thief update");

    let mut second = Vec::new();
    let _ = slot
        .renderer
        .render_at(walk_2, &mut second, (0, 0), (10, 2))
        .expect("paint generation 2");
    let painted = String::from_utf8_lossy(&second);
    assert!(
        painted.contains("ZZ"),
        "an incremental paint (no force_full) after a same-geometry \
             republish must serve the new generation's rows, got {painted:?}"
    );
}

/// The driver holds exactly one client-local origin. A
/// second attention jump cannot overwrite it, and return consumes it.
#[test]
fn attention_navigation_saves_once_and_consumes() {
    let mut navigation = AttentionNavigation::default();
    navigation.save_origin_once(Some(&ResourceId::local(1)));
    navigation.save_origin_once(Some(&ResourceId::local(2)));
    assert_eq!(navigation.take_origin(), Some(ResourceId::local(1)));
    assert_eq!(navigation.take_origin(), None);
}

/// Input clears the asked flag once; repeats and unknown panes report
/// false.
#[test]
fn clear_attention_on_input_clears_once() {
    let id = ResourceId::local(1);
    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    let mut slot = PaneSlot::new_with_size(80, 24).expect("slot");
    slot.attention = true;
    panes.insert(id.clone(), slot);

    assert!(clear_attention_on_input(&mut panes, &id), "first clear");
    assert!(
        !panes.get(&id).expect("slot").attention,
        "flag must be down after the clear"
    );
    assert!(
        !clear_attention_on_input(&mut panes, &id),
        "already-clear pane reports no transition"
    );
    assert!(
        !clear_attention_on_input(&mut panes, &ResourceId::local(9)),
        "unknown pane reports no transition"
    );
}
