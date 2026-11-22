//! The agent stream's frames as a resume-capable server sends them, and
//! what the plane publishes and remembers from them.

use super::*;

pub(super) fn agent_hello(resume: bool, incarnation: u8) -> FrameKind {
    let mut hello = hello_ok_with(&[ServerFeature::SpawnIdempotency]);
    if let FrameKind::HelloOk {
        server_caps,
        server_id,
        ..
    } = &mut hello
    {
        *server_id = vec![incarnation; 16];
        *server_caps = server_caps.with_features_ext(ServerFeatureExtSet::with(if resume {
            &[ServerFeatureExt::AgentStreamResume]
        } else {
            &[]
        }));
    }
    hello
}

pub(super) fn agent_requests(plane: &mut ControlPlane) -> Vec<(u32, Option<u64>)> {
    plane
        .take_outbound()
        .iter()
        .filter_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                request_id,
                command:
                    Command::AttachResource {
                        terminal_id,
                        agent_after_seq,
                        ..
                    },
            } if terminal_id == ResourceId::local(AGENT) => Some((request_id, agent_after_seq)),
            _ => None,
        })
        .collect()
}

pub(super) fn reconnect_agents(
    plane: &mut ControlPlane,
    resume: bool,
    incarnation: u8,
) -> Vec<(u32, Option<u64>)> {
    plane.connection_lost(None);
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), None);
    reopen(plane, agent_hello(resume, incarnation));
    let attach_id = plane.queued_attach_id();
    attach_ready_to(plane, attach_id, with_agent(terminal()));
    agent_requests(plane)
}

pub(super) const fn agent_ids(generation: u64) -> (StreamId, BootstrapId) {
    (
        StreamId::new(100 + generation).unwrap(),
        BootstrapId::new(200 + generation).unwrap(),
    )
}

pub(super) fn agent_begin(
    plane: &mut ControlPlane,
    generation: u64,
    after_seq: Option<u64>,
    base_seq: u64,
) {
    let (stream_id, bootstrap_id) = agent_ids(generation);
    plane
        .feed(FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(AGENT),
            stream_id,
            bootstrap_id,
            profile: after_seq.map_or(BootstrapStreamProfile::AgentEventsJsonlV1, |after_seq| {
                BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq }
            }),
            cols: 0,
            rows: 0,
            base_seq,
        })
        .unwrap();
}

pub(super) fn agent_payload(records: &[(u64, &str)]) -> String {
    use std::fmt::Write;
    let mut payload = String::new();
    for (seq, kind) in records {
        writeln!(
            payload,
            "{{\"seq\":{seq},\"ts_ms\":1,\"type\":\"{kind}\",\"data\":{{}}}}"
        )
        .unwrap();
    }
    payload
}

pub(super) fn agent_chunk(
    plane: &mut ControlPlane,
    generation: u64,
    chunk_seq: u32,
    payload: String,
) -> Result<(), ControlError> {
    let (stream_id, bootstrap_id) = agent_ids(generation);
    plane.feed(FrameKind::BootstrapChunk {
        terminal_id: ResourceId::local(AGENT),
        stream_id,
        bootstrap_id,
        chunk_seq,
        payload: payload.into(),
    })
}

pub(super) fn agent_ready(plane: &mut ControlPlane, generation: u64) -> Result<(), ControlError> {
    let (stream_id, bootstrap_id) = agent_ids(generation);
    plane.feed(FrameKind::BootstrapReady {
        terminal_id: ResourceId::local(AGENT),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    })
}

pub(super) fn agent_live(
    plane: &mut ControlPlane,
    generation: u64,
    records: &[(u64, &str)],
) -> Result<(), ControlError> {
    let (stream_id, bootstrap_id) = agent_ids(generation);
    plane.feed(FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(AGENT),
        stream_id,
        bootstrap_id,
        seq: records.last().unwrap().0,
        bytes: agent_payload(records).into(),
    })
}

pub(super) fn take_agent_records(plane: &mut ControlPlane) -> Vec<(Vec<u64>, bool)> {
    publications_of(plane.take_events())
}

pub(super) fn publications_of(events: Vec<Event>) -> Vec<(Vec<u64>, bool)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            Event::AgentPublication { publication, .. } => {
                assert_eq!(publication.resource_id, ResourceId::local(AGENT));
                Some((
                    publication
                        .records
                        .iter()
                        .map(|record| record.seq)
                        .collect(),
                    publication.mode == phux_client_runtime::control::AgentRecordMode::Replace,
                ))
            }
            _ => None,
        })
        .collect()
}

pub(super) fn commit_initial_agent(plane: &mut ControlPlane, request_id: u32) {
    // Reference-server ordering: the bootstrap can precede COMMAND_RESULT.
    publish_agent(plane);
    plane.reply(request_id, CommandResult::Ok);
    assert_eq!(take_agent_records(plane), [(vec![1, 2], true)]);
}

/// A resume-capable plane attached and ready with the agent committed at
/// seq 2 (generation 1).
pub(super) fn resumed_with_initial_agent() -> ControlPlane {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    commit_initial_agent(&mut plane, requests[0].0);
    plane
}

pub(super) fn agent_info(
    plane: &ControlPlane,
) -> Option<phux_client_runtime::engine::AgentStreamInfo> {
    plane
        .engine()
        .unwrap()
        .agent_stream_info(&ResourceId::local(AGENT))
}

pub(super) fn agent_cursor(plane: &ControlPlane) -> Option<u64> {
    plane.agent_after_seq(&ResourceId::local(AGENT))
}

/// The agent's full delta bootstrap of generation `generation`: begin, one
/// chunk of `records`, ready.
pub(super) fn agent_bootstrap(
    plane: &mut ControlPlane,
    generation: u64,
    after_seq: Option<u64>,
    records: &[(u64, &str)],
) {
    agent_begin(plane, generation, after_seq, records.last().unwrap().0);
    agent_chunk(plane, generation, 0, agent_payload(records)).unwrap();
    agent_ready(plane, generation).unwrap();
}

/// Close the agent resource (`close`) or detach its parent; either retires
/// the agent once, reporting the cursor it stood at.
pub(super) fn retire_agent(plane: &mut ControlPlane, close: bool) -> Vec<Option<u64>> {
    if close {
        plane
            .feed(phux_wire_fixtures::closed(&ResourceId::local(AGENT), None))
            .unwrap();
    } else {
        let request_id = plane.detach_terminal(&terminal());
        plane.reply(request_id, CommandResult::Ok);
    }
    plane
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::AgentSessionClosed {
                terminal_id,
                stream,
                ..
            } if terminal_id == ResourceId::local(AGENT) => Some(stream.map(|s| s.last_seq)),
            _ => None,
        })
        .collect()
}
