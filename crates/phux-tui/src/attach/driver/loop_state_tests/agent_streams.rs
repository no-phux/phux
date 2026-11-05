//! Real server -> native TUI bootstrap -> explicit agent stream -> chrome state.
use super::*;
use phux_client_core::session::agent_stream::AgentSessionStatus;
use phux_protocol::wire::frame::{SpawnResource, SpawnResult, ViewportInfo};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, command, join_after_shutdown, recv_until, send_frame,
    spawn_resource, spawn_server_seed_pty_no_cmd, wait_for_socket,
};

const SESSION: &str = "tui-agent-bootstrap";

async fn spawn_agent(producer: &mut tokio::net::UnixStream, session: &str) -> ResourceId {
    send_frame(
        producer,
        &FrameKind::Attach {
            attach_id: 1,
            target: AttachTarget::CreateIfMissing {
                name: session.to_owned(),
                command: Some(vec!["/bin/cat".to_owned()]),
                cwd: None,
            },
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: None,
        },
    )
    .await;
    let parent = recv_until(producer, |_, frame| match frame {
        FrameKind::Attached { snapshot, .. } => Some(snapshot.focused_resource),
        _ => None,
    })
    .await;
    let result = spawn_resource(
        producer,
        50,
        Spawn {
            resource: Some(Box::new(SpawnResource::agent_session(parent, "claude"))),
            ..Spawn::default()
        },
    )
    .await;
    let SpawnResult::Ok(id) = result else {
        panic!("agent spawn: {result:?}");
    };
    id
}

async fn append(producer: &mut tokio::net::UnixStream, id: &ResourceId, kind: &str) {
    let result = command(
        producer,
        51,
        Command::AppendResourceOutput {
            terminal_id: id.clone(),
            bytes: format!("{{\"type\":\"{kind}\",\"data\":{{}}}}\n").into_bytes(),
        },
    )
    .await;
    assert!(
        matches!(result, CommandResult::Ok | CommandResult::OkWith(_)),
        "{result:?}"
    );
}

async fn open_loop(socket: &std::path::Path) -> (SessionLoop, Connection, Vec<u8>) {
    use crate::attach::driver::session_io::{
        attach_client_caps, send_attach_without_size_vote, wait_for_attached,
    };
    let caps = attach_client_caps(None, &crate::attach::Dial::uds(socket));
    let mut client = Connection::connect_with_hello(socket, "tui-agent-live-test".into(), caps)
        .await
        .unwrap();
    let negotiated = client.negotiated_bootstrap().unwrap();
    let attach_id =
        send_attach_without_size_vote(&mut client, AttachTarget::ByName(SESSION.into()))
            .await
            .unwrap();
    let attached = wait_for_attached(&mut client, attach_id).await.unwrap();
    let mut state = SessionLoop::new(
        negotiated,
        PredictiveConfig::disabled(),
        false,
        None,
        EntryPick::default(),
        None,
    )
    .unwrap();
    let mut out = Vec::new();
    state
        .bootstrap(&mut client, &mut out, attached, None)
        .await
        .unwrap();
    pump_until(&mut state, &mut client, &mut out, |state| {
        state.mirror.engine_kernel.attach_ready_pending().is_none()
    })
    .await;
    state
        .emit_deferred_bootstrap_outbound(&mut client)
        .await
        .unwrap();
    (state, client, out)
}

async fn apply(
    state: &mut SessionLoop,
    client: &mut Connection,
    out: &mut Vec<u8>,
    frame: FrameKind,
) {
    assert!(
        !state
            .coordinate_multistream_frame(client, &frame)
            .await
            .unwrap()
    );
    let step = state
        .apply_server_frame(
            client,
            out,
            None,
            frame,
            true,
            &mut RepaintAccumulator::default(),
        )
        .await
        .unwrap();
    assert!(matches!(step, FrameStep::Done));
}

async fn pump_until(
    state: &mut SessionLoop,
    client: &mut Connection,
    out: &mut Vec<u8>,
    ready: impl Fn(&SessionLoop) -> bool,
) -> Vec<FrameKind> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut frames = Vec::new();
        while !ready(state) {
            let frame = client.recv().await.unwrap();
            frames.push(frame.clone());
            apply(state, client, out, frame).await;
        }
        frames
    })
    .await
    .expect("native TUI reaches the agent publication")
}

fn committed(state: &SessionLoop, id: &ResourceId, seq: u64) -> bool {
    state
        .mirror
        .engine_kernel
        .agent_stream_info(id)
        .is_some_and(|info| info.last_seq == seq)
        && !state
            .mirror
            .pending_resource_ops
            .values()
            .any(|pending| pending == id)
}

fn sequences(state: &SessionLoop, id: &ResourceId) -> Vec<u64> {
    state
        .mirror
        .engine_kernel
        .agent_session(id)
        .unwrap()
        .log
        .records()
        .map(|record| record.seq)
        .collect()
}

async fn prove_duplicate_event(
    state: &mut SessionLoop,
    client: &mut Connection,
    out: &mut Vec<u8>,
    id: &ResourceId,
) {
    let parent = state
        .mirror
        .engine_kernel
        .agent_session(id)
        .unwrap()
        .parent
        .unwrap()
        .clone();
    let before = state.next_request_id;
    apply(
        state,
        client,
        out,
        FrameKind::Event {
            terminal: Some(id.clone()),
            stamp: None,
            event: phux_protocol::wire::frame::AgentEvent::ResourceSpawned {
                kind: phux_protocol::ResourceKind::AgentSession,
                parent: Some(parent),
            },
        },
    )
    .await;
    assert_eq!(
        state.next_request_id, before,
        "known snapshot agent is not attached twice"
    );
}

async fn prove_reattach(
    state: &mut SessionLoop,
    client: &mut Connection,
    out: &mut Vec<u8>,
    id: &ResourceId,
) {
    use crate::attach::driver::session_io::send_attach_without_size_vote;
    let previous = state.mirror.engine_kernel.agent_stream_info(id);
    send_attach_without_size_vote(client, AttachTarget::ByName(SESSION.into()))
        .await
        .unwrap();
    // Native rebootstrap retains this kernel's committed cursor. Drive the
    // actual ATTACHED outcome, rather than issuing an agent command in the test.
    let frames = pump_until(state, client, out, |state| {
        committed(state, id, 2)
            && state.mirror.engine_kernel.attach_ready_pending().is_none()
            && state.mirror.engine_kernel.agent_stream_info(id) != previous
    })
    .await;
    assert!(frames.iter().any(|frame| matches!(frame, FrameKind::BootstrapBegin {
        terminal_id, profile: phux_protocol::BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq: 2 }, ..
    } if terminal_id == id)), "the actual TUI reattach requests its committed cursor: {frames:?}");
    assert!(!frames.iter().any(
        |frame| matches!(frame, FrameKind::BootstrapChunk { terminal_id, .. } if terminal_id == id)
    ));
    assert_eq!(sequences(state, id), [1, 2]);
}

#[tokio::test(flavor = "current_thread")]
async fn native_tui_subscribes_snapshot_agents_and_resumes_without_duplicate_history() {
    Box::pin(tokio::task::LocalSet::new().run_until(async {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("server.sock");
        let (shutdown, server) = spawn_server_seed_pty_no_cmd(socket.clone(), None);
        let mut producer = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let id = spawn_agent(&mut producer, SESSION).await;
        append(&mut producer, &id, "tool_start").await;
        let mut foreign_producer = wait_for_socket(&socket, SOCKET_CONNECT_DEADLINE).await;
        let foreign = spawn_agent(&mut foreign_producer, "foreign").await;
        let (mut state, mut client, mut out) = Box::pin(open_loop(&socket)).await;
        pump_until(&mut state, &mut client, &mut out, |state| {
            committed(state, &id, 1)
        })
        .await;
        assert_eq!(sequences(&state, &id), [1]);
        assert_eq!(
            state
                .mirror
                .engine_kernel
                .agent_session(&id)
                .unwrap()
                .state
                .status,
            AgentSessionStatus::Working
        );
        assert!(state.mirror.engine_kernel.agent_session(&foreign).is_none());
        prove_duplicate_event(&mut state, &mut client, &mut out, &id).await;
        append(&mut producer, &id, "stop").await;
        pump_until(&mut state, &mut client, &mut out, |state| {
            committed(state, &id, 2)
        })
        .await;
        assert_eq!(
            state
                .mirror
                .engine_kernel
                .agent_session(&id)
                .unwrap()
                .state
                .status,
            AgentSessionStatus::Done
        );
        assert_eq!(sequences(&state, &id), [1, 2]);
        prove_reattach(&mut state, &mut client, &mut out, &id).await;
        drop((state, client));
        let (mut state, mut client, mut out) = Box::pin(open_loop(&socket)).await;
        pump_until(&mut state, &mut client, &mut out, |state| {
            committed(state, &id, 2)
        })
        .await;
        assert_eq!(
            sequences(&state, &id),
            [1, 2],
            "fresh reconnect owns no old cursor and gets the retained cut"
        );
        drop((state, client, producer, foreign_producer));
        join_after_shutdown(shutdown, server).await;
    }))
    .await;
}
