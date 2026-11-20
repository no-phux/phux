//! A terminal the selected session lists but never streamed here, as one
//! moved in from another session is (#2223): showing it asks for its stream.

use std::sync::Arc;

use phux_client_runtime::Runtime;
use phux_client_runtime::control::{ControlOptions, ControlPlane, SessionRecovery};
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{Command, CommandValue, FrameKind};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

use crate::desk::Desk;
use crate::launch::Resolved;
use crate::wire_fixture::{answer, outbound as sent};

#[test]
fn showing_a_terminal_nothing_streams_asks_for_it() {
    let client = Arc::new(Runtime::embedded(ControlOptions {
        session_recovery: SessionRecovery::CallerValidated,
        ..ControlOptions::default()
    }));
    let mut desk = Desk::with_preferences(
        &Resolved::offline("unstreamed"),
        None,
        rune::prefs::Prefs::memory(),
    );
    desk.connection.client = Some(Arc::clone(&client));
    let mut rt = rune::Runtime::new(
        desk,
        crate::ui::view,
        (1100.0, 720.0),
        rune::Env::new(true, 2.0),
    );
    let step = |rt: &mut rune::Runtime<Desk>| {
        rt.update(|desk, tx| {
            desk.poll(1.0, tx);
        });
        rt.frame(1.0 / 60.0);
    };

    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(crate::wire_fixture::hello(1, false))
        .expect("hello");
    let inventory = sent(&client).iter().find_map(|frame| match frame {
        FrameKind::Command {
            request_id,
            command: Command::GetState { .. },
        } => Some(*request_id),
        _ => None,
    });
    let (session, window) = (SessionId::new(1), WindowId::new(1));
    let (one, two) = (ResourceId::local(1), ResourceId::local(2));
    let inventory_of = |terminals: &[&ResourceId]| {
        let shell =
            WindowInfo::new(window, session, "shell").with_active_resource(Some(one.clone()));
        SessionSnapshot::new(session, window, one.clone())
            .with_sessions(vec![SessionInfo::new(session, "work")])
            .with_windows(vec![shell])
            .with_resources(
                terminals
                    .iter()
                    .map(|id| ResourceInfo::new((*id).clone(), window, 80, 24))
                    .collect(),
            )
    };
    answer(
        &client,
        inventory.expect("an inventory"),
        CommandValue::State(inventory_of(&[&one])),
    );
    step(&mut rt);
    // The server lists a second terminal, but never streams it here.
    let refresh = client.refresh_topology().expect("connected");
    answer(
        &client,
        refresh,
        CommandValue::State(inventory_of(&[&one, &two])),
    );
    step(&mut rt);
    let attaches = |client: &phux_client_runtime::Client| -> Vec<ResourceId> {
        sent(client)
            .into_iter()
            .filter_map(|frame| match frame {
                FrameKind::Command {
                    command: Command::AttachResource { terminal_id, .. },
                    ..
                } => Some(terminal_id),
                _ => None,
            })
            .collect()
    };
    assert!(!attaches(&client).contains(&two), "listed, never streamed");
    rt.update(|desk, _| desk.show(two.clone(), "work"));
    step(&mut rt);
    assert_eq!(
        attaches(&client),
        std::slice::from_ref(&two),
        "showing it asks for it"
    );
    step(&mut rt);
    assert!(attaches(&client).is_empty(), "asked once, not every drain");
}
