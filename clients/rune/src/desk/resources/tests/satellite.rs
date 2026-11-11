//! A hub lists its satellites' documents only in its inventory: the hub's
//! organization holds none of their records. Rune still finds, shows and
//! streams them, without attaching a hub session for them, and its sidebar
//! files them under their satellite session.

use super::*;
use rune::accesskit::Action;

fn satellite_frames(id: &ResourceId, bytes: &[u8]) -> [FrameKind; 3] {
    let stream_id = StreamId::new(70).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    [
        FrameKind::BootstrapBegin {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::PixelSurfacePngV1,
            cols: 0,
            rows: 0,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes.to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ]
}

#[test]
fn a_satellite_surface_on_a_hub_is_listed_shown_and_streamed() {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::Terminal)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    let producer = ResourceId::satellite("box", 69);
    let surface = ResourceId::satellite("box", 70);
    // The hub's GET_STATE: its own terminal, then the satellite's rows,
    // which belong to no hub session (#2051).
    let request = client.refresh_topology().unwrap();
    outbound(&client);
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), id(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(session())),
        ])
        .with_hosts(vec![phux_protocol::wire::info::HostInventory::reachable(
            "box".into(),
            vec![
                // Members name every kind the session holds (#2158).
                phux_protocol::wire::info::HostSessionInfo::new(SessionId::new(2), "lab")
                    .with_members(vec![producer.clone(), surface.clone()]),
            ],
        )])
        .with_resources(vec![
            ResourceInfo::new(id(1), WindowId::new(1), 80, 24)
                .with_session(Some(SessionId::new(1))),
            ResourceInfo::new(producer.clone(), WindowId::new(0), 80, 24),
            ResourceInfo::new(surface.clone(), WindowId::new(0), 0, 0)
                .with_kind(ResourceKind::PixelSurface)
                .with_parent(Some(producer))
                .with_title(Some("display".to_owned()))
                .with_control(Some(phux_protocol::wire::frame::ControlClass::Display)),
        ]);
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        })
        .unwrap();
    step(&mut rt);
    assert!(rt.state.is_document(&surface), "the surface is a document");
    let hit = rt
        .state
        .palette_hits()
        .into_iter()
        .find(|h| matches!(&h.item.target, crate::palette::Target::Terminal { id, .. } if *id == surface))
        .expect("the satellite's surface is in the palette");
    assert_eq!(hit.item.kind, "document");
    assert_eq!(hit.item.title, "display");
    assert!(hit.item.detail.starts_with("box · "), "{}", hit.item.detail);
    // Its producer declared a control class through the hub's listing.
    assert!(
        hit.item.detail.ends_with("Live mirror"),
        "{}",
        hit.item.detail
    );

    outbound(&client);
    rt.frame(1.0);
    let texts: Vec<_> = rt.elements().into_iter().filter_map(|el| el.text).collect();
    assert!(texts.iter().any(|t| t == "box · lab"), "{texts:?}");
    assert!(rt.find("pane-box/@69").is_some(), "its terminal");
    assert!(rt.find("pane-box/@70").is_some(), "its mirror");
    assert!(
        !texts.iter().any(|t| t == "box/@70"),
        "a document member is not also drawn as a terminal row: {texts:?}"
    );
    assert!(rt.accessibility_action("pane-box/@70", Action::Click, None));
    step(&mut rt);
    let sent = outbound(&client);
    assert!(
        !sent.iter().any(|f| matches!(f, FrameKind::Attach { .. })),
        "no hub session is attached for a satellite's resource: {sent:?}"
    );
    let subscribe = sent
        .iter()
        .find_map(|f| match f {
            FrameKind::Command {
                request_id,
                command: Command::AttachResource { terminal_id, .. },
            } if *terminal_id == surface => Some(*request_id),
            _ => None,
        })
        .expect("the surface is subscribed through the hub");
    client
        .feed(FrameKind::CommandResult {
            request_id: subscribe,
            result: CommandResult::Ok,
        })
        .unwrap();
    for frame in satellite_frames(&surface, PNG) {
        client.feed(frame).unwrap();
    }
    step(&mut rt);
    assert_eq!(rt.state.shown.as_ref().unwrap().terminal, surface);
    let Body::Surface(image) = &rt.state.resources.entries[&surface].body else {
        panic!("the satellite's image reached the mirror");
    };
    assert_eq!(image.wait(), Ok([4, 2]));
    step(&mut rt);
    assert_eq!(rt.state.document_input_frame(&surface), Some([4, 2]));
    rt.frame(1.0);
    let texts: Vec<_> = rt.elements().into_iter().filter_map(|el| el.text).collect();
    assert!(
        texts.iter().any(|t| t.contains("Live mirror"))
            && !texts.iter().any(|t| t.contains("Read-only")),
        "{texts:?}"
    );
}
