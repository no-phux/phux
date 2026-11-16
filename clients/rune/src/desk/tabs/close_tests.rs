use std::sync::Arc;

use super::*;
use phux_client_runtime::Runtime;
use phux_client_runtime::control::{ControlOptions, ControlPlane, Status, Topology};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet,
};
use phux_protocol::ids::{AuthorityId, Revision, SessionId, WindowId};
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, CommandValue, FrameKind, OrganizationPage, SessionRecord,
};
use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};

use crate::launch::Resolved;

fn key(n: u8) -> SessionKey {
    SessionKey::new([n; 16]).unwrap()
}

fn sent(client: &Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect()
}

/// A real browsing connection with SESSION_KILL, two empty sessions and a
/// seeded organization journal. No process or socket.
fn fixture() -> (Arc<Client>, Desk) {
    let client = Arc::new(Runtime::embedded(ControlOptions::default()));
    client.with_control(ControlPlane::connection_opened);
    let _ = sent(&client);
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new().with_features_ext(ServerFeatureExtSet::with(&[
                ServerFeatureExt::Organization,
                ServerFeatureExt::SessionKill,
            ])),
            server_id: vec![1; 16],
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(0))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "selected"),
            SessionInfo::new(SessionId::new(2), "empty"),
        ]);
    let page = OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(1), 0)
        .with_sessions(vec![
            SessionRecord::new(key(1), "selected", Revision::new(1))
                .with_live(Some(SessionId::new(1))),
            SessionRecord::new(key(2), "empty", Revision::new(1))
                .with_live(Some(SessionId::new(2))),
        ]);
    for frame in sent(&client) {
        let FrameKind::Command {
            request_id,
            command,
        } = frame
        else {
            continue;
        };
        let result = match command {
            Command::GetState { .. } => {
                CommandResult::OkWith(CommandValue::State(snapshot.clone()))
            }
            Command::GetOrganization { .. } => {
                CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page.clone())))
            }
            _ => continue,
        };
        client
            .feed(FrameKind::CommandResult { request_id, result })
            .unwrap();
    }
    client.attach_session(AttachTarget::ByName("selected".into()));
    assert_eq!(client.selected_session(), Some(1));
    let mut desk = Desk::new(&Resolved::offline("session close"));
    desk.prefs = rune::prefs::Prefs::memory();
    desk.connection.client = Some(client.clone());
    desk.connection.status = Status::Attached;
    desk.connection.topology = Some(Topology::from_snapshot(&snapshot));
    for session in &mut desk.connection.topology.as_mut().unwrap().sessions {
        session.key = Some(key(session.id as u8));
    }
    let _ = sent(&client);
    (client, desk)
}

fn killed(client: &Client) -> Vec<SessionKey> {
    sent(client)
        .into_iter()
        .map(|frame| match frame {
            FrameKind::Command {
                command: Command::SessionKill { session, .. },
                ..
            } => session,
            other => panic!("closing must only send SESSION_KILL: {other:?}"),
        })
        .collect()
}

#[test]
fn a_nonselected_empty_session_closes_by_key_without_selecting_it() {
    let (client, mut desk) = fixture();
    desk.connection.topology.as_mut().unwrap().sessions[1].name = "renamed empty".into();
    desk.close_session_key(key(2));
    assert_eq!(
        desk.pending,
        Some(Pending::CloseSession(key(2), "renamed empty".into()))
    );
    assert_eq!(client.selected_session(), Some(1));
    assert!(sent(&client).is_empty(), "asking sends nothing");

    desk.cancel_pending();
    assert!(desk.pending.is_none());
    assert!(sent(&client).is_empty(), "cancel leaves the session alone");
    assert!(
        desk.connection
            .topology
            .as_ref()
            .unwrap()
            .session_keyed(&key(2))
            .is_some()
    );

    desk.close_session_key(key(2));
    desk.confirm_pending();
    assert_eq!(killed(&client), [key(2)]);
    assert_eq!(client.selected_session(), Some(1));
}

#[test]
fn stale_session_controls_do_not_close_a_reused_name_or_handle() {
    let (client, mut desk) = fixture();
    // The same sidebar position, live id and name now belong to a new
    // session. A cached control must not ask about that replacement.
    desk.connection.topology.as_mut().unwrap().sessions[1].key = Some(key(3));
    desk.close_session_key(key(2));
    assert!(desk.pending.is_none());
    desk.confirm_pending();
    assert!(sent(&client).is_empty());
    assert_eq!(client.selected_session(), Some(1));

    desk.close_session();
    let question = Some(Pending::CloseSession(key(1), "selected".into()));
    assert_eq!(desk.pending, question);
    desk.close_session_key(key(2));
    assert_eq!(
        desk.pending, question,
        "stale controls leave a held question alone"
    );
    // Selection and the target's name can change while confirmation is
    // held; the accepted operation still addresses the original key.
    client.attach_session(AttachTarget::ByName("empty".into()));
    desk.connection.topology.as_mut().unwrap().sessions[0].name = "renamed".into();
    let _ = sent(&client);
    desk.confirm_pending();
    assert_eq!(killed(&client), [key(1)]);
    assert_eq!(client.selected_session(), Some(2));
}

#[test]
fn closing_selected_empty_sessions_never_recreates_their_home() {
    let (client, mut desk) = fixture();
    client.attach_session(AttachTarget::ByName("empty".into()));
    let _ = sent(&client);
    desk.settle_home(&client);
    desk.close_session();
    assert_eq!(
        desk.pending,
        Some(Pending::CloseSession(key(2), "empty".into()))
    );
    desk.confirm_pending();
    assert_eq!(killed(&client), [key(2)]);

    desk.connection
        .topology
        .as_mut()
        .unwrap()
        .sessions
        .retain(|s| s.key != Some(key(2)));
    assert!(desk.settle_home(&client));
    assert_eq!(
        client.selected_session(),
        Some(1),
        "use the remaining session"
    );
    let _ = sent(&client);
    desk.settle_home(&client);
    desk.close_session();
    desk.confirm_pending();
    assert_eq!(killed(&client), [key(1)]);

    desk.connection.topology.as_mut().unwrap().sessions.clear();
    assert!(!desk.settle_home(&client));
    assert!(
        sent(&client).is_empty(),
        "the last session is not created again"
    );
}
