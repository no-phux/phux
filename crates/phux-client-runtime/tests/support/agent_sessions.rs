//! `ControlOptions::subscribe_agent_sessions`: which `AgentSession` streams
//! the runtime subscribes itself.

use super::*;
use phux_protocol::AgentFacet;

const AGENT: u32 = 31;

fn subscribing(subscribe_agent_sessions: bool) -> (ControlPlane, u32) {
    subscribing_with_resume(subscribe_agent_sessions, false)
}

fn subscribing_with_resume(subscribe_agent_sessions: bool, resume: bool) -> (ControlPlane, u32) {
    let mut plane = ControlPlane::new(ControlOptions {
        attach: Some(AttachTarget::ByName("main".to_owned())),
        viewport: (20, 4),
        subscribe_agent_sessions,
        ..ControlOptions::default()
    });
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(agent_hello(resume, 0xAB)).expect("HELLO_OK");
    let attach_id = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("ATTACH");
    (plane, attach_id)
}

/// `two_session_snapshot` plus an agent session running in `parent`.
fn with_agent(parent: ResourceId) -> SessionSnapshot {
    let mut snapshot = two_session_snapshot(false);
    snapshot.resources.push(
        ResourceInfo::new(ResourceId::local(AGENT), WindowId::new(0), 0, 0)
            .with_kind(phux_protocol::ResourceKind::AgentSession)
            .with_parent(Some(parent))
            .with_agent(Some(AgentFacet::new("claude", "working"))),
    );
    snapshot
}

fn attach_to(plane: &mut ControlPlane, attach_id: u32, snapshot: SessionSnapshot) {
    plane
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .expect("ATTACHED");
    let stream_id = StreamId::new(1000 + u64::from(attach_id)).unwrap();
    let bootstrap_id = BootstrapId::new(1000 + u64::from(attach_id)).unwrap();
    plane
        .feed(FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        })
        .unwrap();
    plane
        .feed(FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::new(),
        })
        .unwrap();
    plane
        .feed(FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        })
        .unwrap();
    plane.feed(FrameKind::AttachReady { attach_id }).unwrap();
}

fn attach_resources(plane: &mut ControlPlane) -> Vec<(u32, ResourceId)> {
    plane
        .take_outbound()
        .iter()
        .filter_map(|bytes| match decode(bytes) {
            FrameKind::Command {
                request_id,
                command: Command::AttachResource { terminal_id, .. },
            } => Some((request_id, terminal_id)),
            _ => None,
        })
        .collect()
}

#[test]
fn an_agent_session_is_subscribed_once_its_pane_is_streamed() {
    let (mut plane, attach_id) = subscribing(true);
    // The agent runs in the other session's pane, which this attach does
    // not stream.
    attach_to(&mut plane, attach_id, with_agent(ResourceId::local(8)));
    assert!(attach_resources(&mut plane).is_empty());

    let pane_request = plane.attach_terminal(&ResourceId::local(8));
    assert_eq!(
        attach_resources(&mut plane),
        vec![(pane_request, ResourceId::local(8))]
    );
    plane
        .feed(FrameKind::CommandResult {
            request_id: pane_request,
            result: CommandResult::Ok,
        })
        .expect("pane attached");
    let subscribed = attach_resources(&mut plane);
    assert_eq!(subscribed.len(), 1);
    assert_eq!(subscribed[0].1, ResourceId::local(AGENT));
}

#[test]
fn without_the_option_the_runtime_subscribes_no_agent_session() {
    let (mut plane, attach_id) = subscribing(false);
    attach_to(&mut plane, attach_id, with_agent(terminal()));
    assert!(attach_resources(&mut plane).is_empty());
}

fn agent_hello(resume: bool, incarnation: u8) -> FrameKind {
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
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

fn agent_requests(plane: &mut ControlPlane) -> Vec<(u32, Option<u64>)> {
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

fn reconnect_agents(
    plane: &mut ControlPlane,
    resume: bool,
    incarnation: u8,
) -> Vec<(u32, Option<u64>)> {
    plane.connection_lost(None);
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), None);
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(agent_hello(resume, incarnation)).unwrap();
    let attach_id = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .unwrap();
    attach_to(plane, attach_id, with_agent(terminal()));
    agent_requests(plane)
}

const fn agent_ids(generation: u64) -> (StreamId, BootstrapId) {
    (
        StreamId::new(100 + generation).unwrap(),
        BootstrapId::new(200 + generation).unwrap(),
    )
}

fn agent_begin(plane: &mut ControlPlane, generation: u64, after_seq: Option<u64>, base_seq: u64) {
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

fn agent_payload(records: &[(u64, &str)]) -> String {
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

fn agent_chunk(
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

fn agent_ready(plane: &mut ControlPlane, generation: u64) -> Result<(), ControlError> {
    let (stream_id, bootstrap_id) = agent_ids(generation);
    plane.feed(FrameKind::BootstrapReady {
        terminal_id: ResourceId::local(AGENT),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    })
}

fn agent_live(
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

fn take_agent_records(plane: &mut ControlPlane) -> Vec<(Vec<u64>, bool)> {
    plane
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            Event::AgentRecords {
                terminal_id,
                records,
                retained,
                ..
            } => {
                assert_eq!(terminal_id, ResourceId::local(AGENT));
                Some((
                    records.into_iter().map(|record| record.seq).collect(),
                    retained,
                ))
            }
            _ => None,
        })
        .collect()
}

fn commit_initial_agent(plane: &mut ControlPlane, request_id: u32) {
    // Reference-server ordering: the bootstrap can precede COMMAND_RESULT.
    agent_begin(plane, 1, None, 2);
    agent_chunk(plane, 1, 0, agent_payload(&[(1, "prompt"), (2, "ask")])).unwrap();
    agent_ready(plane, 1).unwrap();
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        })
        .unwrap();
    assert_eq!(take_agent_records(plane), [(vec![1, 2], true)]);
}

#[test]
fn same_incarnation_reconnect_requests_the_kernel_committed_agent_cursor() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, None, "no committed generation yet");
    commit_initial_agent(&mut plane, requests[0].0);
    let old = plane
        .engine()
        .unwrap()
        .agent_stream_info(&ResourceId::local(AGENT))
        .unwrap();
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, Some(2));
    assert_eq!(
        plane
            .engine()
            .unwrap()
            .agent_stream_info(&ResourceId::local(AGENT)),
        Some(old)
    );
    agent_begin(&mut plane, 2, Some(2), 4);
    agent_chunk(&mut plane, 2, 0, agent_payload(&[(3, "tool_start")])).unwrap();
    agent_chunk(&mut plane, 2, 1, agent_payload(&[(4, "provider_raw")])).unwrap();
    assert!(
        take_agent_records(&mut plane).is_empty(),
        "no staging publication"
    );
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), Some(2));
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![3, 4], false)]);
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), Some(4));
    agent_live(&mut plane, 2, &[(5, "stop")]).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![5], false)]);
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), Some(5));
    assert!(agent_live(&mut plane, 1, &[(3, "stop")]).is_err());
    assert!(take_agent_records(&mut plane).is_empty());
}

#[test]
fn missing_resume_feature_requests_a_full_agent_bootstrap_and_eviction_replaces() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    commit_initial_agent(&mut plane, requests[0].0);
    let requests = reconnect_agents(&mut plane, false, 0xAB);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, None);
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), None);
    // Also the response to a resume request whose cursor was evicted.
    agent_begin(&mut plane, 2, None, 5);
    agent_chunk(
        &mut plane,
        2,
        0,
        agent_payload(&[(4, "prompt"), (5, "stop")]),
    )
    .unwrap();
    assert!(take_agent_records(&mut plane).is_empty());
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![4, 5], true)]);
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests[0].1, Some(5));
    agent_begin(&mut plane, 3, None, 8);
    agent_chunk(
        &mut plane,
        3,
        0,
        agent_payload(&[(7, "prompt"), (8, "stop")]),
    )
    .unwrap();
    agent_ready(&mut plane, 3).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![7, 8], true)]);
}

#[test]
fn new_incarnation_forgets_agent_cursors_before_subscribing_recycled_ids() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    commit_initial_agent(&mut plane, requests[0].0);
    let requests = reconnect_agents(&mut plane, true, 0xAC);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, None);
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), None);
    assert!(
        plane
            .engine()
            .unwrap()
            .agent_stream_info(&ResourceId::local(AGENT))
            .is_none()
    );
    assert_eq!(
        plane.take_events().into_iter().filter(|event| matches!(event, Event::AgentSessionClosed { terminal_id, .. } if *terminal_id == ResourceId::local(AGENT))).count(),
        1
    );
    agent_begin(&mut plane, 1, None, 1);
    agent_chunk(&mut plane, 1, 0, agent_payload(&[(1, "prompt")])).unwrap();
    agent_ready(&mut plane, 1).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![1], true)]);
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), Some(1));
}

#[test]
fn zero_missed_agent_records_resume_without_duplicates_then_handoff_to_live() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    commit_initial_agent(&mut plane, requests[0].0);
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests[0].1, Some(2));
    agent_begin(&mut plane, 2, Some(2), 2);
    agent_ready(&mut plane, 2).unwrap();
    assert!(take_agent_records(&mut plane).is_empty());
    let committed = plane
        .engine()
        .unwrap()
        .agent_stream_info(&ResourceId::local(AGENT))
        .unwrap();
    assert_eq!((committed.stream_id, committed.bootstrap_id), agent_ids(2));
    assert_eq!(committed.last_seq, 2);
    agent_live(&mut plane, 2, &[(3, "stop")]).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![3], false)]);
    // Explicit same-connection reattach also appends only the missed suffix.
    agent_begin(&mut plane, 3, Some(3), 4);
    agent_chunk(&mut plane, 3, 0, agent_payload(&[(4, "prompt")])).unwrap();
    agent_ready(&mut plane, 3).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![4], false)]);
    agent_live(&mut plane, 3, &[(5, "stop")]).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![5], false)]);
}

#[test]
fn failed_agent_delta_bootstraps_never_publish_or_advance_a_resume_cursor() {
    for (chunk_seq, payload) in [
        (0, agent_payload(&[(3, "stop"), (5, "ask")])),
        (0, agent_payload(&[(3, "stop"), (3, "ask")])),
        (0, agent_payload(&[(4, "stop")])),
        (0, agent_payload(&[(3, "stop")])),
        (0, String::new()),
        (0, String::from("{malformed}\n")),
        (0, agent_payload(&[(3, "stop")]) + "{\"seq\":4}\n"),
        (1, agent_payload(&[(3, "stop"), (4, "ask")])),
    ] {
        let (mut plane, attach_id) = subscribing_with_resume(true, true);
        attach_to(&mut plane, attach_id, with_agent(terminal()));
        let requests = agent_requests(&mut plane);
        commit_initial_agent(&mut plane, requests[0].0);
        let committed = plane
            .engine()
            .unwrap()
            .agent_stream_info(&ResourceId::local(AGENT));
        agent_begin(&mut plane, 2, Some(2), 4);
        let _ = agent_chunk(&mut plane, 2, chunk_seq, payload);
        assert!(agent_ready(&mut plane, 2).is_err());
        assert!(take_agent_records(&mut plane).is_empty());
        assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), Some(2));
        assert_eq!(
            plane
                .engine()
                .unwrap()
                .agent_stream_info(&ResourceId::local(AGENT)),
            committed
        );
        let requests = reconnect_agents(&mut plane, true, 0xAB);
        assert_eq!(requests[0].1, Some(2));
        agent_begin(&mut plane, 3, Some(2), 3);
        agent_chunk(&mut plane, 3, 0, agent_payload(&[(3, "stop")])).unwrap();
        agent_ready(&mut plane, 3).unwrap();
        assert_eq!(take_agent_records(&mut plane), [(vec![3], false)]);
    }
}

#[test]
fn empty_full_agent_bootstrap_is_a_consumer_visible_replacement() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    commit_initial_agent(&mut plane, requests[0].0);
    agent_begin(&mut plane, 2, None, 2);
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![], true)]);
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), Some(2));
}

#[test]
fn reconnect_before_agent_ready_requests_full_instead_of_unverified_records() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_to(&mut plane, attach_id, with_agent(terminal()));
    assert_eq!(agent_requests(&mut plane)[0].1, None);
    agent_begin(&mut plane, 1, None, 3);
    agent_chunk(
        &mut plane,
        1,
        0,
        agent_payload(&[(1, "prompt"), (2, "ask")]),
    )
    .unwrap();
    assert!(take_agent_records(&mut plane).is_empty());
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), None);
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests[0].1, None);
    agent_begin(&mut plane, 2, None, 3);
    agent_chunk(
        &mut plane,
        2,
        0,
        agent_payload(&[(1, "prompt"), (2, "ask"), (3, "stop")]),
    )
    .unwrap();
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![1, 2, 3], true)]);
    assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), Some(3));
}

#[test]
fn resource_close_and_parent_detach_clear_the_committed_agent_cursor() {
    for close in [true, false] {
        let (mut plane, attach_id) = subscribing_with_resume(true, true);
        attach_to(&mut plane, attach_id, with_agent(terminal()));
        let requests = agent_requests(&mut plane);
        commit_initial_agent(&mut plane, requests[0].0);
        if close {
            plane
                .feed(FrameKind::ResourceClosed {
                    terminal_id: ResourceId::local(AGENT),
                    exit_status: None,
                    signal: None,
                    reason: phux_protocol::wire::frame::CloseReason::Unknown,
                })
                .unwrap();
        } else {
            let request_id = plane.detach_terminal(&terminal());
            plane
                .feed(FrameKind::CommandResult {
                    request_id,
                    result: CommandResult::Ok,
                })
                .unwrap();
        }
        assert_eq!(plane.agent_after_seq(&ResourceId::local(AGENT)), None);
        assert!(
            plane
                .engine()
                .unwrap()
                .agent_stream_info(&ResourceId::local(AGENT))
                .is_none()
        );
        assert_eq!(
            plane.take_events().into_iter().filter(|event| matches!(event, Event::AgentSessionClosed { terminal_id, .. } if *terminal_id == ResourceId::local(AGENT))).count(),
            1
        );
    }
}
