//! L1 §5.2: a kill's reply follows its commit, and the commit is the whole
//! teardown any observer sees (phux-t8nn). Each victim traps `SIGHUP`, so its
//! process outlives the reply by the full pane-kill grace; before the fix
//! `GET_STATE` kept listing it, as `running`, until the reap.
//!
//! No sleeps: every assertion is a read sent after the kill's reply, which
//! the in-order frame loop answers from the committed state.

use phux_protocol::ids::{ResourceId, ServerInstance};
use phux_protocol::wire::frame::{
    AgentEvent, CloseReason, Command, CommandResult, FrameKind, KillConditions, KillPrecondition,
    SpawnResource, SpawnResult, StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;
use phux_server_testkit::{Spawn, attach_by_name, command, send_frame, spawn_resource};
use tokio::net::UnixStream;

use crate::common::{Server, create, find, session, state, subscribe, wait_frame};

const ANCHOR: &str = "anchor";

/// Survives the hangup, so only the grace's `SIGKILL` ends it.
const HANGUP_PROOF: &str = "trap '' HUP; while :; do sleep 1; done";

fn hangup_proof() -> Spawn {
    Spawn::command(&["/bin/sh", "-c", HANGUP_PROOF])
}

/// Create session `name` seeded with a hangup-proof pane; return the pane.
async fn doomed_session(stream: &mut UnixStream, request_id: u32, name: &str) -> ResourceId {
    let body = serde_json::json!({
        "name": name,
        "command": ["/bin/sh", "-c", HANGUP_PROOF],
    });
    let result = create(stream, request_id, body).await;
    let id = result["terminal_id"].as_u64().expect("a seeded create");
    ResourceId::local(u32::try_from(id).unwrap())
}

/// Spawn a hangup-proof pane bound to this incarnation.
async fn doomed_pane(stream: &mut UnixStream, request_id: u32) -> (ResourceId, ServerInstance) {
    let spawn = Spawn {
        resource: Some(Box::new(SpawnResource::default().with_bind_instance(true))),
        ..hangup_proof()
    };
    match spawn_resource(stream, request_id, spawn).await {
        SpawnResult::OkBound { id, instance } => (id, instance),
        other => panic!("spawn failed: {other:?}"),
    }
}

/// `pane` is in no resource list and no window of `snapshot`.
fn assert_gone(snapshot: &SessionSnapshot, pane: &ResourceId, what: &str) {
    assert!(
        find(snapshot, pane).is_none(),
        "{what}: GET_STATE after the reply still lists {pane:?}: {:?}",
        snapshot.resources
    );
}

/// Every kill verb's reply is the teardown: the very next `GET_STATE`, on
/// the killer's connection and on another, lists neither the pane nor the
/// session it emptied, and an `ATTACH` to that session is refused.
#[test]
fn get_state_after_any_kill_reply_omits_the_killed_resources() {
    phux_server_testkit::run_local(async {
        let server = Server::pty(Some(ANCHOR));
        let mut killer = server.connect().await;
        let mut observer = server.connect().await;

        let pane = doomed_session(&mut killer, 1, "doomed").await;
        let before = state(&mut observer, 2).await;
        assert!(find(&before, &pane).is_some() && session(&before, "doomed").is_some());
        let kill_all = Command::KillResources {
            ids: vec![pane.clone()],
            operation_id: None,
        };
        assert_eq!(command(&mut killer, 3, kill_all).await, CommandResult::Ok);
        for (stream, request_id) in [(&mut killer, 4), (&mut observer, 5)] {
            let after = state(stream, request_id).await;
            assert_gone(&after, &pane, "KILL_RESOURCES");
            assert!(
                session(&after, "doomed").is_none(),
                "the emptied session went in the same commit: {:?}",
                after.sessions
            );
        }
        let mut late = server.connect().await;
        send_frame(&mut late, &attach_by_name("doomed")).await;
        wait_frame(&mut late, "the ATTACH answer", |frame| match frame {
            FrameKind::Error { .. } => Some(()),
            FrameKind::Attached { snapshot, .. } => {
                panic!("ATTACH to a killed session succeeded: {snapshot:?}")
            }
            _ => None,
        })
        .await;

        let (single, _) = doomed_pane(&mut killer, 6).await;
        let kill_one = Command::KillResource {
            terminal_id: single.clone(),
            operation_id: None,
        };
        assert_eq!(command(&mut killer, 7, kill_one).await, CommandResult::Ok);
        assert_gone(&state(&mut observer, 8).await, &single, "KILL_RESOURCE");

        let (bound, instance) = doomed_pane(&mut killer, 9).await;
        let kill_if = Command::KillResourceIf {
            terminal_id: bound.clone(),
            precondition: KillPrecondition {
                instance: Some(instance),
                conditions: KillConditions::NONE,
            },
            operation_id: None,
        };
        assert_eq!(command(&mut killer, 10, kill_if).await, CommandResult::Ok);
        assert_gone(&state(&mut observer, 11).await, &bound, "KILL_RESOURCE_IF");

        let (tab, _) = doomed_pane(&mut killer, 12).await;
        let close_tab = Command::CloseTabResources {
            ids: vec![tab.clone()],
        };
        assert_eq!(command(&mut killer, 13, close_tab).await, CommandResult::Ok);
        assert_gone(&state(&mut observer, 14).await, &tab, "CLOSE_TAB_RESOURCES");

        assert!(
            session(&state(&mut observer, 15).await, ANCHOR).is_some(),
            "the bystander session is untouched"
        );
        drop((killer, observer, late));
        server.stop().await;
    });
}

/// The kill's `pane_closed` is journaled in its commit, so a `GET_STATE`
/// that omits the pane carries a `JOURNAL_HEAD` that already covers the
/// close (L1 §7.3): a consumer that catches up to the head has seen it. An
/// `ATTACH_RESOURCE` subscriber gets exactly one `RESOURCE_CLOSED { KILLED }`.
#[test]
fn a_kill_journals_its_close_before_the_reply_and_closes_subscribers_once() {
    phux_server_testkit::run_local(async {
        let server = Server::pty(Some(ANCHOR));
        let mut killer = server.connect().await;
        let mut watcher = server.connect().await;
        let (pane, _) = doomed_pane(&mut killer, 1).await;

        subscribe(&mut watcher, 2, None, None).await;
        let attach = Command::AttachResource {
            terminal_id: pane.clone(),
            role_policy: None,
        };
        assert!(matches!(
            command(&mut watcher, 3, attach).await,
            CommandResult::Ok | CommandResult::OkWith(_)
        ));

        let kill = Command::KillResources {
            ids: vec![pane.clone()],
            operation_id: None,
        };
        assert_eq!(command(&mut killer, 4, kill).await, CommandResult::Ok);
        let after = state(&mut killer, 5).await;
        assert_gone(&after, &pane, "KILL_RESOURCES");
        let head = after
            .journal_head()
            .expect("an EVENT_JOURNAL server writes the head");

        // `pane_closed` and `RESOURCE_CLOSED` reach the watcher by separate
        // paths, in either order; collect both.
        let mut close_seq = None;
        let mut close_reasons = Vec::new();
        wait_frame(&mut watcher, "pane_closed and RESOURCE_CLOSED", |frame| {
            match frame {
                FrameKind::Event {
                    terminal: Some(terminal),
                    event: AgentEvent::ResourceClosed { .. },
                    stamp,
                } if terminal == pane => {
                    close_seq = Some(stamp.expect("a journaled event is stamped").seq);
                }
                FrameKind::ResourceClosed {
                    terminal_id,
                    reason,
                    ..
                } if terminal_id == pane => close_reasons.push(reason),
                _ => {}
            }
            (close_seq.is_some() && !close_reasons.is_empty()).then_some(())
        })
        .await;
        let seq = close_seq.unwrap_or_default();
        assert!(
            seq <= head,
            "pane_closed (seq {seq}) must be journaled by the commit the reply \
             follows, at or below the JOURNAL_HEAD ({head}) of a GET_STATE that \
             already omits the pane"
        );

        let barrier = Command::GetState {
            scope: StateScope::Server,
        };
        send_frame(
            &mut watcher,
            &FrameKind::Command {
                request_id: 6,
                command: barrier,
            },
        )
        .await;
        wait_frame(&mut watcher, "the GET_STATE barrier", |frame| match frame {
            FrameKind::ResourceClosed {
                terminal_id,
                reason,
                ..
            } if terminal_id == pane => {
                close_reasons.push(reason);
                None
            }
            FrameKind::CommandResult { request_id: 6, .. } => Some(()),
            _ => None,
        })
        .await;
        assert_eq!(
            close_reasons,
            vec![CloseReason::Killed],
            "one RESOURCE_CLOSED, KILLED"
        );

        drop((killer, watcher));
        server.stop().await;
    });
}
