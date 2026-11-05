use super::super::{
    AttachTarget, Duration, GhosttyAdapter, HeadlessChrome, HeadlessCompletion, Path,
    SessionKernel, drain_until_settled, send_attach_without_size_vote, wait_for_attached,
};
use super::*;
use phux_client_core::session::agent_stream::AgentSessionStatus;
use phux_protocol::wire::frame::{
    Command, CommandResult, SpawnResource, SpawnResult, ViewportInfo,
};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, command, join_after_shutdown, recv_until, send_frame,
    spawn_resource, spawn_server_seed_pty_no_cmd, wait_for_socket,
};

async fn seed_agent(socket: &Path) -> (tokio::net::UnixStream, ResourceId) {
    let mut producer = wait_for_socket(socket, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut producer,
        &FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::CreateIfMissing {
                name: "headless-agent".into(),
                command: Some(vec!["/bin/cat".into()]),
                cwd: None,
            },
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: None,
        },
    )
    .await;
    let parent = recv_until(&mut producer, |_, frame| match frame {
        FrameKind::Attached { snapshot, .. } => Some(snapshot.focused_resource),
        _ => None,
    })
    .await;
    let spawned = spawn_resource(
        &mut producer,
        50,
        Spawn {
            resource: Some(Box::new(SpawnResource::agent_session(parent, "claude"))),
            ..Spawn::default()
        },
    )
    .await;
    let SpawnResult::Ok(id) = spawned else {
        panic!("{spawned:?}");
    };
    let result = command(
        &mut producer,
        51,
        Command::AppendResourceOutput {
            terminal_id: id.clone(),
            bytes: b"{\"type\":\"tool_start\",\"data\":{}}\n".to_vec(),
        },
    )
    .await;
    assert!(matches!(
        result,
        CommandResult::Ok | CommandResult::OkWith(_)
    ));
    (producer, id)
}

#[tokio::test(flavor = "current_thread")]
async fn headless_snapshot_waits_for_explicit_agent_cut_after_aggregate_ready() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("server.sock");
            let (shutdown, server) = spawn_server_seed_pty_no_cmd(socket.clone(), None);
            let (producer, id) = seed_agent(&socket).await;
            let caps = crate::attach::driver::session_io::attach_client_caps(
                None,
                &crate::attach::Dial::uds(&socket),
            );
            let mut conn =
                Connection::connect_with_hello(&socket, "headless-agent-test".into(), caps)
                    .await
                    .unwrap();
            let negotiated = conn.negotiated_bootstrap().unwrap();
            let mut attach_id = send_attach_without_size_vote(
                &mut conn,
                AttachTarget::ByName("headless-agent".into()),
            )
            .await
            .unwrap();
            let attached = wait_for_attached(&mut conn, attach_id).await.unwrap();
            let kernel =
                SessionKernel::new(GhosttyAdapter::new(negotiated.limits), negotiated.profile);
            let chrome = HeadlessChrome {
                sidebar: None,
                theme: crate::render::Theme::default(),
                agents_share: 40,
                status_bar: None,
            };
            let mut session = HeadlessSession::new(kernel, chrome, (80, 24));
            let outcome = session.ingest(attached, None, None).unwrap();
            let focused = outcome.sessions.map(|(_, focused)| focused);
            session.request_agent_records(&mut conn).await.unwrap();
            assert!(!session.agent_metadata_complete());
            let mut completion = HeadlessCompletion::new(None);
            tokio::time::timeout(
                Duration::from_secs(10),
                drain_until_settled(
                    &mut conn,
                    &mut session,
                    &mut completion,
                    &mut attach_id,
                    focused,
                    None,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            let agent = session.mirror.engine_kernel.agent_session(&id).unwrap();
            assert_eq!(
                agent
                    .log
                    .records()
                    .map(|record| record.seq)
                    .collect::<Vec<_>>(),
                [1]
            );
            assert_eq!(agent.state.status, AgentSessionStatus::Working);
            assert!(session.agent_metadata_complete());
            drop((session, conn, producer));
            join_after_shutdown(shutdown, server).await;
        })
        .await;
}
