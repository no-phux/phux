use super::*;
use phux_client_runtime::{
    Runtime as ClientRuntime,
    control::{ControlOptions, ControlPlane, Status},
};
use phux_protocol::{
    PROTOCOL_VERSION,
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeatureExt, ServerFeatureExtSet,
    },
    ids::{
        ArrangementKey, AuthorityId, BootstrapId, ClientId, ResourceKey, ResourceKind, Revision,
        SessionId, StreamId, WindowId,
    },
    wire::{
        frame::{AttachTarget, OrganizationPage, ResourceRecord, SessionRecord},
        info::{ResourceInfo, SessionInfo, SessionSnapshot},
    },
};
use std::sync::Arc;

pub(super) fn key(n: u8) -> SessionKey {
    SessionKey::new([n; 16]).unwrap()
}
pub(super) fn resource_key(n: u8) -> ResourceKey {
    ResourceKey::new([n; 16]).unwrap()
}
pub(super) fn live(n: u32) -> ResourceId {
    ResourceId::local(n)
}

pub(super) fn outbound(client: &Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect()
}

/// Real protocol/control replicas, with an empty destination and a document sibling.
pub(super) fn connected() -> (Arc<Client>, Desk) {
    let client = Arc::new(ClientRuntime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("source".to_owned())),
        ..ControlOptions::default()
    }));
    client.with_control(ControlPlane::connection_opened);
    outbound(&client);
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
    let sent = outbound(&client);
    let attach_id = sent
        .iter()
        .find_map(|f| match f {
            FrameKind::Attach { attach_id, .. } => Some(*attach_id),
            _ => None,
        })
        .unwrap();
    let request_id = sent
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
            SessionRecord::new(key(1), "source", Revision::new(1))
                .with_live(Some(SessionId::new(1)))
                .with_members(2)
                .with_arrangement(Some(arrangement(1))),
            SessionRecord::new(key(2), "empty", Revision::new(1))
                .with_live(Some(SessionId::new(2)))
                .with_arrangement(Some(arrangement(2))),
        ])
        .with_resources(vec![
            ResourceRecord::new(resource_key(7), key(1), Revision::new(1))
                .with_kind(ResourceKind::Terminal)
                .with_live(Some(live(7))),
            ResourceRecord::new(resource_key(8), key(1), Revision::new(1))
                .with_kind(ResourceKind::AccessibilityTree)
                .with_live(Some(live(8))),
        ]);
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .unwrap();
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), live(7))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "source").with_key(Some(key(1))),
            SessionInfo::new(SessionId::new(2), "empty").with_key(Some(key(2))),
        ])
        .with_resources(vec![
            ResourceInfo::new(live(7), WindowId::new(1), 80, 24)
                .with_session(Some(SessionId::new(1))),
        ]);
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: live(7),
            stream_id: StreamId::new(7).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: live(7),
            stream_id: StreamId::new(7).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            chunk_seq: 0,
            payload: b"shell".to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: live(7),
            stream_id: StreamId::new(7).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            history_cursor: None,
        },
    ] {
        client.feed(frame).unwrap();
    }
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
    let mut desk = Desk::settings_test_fixture();
    desk.status = Status::Attached;
    desk.client = Some(client.clone());
    desk.topology = client.topology();
    desk.sidebar_open = true;
    desk.window_laid_out(1100.0);
    desk.reorder.sidebar_laid_out([0.0, 40.0, 200.0, 170.0]);
    desk.reorder
        .laid_out(Grab::Session(key(1)), [8.0, 40.0, 184.0, 25.0]);
    desk.reorder
        .laid_out(Grab::Resource(7, key(1)), [8.0, 66.0, 184.0, 30.0]);
    desk.reorder
        .laid_out(Grab::Resource(8, key(1)), [8.0, 97.0, 184.0, 30.0]);
    desk.reorder
        .laid_out(Grab::Session(key(2)), [8.0, 150.0, 184.0, 25.0]);
    let _ = client.take_activity();
    outbound(&client);
    (client, desk)
}

pub(super) fn move_request(client: &Client) -> (u32, OperationId) {
    outbound(client)
        .into_iter()
        .find_map(|f| match f {
            FrameKind::Command {
                request_id,
                command:
                    Command::ResourceMove {
                        operation,
                        resource,
                        from,
                        to,
                        ..
                    },
            } => {
                assert_eq!((resource, from, to), (resource_key(7), key(1), key(2)));
                Some((request_id, operation))
            }
            _ => None,
        })
        .expect("actual resource move sent to server")
}

pub(super) fn reply(client: &Client, desk: &mut Desk, request_id: u32, result: CommandResult) {
    client
        .feed(FrameKind::CommandResult { request_id, result })
        .unwrap();
    for event in client.take_activity().events {
        if let Event::OrganizationOperation { .. } = event {
            assert!(desk.apply(client, event, &mut rune::Tx::default()));
        }
    }
}

pub(super) fn arrangement(n: u8) -> ArrangementKey {
    ArrangementKey::new([n; 16]).unwrap()
}

pub(super) fn install_arrangement(
    client: &Client,
    n: u8,
    document: &phux_client_core::layout::ArrangementDoc,
    revision: u64,
) {
    use phux_client_core::layout::wire::{EntryBinding, HandleMap, to_protocol};
    use phux_protocol::wire::frame::{ArrangementOwner, ArrangementValue};
    let request_id = client.fetch_arrangement(arrangement(n)).unwrap();
    let handles = HandleMap::from_pairs([
        (resource_key(7), Some(live(7))),
        (resource_key(8), Some(live(8))),
    ]);
    let value = ArrangementValue::new(
        arrangement(n),
        ArrangementOwner::Session(key(n)),
        Revision::new(revision),
        to_protocol(document, &handles, &EntryBinding::default()).unwrap(),
    );
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::Arrangement(Box::new(value))),
        })
        .unwrap();
}

pub(super) fn advertise_membership(client: &Client, to: u8) {
    let request_id = client.refresh_organization().unwrap();
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(2), 0)
        .with_sessions(
            (1..=2)
                .map(|n| {
                    SessionRecord::new(
                        key(n),
                        if n == 1 { "source" } else { "empty" },
                        Revision::new(2),
                    )
                    .with_live(Some(SessionId::new(u32::from(n))))
                    .with_arrangement(Some(arrangement(n)))
                })
                .collect(),
        )
        .with_resources(vec![
            ResourceRecord::new(resource_key(7), key(to), Revision::new(1))
                .with_kind(ResourceKind::Terminal)
                .with_live(Some(live(7))),
            ResourceRecord::new(resource_key(8), key(to), Revision::new(1))
                .with_kind(ResourceKind::AccessibilityTree)
                .with_live(Some(live(8))),
        ]);
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .unwrap();
}
