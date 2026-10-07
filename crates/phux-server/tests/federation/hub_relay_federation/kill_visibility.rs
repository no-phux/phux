//! L1 §5.2 through a hub (phux-t8nn): the hub answers a satellite kill only
//! after the satellite answered, and the satellite's reply follows its
//! commit, so the hub's very next aggregated `GET_STATE` omits the killed
//! Terminals. Each victim traps `SIGHUP`, so its process outlives the reply
//! by the satellite's full pane-kill grace.

use super::*;
use phux_server_testkit::command;

const HANGUP_PROOF: &str = "trap '' HUP; while :; do sleep 1; done";

fn lists(result: &CommandResult, pane: &ResourceId) -> bool {
    let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
        panic!("GET_STATE via the hub failed: {result:?}");
    };
    snapshot
        .resources
        .iter()
        .any(|resource| &resource.id == pane)
}

#[test]
fn a_relayed_kill_reply_is_the_satellites_committed_teardown() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut hub = fed.linked_hub().await;

        let mut panes = Vec::new();
        for request_id in [8100, 8101] {
            let spawn = on_satellite("sat", Spawn::command(&["/bin/sh", "-c", HANGUP_PROOF]));
            let SpawnResult::Ok(pane) = spawn_resource(&mut hub, request_id, spawn).await else {
                panic!("satellite spawn failed");
            };
            panes.push(pane);
        }
        let (before, _) = get_state_via_hub(&mut hub, 8102).await;
        assert!(panes.iter().all(|pane| lists(&before, pane)), "{before:?}");

        let kill_one = Command::KillResource {
            terminal_id: panes[0].clone(),
            operation_id: None,
        };
        assert_eq!(command(&mut hub, 8103, kill_one).await, CommandResult::Ok);
        let (after_one, _) = get_state_via_hub(&mut hub, 8104).await;
        assert!(
            !lists(&after_one, &panes[0]),
            "KILL_RESOURCE: {after_one:?}"
        );

        let kill_rest = Command::KillResources {
            ids: vec![panes[1].clone()],
            operation_id: None,
        };
        assert_eq!(command(&mut hub, 8105, kill_rest).await, CommandResult::Ok);
        let (after_all, _) = get_state_via_hub(&mut hub, 8106).await;
        assert!(
            !lists(&after_all, &panes[1]),
            "KILL_RESOURCES: {after_all:?}"
        );
    });
}
