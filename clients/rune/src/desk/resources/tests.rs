use super::*;
use crate::launch::Resolved;
use phux_client_core::layout::wire::{EntryBinding, HandleMap, to_protocol};
use phux_client_core::layout::{ArrangementDoc, Group, LayoutNode, LayoutState, SplitDir, leaves};
use phux_client_runtime::{
    Runtime as ClientRuntime,
    control::{ControlOptions, ControlPlane},
};
use phux_protocol::{
    caps::BootstrapStreamProfile,
    ids::{
        ArrangementKey, AuthorityId, BootstrapId, ResourceKind, Revision, SessionId, SessionKey,
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

use crate::wire_fixture::{answer, attach_id, bootstrap_frames, organization_request, outbound};
mod decoding;

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

/// Records `1..=N`, each of its kind in `kinds`.
fn mirrors<const N: usize>(kinds: [ResourceKind; N]) -> [ResourceRecord; N] {
    let mut n = 0;
    kinds.map(|kind| {
        n += 1;
        record(n, kind)
    })
}

fn hello(server: u8) -> FrameKind {
    crate::wire_fixture::hello(server, true)
}

fn seed(client: &Client, records: &[ResourceRecord]) -> Option<u32> {
    let sent = outbound(client);
    let request = organization_request(&sent).expect("organization seed requested");
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(1), 0)
        .with_sessions(vec![
            SessionRecord::new(session(), "work", Revision::new(1))
                .with_members(records.len() as u32)
                .with_arrangement(Some(arrangement()))
                .with_live(Some(SessionId::new(1))),
        ])
        .with_resources(records.to_vec());
    answer(
        client,
        request,
        CommandValue::OrganizationPage(Box::new(page)),
    );
    attach_id(&sent)
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
    answer(client, request, CommandValue::Arrangement(Box::new(value)));
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
    crate::wire_fixture::attached(&client, attach_id, snapshot);
    for record in records.iter().filter(|r| r.kind == ResourceKind::Terminal) {
        publish(
            &client,
            record.live.as_ref().unwrap().local_id().unwrap(),
            ResourceKind::Terminal,
            b"terminal sibling",
        );
    }
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
    let mut desk = Desk::with_preferences(
        &Resolved::offline("fixture only"),
        None,
        rune::prefs::Prefs::memory(),
    );
    desk.link.heard = true; // no first-run preferences write
    desk.connection.client = Some(Arc::clone(&client));
    desk.connection.resources.images = Some(rune::ImageCache::new(Arc::new(|| {})));
    let mut rt =
        crate::test_support::runtime(desk, crate::ui::view, (800.0, 600.0), Env::new(true, 1.0));
    rt.on_key = Some(Desk::key);
    step(&mut rt);
    (client, rt)
}

/// A [`fixture`] showing one accessibility-tree document, `@1`.
fn one_document() -> (Arc<Client>, Runtime<Desk>) {
    fixture(
        &[record(1, ResourceKind::AccessibilityTree)],
        &doc(LayoutNode::Leaf(id(1))),
    )
}

/// [`publish`] `json` as document `@1`'s accessibility tree.
fn publish_tree(client: &Client, json: &[u8]) {
    publish(client, 1, ResourceKind::AccessibilityTree, json);
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
        slots: Vec::new(),
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
    publish_on(client, n, StreamId::new(u64::from(n)).unwrap(), kind, bytes);
}

/// [`publish`] on `stream_id`: a reattach's cut rides a new stream, as the
/// server's does, since a generation is never bootstrapped twice.
fn publish_on(client: &Client, n: u32, stream_id: StreamId, kind: ResourceKind, bytes: &[u8]) {
    let profile = match kind {
        ResourceKind::PixelSurface => BootstrapStreamProfile::PixelSurfacePngV1,
        ResourceKind::Terminal => BootstrapStreamProfile::SynthesizedVtRaw,
        _ => BootstrapStreamProfile::AccessibilityTreeJsonV1,
    };
    let size = if kind == ResourceKind::Terminal {
        [80, 24]
    } else {
        [0, 0]
    };
    for frame in bootstrap_frames(&id(n), [stream_id.get(), 1], profile, size, bytes) {
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

/// Attach `attach_id` with a snapshot of `records`, the first in front.
fn attach_records(client: &Client, attach_id: u32, records: &[ResourceRecord]) {
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), id(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(session())),
        ])
        .with_resources(
            records
                .iter()
                .map(|r| {
                    ResourceInfo::resource(r.live.clone().unwrap(), r.kind)
                        .with_session(Some(SessionId::new(1)))
                })
                .collect(),
        );
    crate::wire_fixture::attached(client, attach_id, snapshot);
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
    assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, id(1));
    assert!(rt.state.connection.shown.as_ref().unwrap().view.is_none());
    assert_eq!(rt.state.connection.others.get(&id(2)), Some(&None));
    assert_eq!(
        leaves(rt.state.connection.layout.as_ref().unwrap()),
        [id(1), id(2)]
    );
    assert_eq!(rt.state.connection.tabs.list.len(), 2);
    rt.update(|d, _| d.focus_pane(&id(2)));
    assert!(rt.state.connection.shown.as_ref().unwrap().view.is_none());
    assert_eq!(rt.state.connection.others.get(&id(1)), Some(&None));
    rt.update(|d, _| d.select_tab(1));
    step(&mut rt);
    assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, id(3));
    assert!(
        rt.state
            .palette_hits()
            .iter()
            .any(|h| h.item.kind == "document")
    );
}

#[test]
fn real_wire_json_updates_replace_plain_native_inspector_and_empty_errors() {
    let (client, mut rt) = one_document();
    assert!(matches!(
        rt.state.connection.resources.entries[&id(1)].body,
        Body::Pending
    ));
    publish_tree(
        &client,
        br#"{"title":"<script>not HTML</script>","children":[]}"#,
    );
    step(&mut rt);
    assert!(
        matches!(&rt.state.connection.resources.entries[&id(1)].body, Body::Tree(json) if json.contains("<script>not HTML</script>"))
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
        assert_eq!(rt.state.connection.resources.entries[&id(1)].seq, Some(seq));
        if seq == 1 {
            assert!(
                matches!(&rt.state.connection.resources.entries[&id(1)].body, Body::Tree(json) if json.contains("new"))
            );
        }
        if seq == 2 {
            assert!(matches!(
                rt.state.connection.resources.entries[&id(1)].body,
                Body::Empty
            ));
        }
    }
    assert!(
        matches!(&rt.state.connection.resources.entries[&id(1)].body, Body::Invalid(error) if error.contains("JSON"))
    );
}

#[test]
fn document_focus_cannot_send_terminal_input_upload_history_resize_or_paint_ack() {
    let (client, mut rt) = one_document();
    outbound(&client);
    rt.key(crate::keys::plain(Key::Text("x".into())));
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
    assert!(rt.state.connection.find.is_none());
    assert!(rt.state.connection.preedit.is_none());
    assert!(rt.state.connection.painted.borrow().is_none());
    assert!(
        outbound(&client).is_empty(),
        "no terminal commands for a document"
    );
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
    assert!(!rt.state.connection.resources.entries.contains_key(&id(1)));
    assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, id(2));
    assert_eq!(
        leaves(rt.state.connection.layout.as_ref().unwrap()),
        [id(2)]
    );
    assert!(rt.state.connection.others.is_empty());
    assert!(crate::test_support::notice(&rt.state).contains("Mirror unavailable"));
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
    let (client, mut rt) = one_document();
    outbound(&client);
    rt.update(|d, _| d.close_terminal());
    assert!(rt.state.connection.shown.is_none());
    assert!(
        rt.state.connection.resources.entries.contains_key(&id(1)),
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
        rt.state.connection.shown.as_ref().unwrap().resource,
        id(1),
        "direct reopen is still available"
    );
}

#[test]
fn mirror_startup_requires_confirmation_and_uses_dedicated_argv_not_shell_input() {
    let (client, mut rt) = one_document();
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
    rt.state
        .connection
        .resources
        .producers
        .insert(id(7), Source::Page);
    assert!(rt.state.producer_ended(&id(7), Some(1)));
    assert!(crate::test_support::notice(&rt.state).contains("localhost:9222"));
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
    assert!(rt.state.connection.shown.as_ref().unwrap().view.is_some());
    assert_eq!(rt.state.connection.others.get(&id(2)), Some(&None));
    rt.update(|d, _| d.focus_pane(&id(2)));
    assert!(rt.state.connection.shown.as_ref().unwrap().view.is_none());
    assert!(rt.state.connection.others[&id(1)].is_some());
    remove(&client, 2, ResourceKind::AccessibilityTree);
    step(&mut rt);
    assert_eq!(rt.state.connection.shown.as_ref().unwrap().resource, id(1));
    assert!(rt.state.connection.shown.as_ref().unwrap().view.is_some());
}

mod addressable;
mod input;
mod lifecycle;
mod native;
mod receipts;
mod review;
mod satellite;
mod slots;
mod streaming;
mod typed;
