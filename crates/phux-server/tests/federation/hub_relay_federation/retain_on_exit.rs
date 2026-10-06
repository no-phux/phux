//! Retain-on-exit through the hub (ADR-0124, `docs/spec/L1.md` §3.1, §9.1).
//! A hub forwards `SPAWN_RESOURCE` field 16 `retain_secs` to a satellite that
//! advertises `RETAIN_ON_EXIT`, so the satellite keeps the exited Terminal as
//! a facet instead of closing it. The refusal toward a satellite without the
//! bit is pinned by the relay unit tests; every live satellite here has it.

use super::*;
use phux_protocol::wire::frame::ResourceLifecycle;
use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

/// Spawn `sh -c script` on `sat` through the hub, retained for `retain` seconds.
async fn spawn_via_hub(
    hub: &mut UnixStream,
    request_id: u32,
    script: &str,
    retain: Option<u32>,
) -> u32 {
    let spawn = Spawn {
        resource: retain
            .map(|secs| Box::new(SpawnResource::default().with_retain_secs(Some(secs)))),
        ..Spawn::command(&["/bin/sh", "-c", script])
    };
    let result = spawn_resource(hub, request_id, on_satellite("sat", spawn)).await;
    let SpawnResult::Ok(ResourceId::Satellite { host, id }) = result else {
        panic!("satellite spawn {request_id} must succeed: {result:?}");
    };
    assert_eq!(host.as_str(), "sat");
    id
}

/// Poll the satellite's own `GET_STATE` until `ready` accepts it.
async fn wait_for_satellite_state(
    satellite: &mut UnixStream,
    first_request_id: u32,
    what: &str,
    ready: impl Fn(&SessionSnapshot) -> bool,
) -> SessionSnapshot {
    let deadline = Instant::now() + STEP_DEADLINE;
    for request_id in first_request_id.. {
        let (result, _) = get_state_via_hub(satellite, request_id).await;
        let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
            panic!("expected state, got {result:?}");
        };
        if ready(&snapshot) {
            return snapshot;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    unreachable!()
}

fn find(snapshot: &SessionSnapshot, id: u32) -> Option<&ResourceInfo> {
    let id = ResourceId::local(id);
    snapshot.resources.iter().find(|r| r.id == id)
}

/// The hub relays `retain_secs`: the satellite keeps the retained pane with
/// its exit facet, and closes the pane spawned without the field.
#[test]
fn a_hub_relayed_retained_spawn_is_kept_on_the_satellite_after_exit() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut satellite = fed.satellite().await;
        let mut hub = fed.linked_hub().await;

        let retained = spawn_via_hub(&mut hub, 9000, "exit 7", Some(600)).await;
        let plain = spawn_via_hub(&mut hub, 9001, "exit 3", None).await;

        let snapshot = wait_for_satellite_state(
            &mut satellite,
            9100,
            "the retained pane to exit and the plain pane to close",
            |s| {
                find(s, plain).is_none()
                    && find(s, retained)
                        .is_some_and(|r| matches!(r.lifecycle, ResourceLifecycle::Exited))
            },
        )
        .await;
        let info = find(&snapshot, retained).expect("retained pane listed");
        let exit = info
            .exit
            .as_ref()
            .expect("a retained pane carries its exit");
        assert_eq!(exit.exit_status, Some(7), "{exit:?}");
        assert!(
            exit.retained_until_ms > exit.exited_at_ms,
            "retain_secs crossed the link: {exit:?}"
        );
        // The exit resync tombstoned the hub's generation before replacing
        // it (L1 §4.6), so the link survived and the grid reads through it.
        let screen =
            get_screen_via_hub(&mut hub, 9200, ResourceId::satellite("sat", retained)).await;
        assert!(matches!(screen, CommandResult::OkWith(_)), "{screen:?}");

        drop((hub, satellite));
        fed.shutdown().await;
    });
}
