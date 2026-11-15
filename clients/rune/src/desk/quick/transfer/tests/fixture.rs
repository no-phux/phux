use super::*;
use phux_client_runtime::{
    Runtime as ClientRuntime,
    control::{ControlOptions, ControlPlane},
};
use phux_protocol::{
    ids::{ArrangementKey, AuthorityId, ResourceKey, ResourceKind, Revision, WindowId},
    wire::{
        frame::{
            Command, CommandValue, FrameKind, OrganizationPage, ResourceRecord, SessionRecord,
        },
        info::{ResourceInfo, SessionInfo, SessionSnapshot},
    },
};

pub(super) use crate::wire_fixture::outbound;
use crate::wire_fixture::{answer, attach_id, hello, organization_request, request};

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
    client.feed(hello(1, true)).unwrap();
    let sent = outbound(&client);
    let attach_id = attach_id(&sent).unwrap();
    let request_id = organization_request(&sent).unwrap();
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
    answer(
        &client,
        request_id,
        CommandValue::OrganizationPage(Box::new(page)),
    );
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal())
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "work").with_key(Some(session(1))),
            SessionInfo::new(SessionId::new(2), "quick").with_key(Some(session(2))),
        ])
        .with_resources(vec![
            ResourceInfo::new(terminal(), WindowId::new(1), 80, 24)
                .with_session(Some(SessionId::new(2))),
        ]);
    crate::wire_fixture::attached(&client, attach_id, snapshot);
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
    let request = request(&outbound(client), |c| {
        matches!(c, Command::ResourceMove { .. })
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
    answer(
        client,
        request_id,
        CommandValue::Arrangement(Box::new(value)),
    );
}
