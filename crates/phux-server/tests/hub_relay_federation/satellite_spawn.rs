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
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: Some(vec!["/bin/cat".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: Some(SatelliteHost::new("sat")),
            owner_terminal,
            agent_session,
            initial_size: Some((132, 43)),
            resource: None,
        },
    )
    .await;
    tokio::time::timeout(STEP_DEADLINE, async {
        loop {
            if let (
                _,
                FrameKind::ResourceSpawned {
                    request_id: got,
                    result,
                },
            ) = recv_typed(stream).await
                && got == request_id
            {
                return result;
            }
        }
    })
    .await
    .expect("correlated spawn reply")
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
        panic!("attach fixture");
    };
    *target = AttachTarget::CreateIfMissing {
        name: "other".to_owned(),
        command: Some(vec!["/bin/cat".to_owned()]),
        cwd: None,
    };
    send_frame(stream, &attach).await;
    tokio::time::timeout(STEP_DEADLINE, async {
        loop {
            if let (_, FrameKind::Attached { snapshot, .. }) = recv_typed(stream).await {
                return snapshot.focused_resource;
            }
        }
    })
    .await
    .expect("other session attached")
}

#[test]
fn exact_owner_satellite_spawn_preserves_window_and_initial_size() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"), ws_port);
        let seed = discover_satellite_pane(ws_port).await;
        let mut satellite = wait_for_socket(&tmp.path().join("sat.sock"), STEP_DEADLINE).await;
        let other = create_other_session(&mut satellite).await;
        let (hub_shutdown, hub_task) = spawn_hub(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
        );
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        assert!(matches!(
            get_screen_until_ok(&mut hub, seed).await,
            CommandResult::OkWith(_)
        ));

        let result = spawn_split(
            &mut hub,
            7000,
            Some(ResourceId::satellite("sat", seed)),
            None,
        )
        .await;
        let SpawnResult::Ok(ResourceId::Satellite { host, id }) = result else {
            panic!("exact-owner spawn must succeed: {result:?}");
        };
        assert_eq!(host.as_str(), "sat");
        let state = snapshot(&mut satellite, 7001).await;
        let owner = state
            .resources
            .iter()
            .find(|p| p.id == ResourceId::local(seed))
            .unwrap();
        let other = state.resources.iter().find(|p| p.id == other).unwrap();
        let spawned = state
            .resources
            .iter()
            .find(|p| p.id == ResourceId::local(id))
            .unwrap();
        assert_ne!(owner.window_id, other.window_id, "distinct hosting windows");
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
        assert_eq!(screen["cols"], 132);
        assert_eq!(screen["rows"], 43);

        // Legacy owner-less spawns still work, including caller-supplied geometry.
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

        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
        drop(sat_shutdown);
        sat_task.await.unwrap().unwrap();
    });
}

#[test]
fn satellite_spawn_refuses_wrong_owners_and_provenance_without_creating_panes() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"), ws_port);
        let seed = discover_satellite_pane(ws_port).await;
        let (hub_shutdown, hub_task) = spawn_hub_with_session(
            tmp.path().join("hub.sock"),
            vec![satellite_entry("sat", ws_port)],
            Some("hub-session"),
        );
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        assert!(matches!(
            get_screen_until_ok(&mut hub, seed).await,
            CommandResult::OkWith(_)
        ));
        let before = snapshot(&mut hub, 7100).await;
        // Hub GET_STATE aggregates satellite panes. Pin that premise so this
        // before/after check cannot accidentally cover only the hub's state.
        assert_eq!(before.resources.len(), 2);
        assert!(
            before
                .resources
                .iter()
                .any(|p| p.id == ResourceId::satellite("sat", seed))
        );
        assert!(
            before
                .resources
                .iter()
                .any(|p| matches!(p.id, ResourceId::Local { .. }))
        );
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
                Some(ResourceId::satellite("sat", seed)),
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
            "no local or satellite fallback spawns"
        );
        assert_eq!(after.sessions, before.sessions);

        drop(hub_shutdown);
        hub_task.await.unwrap().unwrap();
        drop(sat_shutdown);
        sat_task.await.unwrap().unwrap();
    });
}
