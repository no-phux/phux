use super::*;
use crate::launch::Resolved;
use phux_client_core::layout::wire::{EntryBinding, HandleMap, to_protocol};
use phux_client_core::layout::{ArrangementDoc, Group, LayoutNode, LayoutState, SplitDir, leaves};
use phux_client_runtime::{
    Runtime as ClientRuntime,
    control::{ControlOptions, ControlPlane},
};
use phux_protocol::{
    PROTOCOL_VERSION,
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeatureExt, ServerFeatureExtSet,
    },
    ids::{
        ArrangementKey, AuthorityId, BootstrapId, ClientId, Revision, SessionId, SessionKey,
        StreamId, WindowId,
    },
    wire::{
        frame::{
            ArrangementOwner, ArrangementValue, AttachTarget, Command, CommandResult, CommandValue,
            FrameKind, OrganizationPage, SessionRecord,
        },
        info::{ResourceInfo, SessionInfo, SessionSnapshot},
    },
};
use rune::{Env, Key, KeyEvent, Mods, Runtime};

// A complete 4×2 RGBA PNG: left red, right blue. CRCs and IDAT are real,
// unlike the header-only fixtures used by some server protocol tests.
const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 4, 0, 0, 0, 2, 8, 6, 0,
    0, 0, 127, 168, 125, 99, 0, 0, 0, 20, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240, 31,
    132, 161, 212, 127, 6, 116, 1, 0, 7, 41, 15, 241, 247, 14, 147, 157, 0, 0, 0, 0, 73, 69, 78,
    68, 174, 66, 96, 130,
];

fn session() -> SessionKey {
    SessionKey::new([1; 16]).unwrap()
}
fn key(n: u8) -> ResourceKey {
    ResourceKey::new([n; 16]).unwrap()
}
fn id(n: u32) -> ResourceId {
    ResourceId::local(n)
}
fn arrangement() -> ArrangementKey {
    ArrangementKey::new([3; 16]).unwrap()
}

fn record(n: u8, kind: ResourceKind) -> ResourceRecord {
    ResourceRecord::new(key(n), session(), Revision::new(1))
        .with_kind(kind)
        .with_name(Some(format!("Mirror {n}")))
        .with_live(Some(id(u32::from(n))))
}

fn hello(server: u8) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new()
            .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization])),
        server_id: vec![server; 16],
        selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
        bootstrap_limits: BootstrapLimits::default(),
    }
}

fn outbound(client: &Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect()
}

fn seed(client: &Client, records: &[ResourceRecord]) -> Option<u32> {
    let sent = outbound(client);
    let attach = sent.iter().find_map(|frame| match frame {
        FrameKind::Attach { attach_id, .. } => Some(*attach_id),
        _ => None,
    });
    let request = sent
        .into_iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::GetOrganization { .. },
            } => Some(request_id),
            _ => None,
        })
        .expect("organization seed requested");
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(1), 0)
        .with_sessions(vec![
            SessionRecord::new(session(), "work", Revision::new(1))
                .with_members(records.len() as u32)
                .with_arrangement(Some(arrangement()))
                .with_live(Some(SessionId::new(1))),
        ])
        .with_resources(records.to_vec());
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .unwrap();
    attach
}

fn install_arrangement(client: &Client, records: &[ResourceRecord], doc: &ArrangementDoc) {
    let request = client.fetch_arrangement(arrangement()).unwrap();
    let map = HandleMap::from_pairs(records.iter().map(|r| (r.key, r.live.clone())));
    let document = to_protocol(doc, &map, &EntryBinding::default()).unwrap();
    let value = ArrangementValue::new(
        arrangement(),
        ArrangementOwner::Session(session()),
        Revision::new(1),
        document,
    );
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::Arrangement(Box::new(value))),
        })
        .unwrap();
}

fn fixture(records: &[ResourceRecord], doc: &ArrangementDoc) -> (Arc<Client>, Runtime<Desk>) {
    let client = Arc::new(ClientRuntime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("work".to_owned())),
        ..ControlOptions::default()
    }));
    client.with_control(ControlPlane::connection_opened);
    outbound(&client);
    client.feed(hello(1)).unwrap();
    let attach_id = seed(&client, records).expect("attach requested");
    install_arrangement(&client, records, doc);
    let snapshot = SessionSnapshot::new(
        SessionId::new(1),
        WindowId::new(1),
        records[0].live.clone().unwrap(),
    )
    .with_sessions(vec![
        SessionInfo::new(SessionId::new(1), "work").with_key(Some(session())),
    ])
    .with_resources(
        records
            .iter()
            .map(|r| {
                ResourceInfo::new(r.live.clone().unwrap(), WindowId::new(1), 80, 24)
                    .with_kind(r.kind)
                    .with_session(Some(SessionId::new(1)))
            })
            .collect(),
    );
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    for record in records.iter().filter(|r| r.kind == ResourceKind::Terminal) {
        publish(
            &client,
            record.live.as_ref().unwrap().local_id().unwrap(),
            ResourceKind::Terminal,
            b"terminal sibling",
        );
    }
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
    let mut desk = Desk::new(&Resolved::offline("fixture only"));
    desk.link.heard = true; // no first-run preferences write
    desk.client = Some(Arc::clone(&client));
    desk.resources.images = Some(rune::ImageCache::new(Arc::new(|| {})));
    let mut rt = Runtime::new(desk, crate::ui::view, (800.0, 600.0), Env::new(true, 1.0));
    rt.on_key = Some(Desk::key);
    step(&mut rt);
    (client, rt)
}

fn step(rt: &mut Runtime<Desk>) {
    rt.update(|d, tx| {
        d.poll(1.0, tx);
    });
    rt.frame(1.0);
    rt.frame(1.0);
}

fn doc(tree: LayoutNode) -> ArrangementDoc {
    ArrangementDoc {
        groups: vec![Group::new(
            "mirrors".to_owned(),
            LayoutState {
                tree: Some(tree),
                ..LayoutState::default()
            },
        )],
        active: 0,
    }
}

fn split(a: u32, b: u32) -> LayoutNode {
    LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(LayoutNode::Leaf(id(a))),
        right: Box::new(LayoutNode::Leaf(id(b))),
    }
}

fn publish(client: &Client, n: u32, kind: ResourceKind, bytes: &[u8]) {
    let profile = match kind {
        ResourceKind::PixelSurface => BootstrapStreamProfile::PixelSurfacePngV1,
        ResourceKind::Terminal => BootstrapStreamProfile::SynthesizedVtRaw,
        _ => BootstrapStreamProfile::AccessibilityTreeJsonV1,
    };
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: id(n),
            stream_id: StreamId::new(u64::from(n)).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            profile,
            cols: if kind == ResourceKind::Terminal {
                80
            } else {
                0
            },
            rows: if kind == ResourceKind::Terminal {
                24
            } else {
                0
            },
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: id(n),
            stream_id: StreamId::new(u64::from(n)).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            chunk_seq: 0,
            payload: bytes.to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: id(n),
            stream_id: StreamId::new(u64::from(n)).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            history_cursor: None,
        },
    ] {
        client.feed(frame).unwrap();
    }
}

fn remove(client: &Client, n: u8, kind: ResourceKind) {
    client
        .feed(FrameKind::Event {
            terminal: None,
            stamp: Some(Box::new(phux_protocol::wire::frame::EventStamp::new(1, 0))),
            event: phux_protocol::wire::frame::AgentEvent::ResourceRemoved {
                resource: key(n),
                session: session(),
                kind,
            },
        })
        .unwrap();
}

#[test]
fn document_only_startup_adopts_org_identity_secondary_leaves_and_tabs() {
    let records = [
        record(1, ResourceKind::AccessibilityTree),
        record(2, ResourceKind::PixelSurface),
        record(3, ResourceKind::AccessibilityTree),
    ];
    let mut arrangement_doc = doc(split(1, 2));
    arrangement_doc.add_group("tree".to_owned(), id(3));
    arrangement_doc.active = 0;
    let (client, mut rt) = fixture(&records, &arrangement_doc);
    assert!(
        client.topology().unwrap().panes.is_empty(),
        "Topology stays terminal-only"
    );
    assert_eq!(rt.state.shown.as_ref().unwrap().terminal, id(1));
    assert!(rt.state.shown.as_ref().unwrap().view.is_none());
    assert_eq!(rt.state.others.get(&id(2)), Some(&None));
    assert_eq!(leaves(rt.state.layout.as_ref().unwrap()), [id(1), id(2)]);
    assert_eq!(rt.state.tabs.list.len(), 2);
    rt.update(|d, _| d.focus_pane(&id(2)));
    assert!(rt.state.shown.as_ref().unwrap().view.is_none());
    assert_eq!(rt.state.others.get(&id(1)), Some(&None));
    rt.update(|d, _| d.select_tab(1));
    step(&mut rt);
    assert_eq!(rt.state.shown.as_ref().unwrap().terminal, id(3));
    assert!(
        rt.state
            .palette_hits()
            .iter()
            .any(|h| h.item.kind == "document")
    );
}

#[test]
fn real_wire_json_updates_replace_plain_native_inspector_and_empty_errors() {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::AccessibilityTree)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    assert!(matches!(
        rt.state.resources.entries[&id(1)].body,
        Body::Pending
    ));
    publish(
        &client,
        1,
        ResourceKind::AccessibilityTree,
        br#"{"title":"<script>not HTML</script>","children":[]}"#,
    );
    step(&mut rt);
    assert!(
        matches!(&rt.state.resources.entries[&id(1)].body, Body::Tree(json) if json.contains("<script>not HTML</script>"))
    );
    for (seq, bytes) in [(1, b"{\"new\":2}".as_slice()), (2, b""), (3, b"not JSON")] {
        client
            .feed(FrameKind::ResourceOutput {
                terminal_id: id(1),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                bytes: bytes.to_vec().into(),
            })
            .unwrap();
        step(&mut rt);
        assert_eq!(rt.state.resources.entries[&id(1)].seq, Some(seq));
        if seq == 1 {
            assert!(
                matches!(&rt.state.resources.entries[&id(1)].body, Body::Tree(json) if json.contains("new"))
            );
        }
        if seq == 2 {
            assert!(matches!(
                rt.state.resources.entries[&id(1)].body,
                Body::Empty
            ));
        }
    }
    assert!(
        matches!(&rt.state.resources.entries[&id(1)].body, Body::Invalid(error) if error.contains("JSON"))
    );
}

#[test]
fn document_focus_cannot_send_terminal_input_upload_history_resize_or_paint_ack() {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::AccessibilityTree)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    outbound(&client);
    rt.key(KeyEvent {
        key: Key::Text("x".into()),
        mods: Mods::default(),
    });
    rt.update(|d, tx| {
        d.ime(&rune::Ime::Commit("typed".into()));
        d.paste(&id(1), "unsafe\n");
        d.paste_clipboard(tx);
        d.drop_files(&id(1), &["/tmp/file".into()]);
        d.shortcut(crate::keys::Shortcut::ScrollTop, tx);
        d.open_find();
        d.laid_out(id(1), [0.0, 0.0, 800.0, 600.0]);
        d.drawn();
    });
    assert!(rt.state.pending.is_none());
    assert!(rt.state.find.is_none());
    assert!(rt.state.preedit.is_none());
    assert!(rt.state.painted.borrow().is_none());
    assert!(
        outbound(&client).is_empty(),
        "no terminal commands for a document"
    );
}

#[test]
fn png_uses_real_decoder_and_reports_truncated_png() {
    let (decoded, wake) = std::sync::mpsc::channel();
    let cache = rune::ImageCache::new(Arc::new(move || {
        let _ = decoded.send(());
    }));
    let mut d = Document::new(record(1, ResourceKind::PixelSurface));
    let mut decodes = surface::Decodes::default();
    let mut publication = DocumentPublication {
        resource_id: id(1),
        kind: ResourceKind::PixelSurface,
        seq: 0,
        bytes: PNG.to_vec(),
        content_type: None,
    };
    assert!(d.publish(&publication, Some(&cache), &mut decodes));
    let Body::Surface(image) = &d.body else {
        panic!("image")
    };
    assert_eq!(image.wait(), Ok([4, 2]));
    wake.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    assert!(
        !d.publish(&publication, Some(&cache), &mut decodes),
        "stale sequence ignored"
    );
    publication.seq = 1;
    publication.bytes = PNG[..24].to_vec();
    assert!(d.publish(&publication, Some(&cache), &mut decodes));
    // Retire the completed first decode, then decode the queued next cut.
    d.finish_surface(Some(&cache), &mut decodes);
    assert!(d.surface.flight.as_ref().unwrap().image.wait().is_err());
    wake.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    d.finish_surface(Some(&cache), &mut decodes);
    assert!(matches!(&d.body, Body::Invalid(error) if error.contains("PNG")));
}

#[test]
fn a_jpeg_frame_uses_the_real_decoder() {
    const JPEG: &[u8] = include_bytes!("../../../../../crates/phux-core/testdata/baseline-5x3.jpg");
    let cache = rune::ImageCache::new(Arc::new(|| {}));
    let mut d = Document::new(record(1, ResourceKind::PixelSurface));
    let publication = DocumentPublication {
        resource_id: id(1),
        kind: ResourceKind::PixelSurface,
        seq: 0,
        bytes: JPEG.to_vec(),
        content_type: None,
    };
    assert!(d.publish(&publication, Some(&cache), &mut surface::Decodes::default()));
    let Body::Surface(image) = &d.body else {
        panic!("image")
    };
    assert_eq!(image.wait(), Ok([5, 3]));
}

#[test]
fn organization_removal_prunes_documents_and_hands_focus_to_live_neighbour() {
    let records = [
        record(1, ResourceKind::AccessibilityTree),
        record(2, ResourceKind::PixelSurface),
    ];
    let (client, mut rt) = fixture(&records, &doc(split(1, 2)));
    remove(&client, 1, ResourceKind::AccessibilityTree);
    step(&mut rt);
    assert!(!rt.state.resources.entries.contains_key(&id(1)));
    assert_eq!(rt.state.shown.as_ref().unwrap().terminal, id(2));
    assert_eq!(leaves(rt.state.layout.as_ref().unwrap()), [id(2)]);
    assert!(rt.state.others.is_empty());
    assert!(
        rt.state
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("Mirror unavailable")
    );
}

#[test]
fn closing_a_mirror_view_preserves_unrelated_tabs_and_never_ends_its_producer() {
    let mut arrangement_doc = doc(split(1, 2));
    arrangement_doc.add_group("unrelated".to_owned(), id(3));
    remove_document_leaves(&mut arrangement_doc, &[id(1)]);
    assert_eq!(arrangement_doc.groups.len(), 2);
    assert_eq!(
        leaves(arrangement_doc.groups[0].state.tree.as_ref().unwrap()),
        [id(2)]
    );
    assert_eq!(
        leaves(arrangement_doc.groups[1].state.tree.as_ref().unwrap()),
        [id(3)]
    );
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::AccessibilityTree)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    outbound(&client);
    rt.update(|d, _| d.close_terminal());
    assert!(rt.state.shown.is_none());
    assert!(
        rt.state.resources.entries.contains_key(&id(1)),
        "source still exists"
    );
    let frames = outbound(&client);
    assert!(!frames.is_empty(), "arrangement edit queued");
    assert!(
        frames
            .iter()
            .all(|f| matches!(f, FrameKind::Command { .. }))
    );
    rt.update(|d, _| d.show(id(1), "work"));
    assert_eq!(
        rt.state.shown.as_ref().unwrap().terminal,
        id(1),
        "direct reopen is still available"
    );
}

#[test]
fn mirror_startup_requires_confirmation_and_uses_dedicated_argv_not_shell_input() {
    let (client, mut rt) = fixture(
        &[record(1, ResourceKind::AccessibilityTree)],
        &doc(LayoutNode::Leaf(id(1))),
    );
    rt.state.launch.phux = Some("/dev/phux".into());
    rt.state.launch.socket = Ok("/dev/socket".into());
    outbound(&client);
    rt.update(|d, _| d.ask_mirror(Source::Page, "first;echo no".to_owned()));
    assert!(
        outbound(&client).is_empty(),
        "asking does not start a process"
    );
    assert!(matches!(
        rt.state.pending,
        Some(crate::desk::Pending::Mirror(..))
    ));
    rt.update(|d, _| d.confirm_pending());
    let frames = outbound(&client);
    assert_eq!(frames.len(), 1);
    let FrameKind::SpawnResource {
        command: Some(argv),
        ..
    } = &frames[0]
    else {
        panic!("dedicated producer terminal")
    };
    assert_eq!(
        argv,
        &[
            "/dev/phux",
            "--socket",
            "/dev/socket",
            "agent",
            "page",
            "mirror",
            "--",
            "first;echo no"
        ]
    );
    rt.state.resources.producers.insert(id(7), Source::Page);
    assert!(rt.state.producer_ended(&id(7), Some(1)));
    assert!(
        rt.state
            .notice
            .as_ref()
            .unwrap()
            .text
            .contains("localhost:9222")
    );
    assert!(
        !rt.state.producer_ended(&id(7), Some(1)),
        "one failure notice per producer"
    );
}

#[test]
fn mixed_layout_keeps_terminal_view_and_readonly_secondary_document() {
    let records = [
        record(1, ResourceKind::Terminal),
        record(2, ResourceKind::AccessibilityTree),
    ];
    let (client, mut rt) = fixture(&records, &doc(split(1, 2)));
    step(&mut rt);
    assert!(rt.state.shown.as_ref().unwrap().view.is_some());
    assert_eq!(rt.state.others.get(&id(2)), Some(&None));
    rt.update(|d, _| d.focus_pane(&id(2)));
    assert!(rt.state.shown.as_ref().unwrap().view.is_none());
    assert!(rt.state.others[&id(1)].is_some());
    remove(&client, 2, ResourceKind::AccessibilityTree);
    step(&mut rt);
    assert_eq!(rt.state.shown.as_ref().unwrap().terminal, id(1));
    assert!(rt.state.shown.as_ref().unwrap().view.is_some());
}

mod native;
mod receipts;
mod review;
mod streaming;
