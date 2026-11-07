//! Embedded protocol fixture with real organization, arrangement, terminal
//! replicas and a document sibling. No server and no manufactured view IDs.

use phux_client_core::layout::wire::{EntryBinding, HandleMap, to_protocol};
use phux_client_core::layout::{ArrangementDoc, Group, LayoutState};
use phux_client_runtime::{
    Client, Runtime as ClientRuntime,
    control::{ControlOptions, ControlPlane},
};
use phux_protocol::{
    PROTOCOL_VERSION, ResourceKind,
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeatureExt, ServerFeatureExtSet,
    },
    ids::{
        ArrangementKey, AuthorityId, BootstrapId, ClientId, ResourceKey, Revision, SessionId,
        SessionKey, StreamId, WindowId,
    },
    wire::{
        frame::{
            ArrangementOwner, ArrangementValue, AttachTarget, Command, CommandResult, CommandValue,
            FrameKind, OrganizationPage, ResourceRecord, SessionRecord,
        },
        info::{ResourceInfo, SessionInfo, SessionSnapshot},
    },
};
use rune::prefs::Prefs;
use rune::{Env, Runtime};
use std::sync::Arc;

use super::*;
use crate::launch::Resolved;

fn session() -> SessionKey {
    SessionKey::new([1; 16]).unwrap()
}
fn arrangement() -> ArrangementKey {
    ArrangementKey::new([3; 16]).unwrap()
}

pub fn outbound(client: &Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|b| FrameKind::decode(b).unwrap().0)
        .collect()
}

pub fn doc() -> ArrangementDoc {
    let mut doc = ArrangementDoc {
        groups: vec![Group::new(
            "spatial".into(),
            LayoutState {
                tree: Some(tree()),
                ..LayoutState::default()
            },
        )],
        active: 0,
    };
    doc.add_group("other".into(), id(5));
    doc.active = 0;
    doc
}

pub fn stored(client: &Client) -> ArrangementDoc {
    client
        .organization()
        .arrangement_document(&session())
        .unwrap()
        .2
}

fn records() -> Vec<ResourceRecord> {
    (1..=6)
        .map(|n| {
            ResourceRecord::new(
                ResourceKey::new([n; 16]).unwrap(),
                session(),
                Revision::new(1),
            )
            .with_live(Some(id(u32::from(n))))
            .with_kind(if n == 4 || n == 6 {
                ResourceKind::AccessibilityTree
            } else {
                ResourceKind::Terminal
            })
            .with_name(Some(match n {
                1 => "Build".into(),
                2 => "Tests".into(),
                3 => "Logs".into(),
                _ => format!("Pane {n}"),
            }))
        })
        .collect()
}

pub fn install(client: &Client, doc: &ArrangementDoc, revision: u64) {
    let request = client.fetch_arrangement(arrangement()).unwrap();
    let records = records();
    let map = HandleMap::from_pairs(records.iter().map(|r| (r.key, r.live.clone())));
    let value = ArrangementValue::new(
        arrangement(),
        ArrangementOwner::Session(session()),
        Revision::new(revision),
        to_protocol(doc, &map, &EntryBinding::default()).unwrap(),
    );
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::Arrangement(Box::new(value))),
        })
        .unwrap();
}

fn seed(client: &Client) -> u32 {
    let sent = outbound(client);
    let attach = sent
        .iter()
        .find_map(|f| match f {
            FrameKind::Attach { attach_id, .. } => Some(*attach_id),
            _ => None,
        })
        .unwrap();
    let request = sent
        .iter()
        .find_map(|f| match f {
            FrameKind::Command {
                request_id,
                command: Command::GetOrganization { .. },
            } => Some(*request_id),
            _ => None,
        })
        .unwrap();
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(1), 0)
        .with_sessions(vec![
            SessionRecord::new(session(), "work", Revision::new(1))
                .with_members(6)
                .with_arrangement(Some(arrangement()))
                .with_live(Some(SessionId::new(1))),
        ])
        .with_resources(records());
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .unwrap();
    attach
}

fn boot(client: &Client) {
    client.with_control(ControlPlane::connection_opened);
    outbound(client);
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization])),
            server_id: vec![1; 16],
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    let attach_id = seed(client);
    install(client, &doc(), 1);
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), id(1))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(session())),
        ])
        .with_resources(
            records()
                .iter()
                .map(|r| {
                    ResourceInfo::new(r.live.clone().unwrap(), WindowId::new(1), 80, 24)
                        .with_kind(r.kind)
                        .with_session(Some(SessionId::new(1)))
                        .with_title(Some(
                            match r.live.as_ref().unwrap().local_id().unwrap() {
                                1 => "Build",
                                2 => "Tests",
                                3 => "Logs",
                                _ => "Shell",
                            }
                            .to_owned(),
                        ))
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
    for record in records()
        .iter()
        .filter(|r| r.kind == ResourceKind::Terminal)
    {
        let n = record.live.as_ref().unwrap().local_id().unwrap();
        let payload = (0..100)
            .map(|line| format!("Pane {n} history line {line}\r\n"))
            .collect::<String>();
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: id(n),
                stream_id: StreamId::new(u64::from(n)).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: id(n),
                stream_id: StreamId::new(u64::from(n)).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                chunk_seq: 0,
                payload: payload.into_bytes().into(),
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
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
}

pub fn fixture(phone: bool) -> (Arc<Client>, Runtime<Desk>) {
    let client = Arc::new(ClientRuntime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("work".into())),
        ..ControlOptions::default()
    }));
    boot(&client);
    let mut desk = Desk::new(&Resolved::offline("spatial fixture")).with_phone(phone);
    desk.prefs = Prefs::memory();
    desk.link.heard = true;
    desk.client = Some(Arc::clone(&client));
    let mut rt = Runtime::new(desk, crate::ui::view, (393.0, 852.0), Env::new(true, 2.0));
    rt.on_key = Some(Desk::key);
    poll(&mut rt);
    settle(&mut rt);
    outbound(&client);
    (client, rt)
}

pub fn poll(rt: &mut Runtime<Desk>) {
    rt.update(|d, tx| {
        d.poll(1.0, tx);
    });
    rt.frame(1.0 / 60.0);
}

pub fn settle(rt: &mut Runtime<Desk>) {
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
}

pub fn focus(rt: &Runtime<Desk>) -> ResourceId {
    rt.state.shown.as_ref().unwrap().terminal.clone()
}

pub fn geometry(rt: &Runtime<Desk>, key: &str) -> rune::ElementInfo {
    rt.elements()
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .unwrap_or_else(|| panic!("no {key}"))
}

pub fn tap(rt: &mut Runtime<Desk>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no {key}"));
    touch(rt, rune::TouchPhase::Started, x, y);
    touch(rt, rune::TouchPhase::Ended, x, y);
}

pub fn touch(rt: &mut Runtime<Desk>, phase: rune::TouchPhase, x: f32, y: f32) {
    rt.touch(1, phase, x, y, 1.0);
    rt.frame(1.0 / 60.0);
}

pub fn no_input(client: &Client) {
    for frame in outbound(client) {
        assert!(
            !matches!(
                frame,
                FrameKind::InputKey { .. }
                    | FrameKind::InputPaste { .. }
                    | FrameKind::InputMouse { .. }
            ),
            "navigation sent {frame:?}"
        );
    }
}

pub fn output(client: &Client, n: u32, seq: u64, bytes: &[u8]) {
    client
        .feed(FrameKind::ResourceOutput {
            terminal_id: id(n),
            stream_id: StreamId::new(u64::from(n)).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: bytes.to_vec().into(),
        })
        .unwrap();
}

pub fn remove_document(client: &Client, n: u8) {
    client
        .feed(FrameKind::Event {
            terminal: None,
            stamp: Some(Box::new(phux_protocol::wire::frame::EventStamp::new(1, 0))),
            event: phux_protocol::wire::frame::AgentEvent::ResourceRemoved {
                resource: ResourceKey::new([n; 16]).unwrap(),
                session: session(),
                kind: ResourceKind::AccessibilityTree,
            },
        })
        .unwrap();
}
