//! Held verbs beyond `?signal`: a held `TERMINATE` opens an approval like a
//! held kill, runs only after approval, and never runs when denied.

use super::*;

fn session_kill(state: &SharedState, name: &str, operation: u8) -> Command {
    let session = state.with(|s| {
        let id = s.registry().session_by_name(name).unwrap();
        crate::org_wire::session_key(s.registry().session_key(id).unwrap()).unwrap()
    });
    Command::SessionKill {
        operation: phux_protocol::ids::OperationId::new([operation; 16]).unwrap(),
        session,
    }
}

fn held_terminate_world() -> World {
    world(|_, _| {
        vec![
            (
                REQUESTER,
                vec!["inventory,observe,?terminate@global".to_owned()],
            ),
            (APPROVER, vec!["inventory,terminate@global".to_owned()]),
        ]
    })
}

#[tokio::test(flavor = "current_thread")]
async fn a_held_terminate_runs_only_after_approval_and_never_after_denial() {
    LocalSet::new()
        .run_until(async {
            let world = held_terminate_world();
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;

            // Denied: the approval entry names the session; nothing runs.
            let kill = session_kill(&world.state, "beta", 1);
            requester.send(&command(HELD, kill.clone()));
            let id = the_pending_approval(&world.state).await;
            let entry = record(&world.state, id).expect("the approval entry is written");
            assert_eq!(entry["method"], "SESSION_KILL");
            let Command::SessionKill { session, .. } = &kill else {
                unreachable!()
            };
            assert_eq!(
                entry["subjects"],
                serde_json::json!([format!("session:{session}")])
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(requester.results(HELD).is_empty(), "the result is deferred");
            approver.send(&decision(1, id, APPROVAL_DENY));
            let denied = requester.result(HELD).await;
            assert!(is_denied(&denied, "approval denied"), "{denied:?}");
            assert!(
                world
                    .state
                    .with(|s| s.registry().session_by_name("beta").is_some()),
                "a denied kill never runs"
            );

            // Approved: it runs once, as the requester.
            let kill = session_kill(&world.state, "beta", 2);
            requester.send(&command(HELD + 1, kill));
            let id = the_pending_approval(&world.state).await;
            assert!(
                world
                    .state
                    .with(|s| s.registry().session_by_name("beta").is_some())
            );
            approver.send(&decision(2, id, APPROVAL_APPROVE));
            let ran = requester.result(HELD + 1).await;
            assert!(
                !matches!(ran, CommandResult::Error { .. } | CommandResult::Refused(_)),
                "the approved kill ran: {ran:?}"
            );
            assert!(
                until_state(&world.state, |s| s
                    .registry()
                    .session_by_name("beta")
                    .is_none())
                .await,
                "the session is gone"
            );

            requester.close().await;
            approver.close().await;
        })
        .await;
}
