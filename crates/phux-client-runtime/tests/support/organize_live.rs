use super::*;
use phux_client_core::organize::{JudgeKind, NewSession, Plan, PlannedMove};
use phux_client_runtime::organize::{Report, Sequencer};

/// Feed every organization outcome to `run` until it reports.
async fn drive(client: &Client, run: &mut Sequencer, first: Option<Report>) -> Report {
    if let Some(report) = first {
        return report;
    }
    let mut idle = Instant::now();
    loop {
        let events = client.take_events();
        if !events.is_empty() {
            idle = Instant::now();
        }
        for event in events {
            if let Event::OrganizationOperation { operation, outcome } = event {
                let replica = client.organization();
                if let Some(report) = run.on_outcome(operation, &outcome, &replica, client) {
                    return report;
                }
            }
        }
        assert!(idle.elapsed() < DEADLINE, "timed out waiting for organize");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The desktop's organize path end to end: a plan that creates a session
/// and moves the seed terminal into it applies through the runtime lane,
/// and its journal undoes it, deleting the created session.
#[test]
fn a_plan_applies_and_undoes_through_the_runtime_lane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let (shutdown, server) = spawn_server(socket.clone(), Some("main"));
        let client = Runtime::connect(Target::uds(&socket), options()).unwrap();
        wait_for_status(&client, Status::Attached).await;
        wait_until("the organization seed", || {
            let replica = client.organization();
            replica.authority().is_some() && !replica.resources().is_empty()
        })
        .await;
        let replica = client.organization();
        let pane = replica.resources()[0].clone();
        let plan = Plan {
            judge: JudgeKind::Heuristic,
            creates: vec![NewSession {
                name: "web".into(),
                project_id: "github.com/o/web".into(),
                project_name: "web".into(),
                checkout: "/src/web".into(),
            }],
            moves: vec![PlannedMove {
                pane: pane.key,
                label: "@1".into(),
                from: pane.session,
                from_name: "main".into(),
                to: None,
                to_name: "web".into(),
                confidence: 1.0,
            }],
            renames: Vec::new(),
            kept: Vec::new(),
        };
        let mut run = Sequencer::apply(plan, replica.authority());
        let first = run.start(&replica, &client);
        let applied = drive(&client, &mut run, first).await;
        assert!(applied.failures.is_empty(), "{applied:?}");
        assert_eq!(applied.journal.created.len(), 1, "{applied:?}");
        assert_eq!(applied.journal.moved.len(), 1, "{applied:?}");
        let web = applied.journal.created[0].key;
        wait_until("the move in the replica", || {
            client.organization().resource(&pane.key).map(|r| r.session) == Some(web)
        })
        .await;
        let project = client
            .organization()
            .session(&web)
            .map(|s| s.attrs.clone())
            .unwrap_or_default();
        assert!(
            project
                .iter()
                .any(|(k, v)| k == "project.id" && v == "github.com/o/web"),
            "{project:?}"
        );

        let mut undo = Sequencer::undo(applied.journal);
        let first = undo.start(&client.organization(), &client);
        let undone = drive(&client, &mut undo, first).await;
        assert!(undone.failures.is_empty(), "{undone:?}");
        assert_eq!(undone.journal.moved.len(), 1, "{undone:?}");
        assert_eq!(undone.journal.created.len(), 1, "{undone:?}");
        wait_until("the session deleted", || {
            let replica = client.organization();
            replica.session(&web).is_none()
                && replica.resource(&pane.key).map(|r| r.session) == Some(pane.session)
        })
        .await;
        client.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}
