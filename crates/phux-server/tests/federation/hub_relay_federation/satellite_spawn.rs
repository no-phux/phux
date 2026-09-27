//! Exact-owner spawning through the real UDS -> hub -> WebSocket -> satellite path.

use super::*;
use phux_protocol::wire::frame::AttachTarget;
use phux_protocol::wire::info::SessionSnapshot;

async fn spawn_split(
    stream: &mut UnixStream,
    request_id: u32,
    owner_terminal: Option<ResourceId>,
    agent_session: Option<Vec<u8>>,
) -> SpawnResult {
    let spawn = Spawn {
        owner_terminal,
        agent_session,
        initial_size: Some((132, 43)),
        ..Spawn::command(&["/bin/cat"])
    };
    spawn_resource(stream, request_id, on_satellite("sat", spawn)).await
}

async fn snapshot(stream: &mut UnixStream, request_id: u32) -> SessionSnapshot {
    let (result, errors) = get_state_via_hub(stream, request_id).await;
    assert!(errors.is_empty(), "unexpected state errors: {errors:?}");
    let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
        panic!("expected state, got {result:?}");
    };
    snapshot
}

async fn create_other_session(stream: &mut UnixStream) -> ResourceId {
    let mut attach = phux_server_testkit::attach_by_name("other");
    let FrameKind::Attach { ref mut target, .. } = attach else {
        unreachable!()
    };
    *target = AttachTarget::CreateIfMissing {
        name: "other".to_owned(),
        command: Some(vec!["/bin/cat".to_owned()]),
        cwd: None,
    };
    send_frame(stream, &attach).await;
    phux_server_testkit::recv_until(stream, |_, frame| match frame {
        FrameKind::Attached { snapshot, .. } => Some(snapshot.focused_resource),
        _ => None,
    })
    .await
}

#[test]
fn exact_owner_satellite_spawn_preserves_window_and_initial_size() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let seed = fed.seed;
        let mut satellite = fed.satellite().await;
        let other = create_other_session(&mut satellite).await;
        let mut hub = fed.linked_hub().await;

        let result = spawn_split(&mut hub, 7000, Some(fed.sat_id()), None).await;
        let SpawnResult::Ok(ResourceId::Satellite { host, id }) = result else {
            panic!("exact-owner spawn must succeed: {result:?}");
        };
        assert_eq!(host.as_str(), "sat");
        let state = snapshot(&mut satellite, 7001).await;
        let find = |id: &ResourceId| state.resources.iter().find(|p| &p.id == id).unwrap();
        let owner = find(&ResourceId::local(seed));
        let spawned = find(&ResourceId::local(id));
        assert_ne!(owner.window_id, find(&other).window_id, "distinct windows");
        assert_eq!(
            spawned.window_id, owner.window_id,
            "exact owner wins over active session"
        );
        assert_eq!((spawned.cols, spawned.rows), (132, 43));
        let CommandResult::OkWith(CommandValue::Json(screen)) =
            get_screen_via_hub(&mut hub, 7002, ResourceId::satellite("sat", id)).await
        else {
            panic!("new terminal must route immediately");
        };
        let screen: serde_json::Value = serde_json::from_str(&screen).unwrap();
        assert_eq!(
            (screen["cols"].as_u64(), screen["rows"].as_u64()),
            (Some(132), Some(43))
        );

        // Owner-less spawns still honour caller-supplied geometry.
        let result = spawn_split(&mut hub, 7003, None, None).await;
        let SpawnResult::Ok(ResourceId::Satellite { id, .. }) = result else {
            panic!("owner-less spawn must succeed: {result:?}");
        };
        let state = snapshot(&mut satellite, 7004).await;
        let spawned = state
            .resources
            .iter()
            .find(|p| p.id == ResourceId::local(id))
            .unwrap();
        assert_eq!((spawned.cols, spawned.rows), (132, 43));

        drop((hub, satellite));
        fed.shutdown().await;
    });
}

#[test]
fn satellite_spawn_refuses_wrong_owners_and_provenance_without_creating_panes() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot_with(None, Some("hub-session")).await;
        let seed = fed.seed;
        let mut hub = fed.linked_hub().await;
        let before = snapshot(&mut hub, 7100).await;
        // The aggregate covers both servers, so the before/after check does too.
        assert_eq!(before.resources.len(), 2);
        assert!(before.resources.iter().any(|p| p.id == fed.sat_id()));
        let cases = [
            (
                Some(ResourceId::local(seed)),
                None,
                "requested satellite host",
            ),
            (
                Some(ResourceId::satellite("different", seed)),
                None,
                "requested satellite host",
            ),
            (
                Some(ResourceId::satellite("sat", u32::MAX)),
                None,
                "not found",
            ),
            (None, Some(vec![]), "agent-session provenance is local-only"),
            (
                Some(fed.sat_id()),
                Some(vec![1]),
                "agent-session provenance is local-only",
            ),
        ];
        for (owner, provenance, diagnostic) in cases {
            let result = spawn_split(&mut hub, 7101, owner, provenance).await;
            let SpawnResult::Err(SpawnError::SpawnFailed(message)) = result else {
                panic!("invalid request must fail: {result:?}");
            };
            assert!(
                message.contains(diagnostic),
                "{message:?} must contain {diagnostic:?}"
            );
        }
        let after = snapshot(&mut hub, 7102).await;
        assert_eq!(
            after.resources.len(),
            before.resources.len(),
            "no fallback spawns"
        );
        assert_eq!(after.sessions, before.sessions);

        drop(hub);
        fed.shutdown().await;
    });
}
