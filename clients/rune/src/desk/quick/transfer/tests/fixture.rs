use super::*;
use phux_client_runtime::{
    Runtime as ClientRuntime,
    control::{ControlOptions, ControlPlane},
};
use phux_protocol::{
    PROTOCOL_VERSION,
    caps::{
        BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeatureExt,
        ServerFeatureExtSet,
    },
    ids::{ArrangementKey, AuthorityId, ClientId, ResourceKey, ResourceKind, Revision, WindowId},
    wire::{
        frame::{
            Command, CommandValue, FrameKind, OrganizationPage, ResourceRecord, SessionRecord,
        },
        info::{ResourceInfo, SessionInfo, SessionSnapshot},
    },
};

pub(super) fn outbound(client: &Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect()
}

pub(super) fn session(n: u8) -> SessionKey {
    SessionKey::new([n; 16]).unwrap()
}

/// Embedded protocol driver only: no sockets, desktop, shortcuts or processes.
pub(super) fn connected() -> (Arc<Client>, Desk) {
    let client = Arc::new(ClientRuntime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("work".to_owned())),
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
        .find_map(|frame| match frame {
            FrameKind::Attach { attach_id, .. } => Some(*attach_id),
            _ => None,
        })
        .unwrap();
    let request_id = sent
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::GetOrganization { .. },
            } => Some(*request_id),
            _ => None,
        })
        .unwrap();
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(1), 0)
        .with_sessions(vec![
            SessionRecord::new(session(1), "work", Revision::new(1))
                .with_live(Some(SessionId::new(1)))
                .with_arrangement(Some(arrangement(1))),
            SessionRecord::new(session(2), "quick", Revision::new(1))
                .with_live(Some(SessionId::new(2)))
                .with_arrangement(Some(arrangement(2)))
                .with_members(1),
        ])
        .with_resources(vec![
            ResourceRecord::new(
                ResourceKey::new([7; 16]).unwrap(),
                session(2),
                Revision::new(1),
            )
            .with_kind(ResourceKind::Terminal)
            .with_live(Some(terminal())),
        ]);
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page))),
        })
        .unwrap();
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal())
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(session(1))),
            SessionInfo::new(SessionId::new(2), "quick").with_key(Some(session(2))),
        ])
        .with_resources(vec![
            ResourceInfo::new(terminal(), WindowId::new(1), 80, 24)
                .with_session(Some(SessionId::new(2))),
        ]);
    client
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    client.feed(FrameKind::AttachReady { attach_id }).unwrap();
    let mut desk = Desk::settings_test_fixture();
    desk.status = Status::Attached;
    desk.topology = client.topology();
    desk.client = Some(client.clone());
    let _ = client.take_activity();
    outbound(&client);
    (client, desk)
}

pub(super) fn begin(
    desk: &mut Desk,
    client: &Client,
) -> (u32, OperationId, Receiver<Result<(), String>>) {
    let (finish, result) = mpsc::channel();
    desk.adopt_quick(
        terminal(),
        Box::new(move |outcome| finish.send(outcome).unwrap()),
    );
    let operation = desk.quick_move.as_ref().expect("move queued").operation;
    let request = outbound(client)
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::ResourceMove { .. },
            } => Some(*request_id),
            _ => None,
        })
        .expect("move submitted over the protocol");
    (request, operation, result)
}

pub(super) fn reply(desk: &mut Desk, client: &Client, request_id: u32, result: CommandResult) {
    client
        .feed(FrameKind::CommandResult { request_id, result })
        .unwrap();
    for event in client.take_activity().events {
        if let Event::OrganizationOperation { operation, outcome } = event {
            assert!(desk.apply(
                client,
                Event::OrganizationOperation { operation, outcome },
                &mut rune::Tx::default()
            ));
        }
    }
}

pub(super) fn terminal() -> ResourceId {
    ResourceId::local(7)
}

pub(super) fn arrangement(n: u8) -> ArrangementKey {
    ArrangementKey::new([n; 16]).unwrap()
}

pub(super) fn install_arrangement(client: &Client, n: u8) {
    use phux_client_core::layout::{
        ArrangementDoc,
        wire::{EntryBinding, HandleMap, to_protocol},
    };
    use phux_protocol::wire::frame::{ArrangementOwner, ArrangementValue};
    let request_id = client.fetch_arrangement(arrangement(n)).unwrap();
    let document = to_protocol(
        &ArrangementDoc::default(),
        &HandleMap::new(),
        &EntryBinding::default(),
    )
    .unwrap();
    let value = ArrangementValue::new(
        arrangement(n),
        ArrangementOwner::Session(session(n)),
        Revision::new(1),
        document,
    );
    client
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::Arrangement(Box::new(value))),
        })
        .unwrap();
}
