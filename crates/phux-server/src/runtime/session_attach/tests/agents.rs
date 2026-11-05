//! Agent membership is aggregate metadata; its stream starts at an explicit cursor.
use super::*;

async fn subscribe_agent(
    connection: &mut Connection,
    requests: &mut mpsc::Receiver<phux_agent_session::session::BootstrapRequest>,
    wire: &phux_protocol::ResourceId,
    after_seq: Option<u64>,
) {
    connection
        .send(FrameKind::Command {
            request_id: 70,
            command: phux_protocol::wire::frame::Command::attach_agent_session(
                wire.clone(),
                None,
                after_seq,
            ),
        })
        .await;
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.after_seq, after_seq);
    let records = if after_seq.is_none() {
        vec![Bytes::from_static(
            b"{\"seq\":1,\"ts_ms\":1,\"type\":\"prompt\",\"data\":{}}\n",
        )]
    } else {
        Vec::new()
    };
    let mut cut = agent_cut(records);
    cut.base_seq = 1;
    cut.resume_after_seq = after_seq;
    request.reply.send(cut).unwrap();
    if after_seq.is_some() {
        let tombstone = connection.next().await;
        assert!(
            matches!(&tombstone, FrameKind::BootstrapTombstone {
            terminal_id, reason: phux_protocol::wire::frame::TombstoneReason::ExplicitReattach,
            last_valid_seq: 1, ..
        } if terminal_id == wire),
            "replacement fences its prior generation: {tombstone:?}"
        );
    }
    expect_agent_bootstrap(connection, wire, after_seq).await;
}

async fn expect_agent_bootstrap(
    connection: &mut Connection,
    wire: &phux_protocol::ResourceId,
    after_seq: Option<u64>,
) {
    let profile = after_seq.map_or(BootstrapStreamProfile::AgentEventsJsonlV1, |after_seq| {
        BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq }
    });
    let begin = connection.next().await;
    assert!(
        matches!(&begin, FrameKind::BootstrapBegin {
        terminal_id, profile: got, base_seq: 1, ..
    } if terminal_id == wire && *got == profile),
        "{begin:?}"
    );
    if after_seq.is_none() {
        assert!(
            matches!(connection.next().await, FrameKind::BootstrapChunk { terminal_id, .. } if terminal_id == *wire)
        );
    }
    assert!(
        matches!(connection.next().await, FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == *wire)
    );
    assert!(matches!(
        connection.next().await,
        FrameKind::CommandResult {
            request_id: 70,
            result: phux_protocol::wire::frame::CommandResult::Ok
        }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn agent_only_and_document_sessions_defer_cuts_without_closing_live_agents() {
    LocalSet::new()
        .run_until(async {
            for with_document in [false, true] {
                let state = SharedState::new();
                let (session, parent) = parent(&state, false);
                if with_document {
                    tree(&state, parent, session);
                }
                let (agent, mut requests) = agent_stub(&state, parent, session);
                let wire = state.with_mut(|s| s.intern_resource_wire(agent));
                let output = state.with(|s| s.resource_handle(agent).unwrap().output.clone());
                let mut connection = Connection::start(&state, false).await;
                for attach_id in [1, 2] {
                    connection.attach(attach_id).await;
                    let profiles =
                        prefix(&mut connection, 1 + usize::from(with_document), attach_id).await;
                    assert!(!profiles.contains(&BootstrapStreamProfile::AgentEventsJsonlV1));
                    assert!(
                        requests.try_recv().is_err(),
                        "aggregate capture never requests an agent cut"
                    );
                    assert_eq!(
                        output.receiver_count(),
                        0,
                        "replacement ATTACH fences the prior explicit pump"
                    );
                    assert!(state.with(|s| s.resource_handle(agent).is_some()));
                    subscribe_agent(
                        &mut connection,
                        &mut requests,
                        &wire,
                        (attach_id == 2).then_some(1),
                    )
                    .await;
                    assert_eq!(
                        output.receiver_count(),
                        1,
                        "only the explicit subscription owns a pump"
                    );
                }
                connection.finish().await;
            }
        })
        .await;
}
