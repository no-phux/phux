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
}

fn attach_ready_to(plane: &mut ControlPlane, attach_id: u32, snapshot: SessionSnapshot) {
    let focused = snapshot.focused_resource.clone();
    attach_to(plane, attach_id, snapshot);
    ready(plane, attach_id, focused);
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
    attach_ready_to(plane, attach_id, with_agent(terminal()));
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
    publications_of(plane.take_events())
}

fn lease_agent_records(
    plane: &mut ControlPlane,
) -> Vec<(
    Vec<u64>,
    bool,
    std::sync::Arc<phux_client_runtime::control::AgentPublication>,
)> {
    plane
        .take_events_leased()
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
                    publication,
                ))
            }
            _ => None,
        })
        .collect()
}

fn publications_of(events: Vec<Event>) -> Vec<(Vec<u64>, bool)> {
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
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
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
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
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
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
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
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    commit_initial_agent(&mut plane, requests[0].0);
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests[0].1, Some(2));
    agent_begin(&mut plane, 2, Some(2), 2);
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![], false)]);
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
        attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
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
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
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
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
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
        attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
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
        let closed: Vec<_> = plane
            .take_events()
            .into_iter()
            .filter_map(|event| match event {
                Event::AgentSessionClosed {
                    terminal_id,
                    stream,
                    ..
                } if terminal_id == ResourceId::local(AGENT) => Some(stream),
                _ => None,
            })
            .collect();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].map(|stream| stream.last_seq), Some(2));
    }
}

fn ready(plane: &mut ControlPlane, attach_id: u32, parent: ResourceId) {
    let stream_id = StreamId::new(u64::from(attach_id)).unwrap();
    let bootstrap_id = BootstrapId::new(u64::from(attach_id)).unwrap();
    plane
        .feed(FrameKind::BootstrapBegin {
            terminal_id: parent.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 20,
            rows: 4,
            base_seq: 0,
        })
        .unwrap();
    plane
        .feed(FrameKind::BootstrapReady {
            terminal_id: parent,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        })
        .unwrap();
    plane.feed(FrameKind::AttachReady { attach_id }).unwrap();
}

fn close_count(plane: &mut ControlPlane) -> usize {
    let events = plane.take_events();
    assert!(!events.iter().any(|event| matches!(event,
        Event::TerminalClosed { terminal_id, .. } if *terminal_id == ResourceId::local(AGENT)
    )));
    events
        .iter()
        .filter(|event| {
            matches!(event, Event::AgentSessionClosed { terminal_id, .. }
            if *terminal_id == ResourceId::local(AGENT))
        })
        .count()
}

fn reconnect_to_beta(plane: &mut ControlPlane) -> u32 {
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let attach_id = outbound_attach_id(plane);
    // A caller can request inventory before ATTACHED. That read must not
    // turn an in-progress session attach into a ready browsing connection.
    let request_id = plane.refresh_topology().unwrap();
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(beta_snapshot())),
        })
        .unwrap();
    assert_eq!(plane.status(), Status::Negotiated);
    assert_eq!(close_count(plane), 0);
    attach_to(plane, attach_id, beta_snapshot());
    attach_id
}

fn beta_snapshot() -> SessionSnapshot {
    let mut snapshot = with_agent(terminal());
    snapshot.focused_session = SessionId::new(2);
    snapshot.focused_window = WindowId::new(2);
    snapshot.focused_resource = ResourceId::local(8);
    snapshot
}

fn followed_home() -> ControlPlane {
    let (mut plane, attach_id) = subscribing(true);
    attach_to(&mut plane, attach_id, with_agent(terminal()));
    let subscriptions = attach_resources(&mut plane);
    assert_eq!(subscriptions.len(), 1);
    plane
        .feed(FrameKind::CommandResult {
            request_id: subscriptions[0].0,
            result: CommandResult::Ok,
        })
        .unwrap();
    ready(&mut plane, attach_id, terminal());
    let _ = plane.take_outbound();
    assert_eq!(close_count(&mut plane), 0);
    assert!(!plane.attach_session(AttachTarget::ByName("beta".into())));
    let requests = attach_resources(&mut plane);
    for (request_id, _) in requests {
        plane
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            })
            .unwrap();
    }
    assert_eq!(close_count(&mut plane), 0, "live switch retains home pumps");
    plane
}

#[test]
fn reconnect_after_switch_retires_home_subscription_once_and_can_follow_it_again() {
    let mut plane = followed_home();
    let attach_id = reconnect_to_beta(&mut plane);
    assert_eq!(close_count(&mut plane), 0, "wait for ATTACH_READY");
    ready(&mut plane, attach_id, ResourceId::local(8));
    assert_eq!(close_count(&mut plane), 1);
    assert!(!plane.engine().unwrap().is_closed(&ResourceId::local(AGENT)));

    let request_id = plane.refresh_topology().unwrap();
    // ATTACHED fences its earlier inventory; answer that and then the fresh read.
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(with_agent(terminal()))),
        })
        .unwrap();
    assert_eq!(close_count(&mut plane), 0);
    let request_id = plane.attach_terminal(&terminal());
    let _ = plane.take_outbound();
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        })
        .unwrap();
    let subscriptions = attach_resources(&mut plane);
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].1, ResourceId::local(AGENT));
    assert_eq!(close_count(&mut plane), 0);
}

#[test]
fn reconnect_waits_for_preserving_parent_admission_and_retires_only_if_refused() {
    for refused in [false, true] {
        let (mut plane, attach_id) = subscribing(true);
        attach_to(&mut plane, attach_id, beta_snapshot());
        ready(&mut plane, attach_id, ResourceId::local(8));
        // Explicit geometry-preserving admission outside the attached session
        // survives reconnect, unlike an ordinary old home/spawn pump.
        let request_id = plane.attach_terminal_preserving_geometry(&terminal());
        plane
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            })
            .unwrap();
        let subscriptions = attach_resources(&mut plane);
        let agent_request = subscriptions
            .iter()
            .find(|(_, id)| *id == ResourceId::local(AGENT))
            .unwrap()
            .0;
        plane
            .feed(FrameKind::CommandResult {
                request_id: agent_request,
                result: CommandResult::Ok,
            })
            .unwrap();
        // Set the reconnect target to the current home session.
        assert!(!plane.attach_session(AttachTarget::ByName("beta".into())));
        let _ = plane.take_outbound();
        let _ = plane.take_events();
        let attach_id = reconnect_to_beta(&mut plane);
        ready(&mut plane, attach_id, ResourceId::local(8));
        assert_eq!(close_count(&mut plane), 0, "replayed attach still pending");
        let requests = attach_resources(&mut plane);
        let request_id = requests.iter().find(|(_, id)| *id == terminal()).unwrap().0;
        // An inventory may optimistically subscribe the agent while its
        // parent's replay is pending; a refusal must still retire it.
        let inventory_request = plane.refresh_topology().unwrap();
        plane
            .feed(FrameKind::CommandResult {
                request_id: inventory_request,
                result: CommandResult::OkWith(CommandValue::State(beta_snapshot())),
            })
            .unwrap();
        let subscriptions = attach_resources(&mut plane);
        assert_eq!(subscriptions.len(), 1);
        assert_eq!(subscriptions[0].1, ResourceId::local(AGENT));
        plane
            .feed(FrameKind::CommandResult {
                request_id: subscriptions[0].0,
                result: CommandResult::Ok,
            })
            .unwrap();
        assert_eq!(close_count(&mut plane), 0);
        plane
            .feed(FrameKind::CommandResult {
                request_id,
                result: if refused {
                    CommandResult::Error {
                        code: ErrorCode::PermissionDenied,
                        message: "denied".into(),
                    }
                } else {
                    CommandResult::Ok
                },
            })
            .unwrap();
        assert_eq!(close_count(&mut plane), usize::from(refused));
        assert!(attach_resources(&mut plane).is_empty());
    }
}

fn followed_own_spawn() -> ControlPlane {
    let (mut plane, attach_id) = subscribing(true);
    let mut before_spawn = beta_snapshot();
    before_spawn
        .resources
        .retain(|resource| resource.id != terminal() && resource.id != ResourceId::local(AGENT));
    attach_to(&mut plane, attach_id, before_spawn);
    ready(&mut plane, attach_id, ResourceId::local(8));
    let request_id = plane.spawn_terminal(SpawnRequest {
        session_id: Some(1),
        ..SpawnRequest::default()
    });
    plane
        .feed(FrameKind::ResourceSpawned {
            request_id,
            result: SpawnResult::Ok(terminal()),
        })
        .unwrap();
    let request_id = plane.refresh_topology().unwrap();
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(beta_snapshot())),
        })
        .unwrap();
    let subscriptions = attach_resources(&mut plane);
    let request_id = subscriptions
        .iter()
        .find(|(_, id)| *id == ResourceId::local(AGENT))
        .unwrap()
        .0;
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        })
        .unwrap();
    assert!(!plane.attach_session(AttachTarget::ByName("beta".into())));
    let _ = plane.take_events();
    plane
}

#[test]
fn reconnect_does_not_treat_old_own_spawn_as_a_current_parent_admission() {
    let mut plane = followed_own_spawn();
    let attach_id = reconnect_to_beta(&mut plane);
    assert_eq!(close_count(&mut plane), 0);
    ready(&mut plane, attach_id, ResourceId::local(8));
    assert_eq!(close_count(&mut plane), 1);
    assert!(!plane.terminal_is_admitted(&terminal()));
    assert!(!plane.engine().unwrap().is_closed(&ResourceId::local(AGENT)));
}

#[test]
fn replayed_spawn_admission_settles_before_orphan_retirement() {
    for admitted in [false, true] {
        let mut plane = followed_own_spawn();
        // Admission settlement includes keyed spawns, even outside the new
        // home session. This spawn is distinct from the old parent @7.
        let request_id = plane.spawn_terminal(SpawnRequest {
            session_id: Some(1),
            ..SpawnRequest::default()
        });
        let _ = plane.take_outbound();
        assert_eq!(close_count(&mut plane), 0);
        plane.connection_opened();
        let _ = plane.take_outbound();
        plane
            .feed(hello_ok_with(&[ServerFeature::SpawnIdempotency]))
            .unwrap();
        let attach_id = outbound_attach_id(&mut plane);
        attach_to(&mut plane, attach_id, beta_snapshot());
        ready(&mut plane, attach_id, ResourceId::local(8));
        assert_eq!(close_count(&mut plane), 0, "spawn replay remains pending");
        assert!(plane.take_outbound().iter().any(|bytes| matches!(decode(bytes), FrameKind::SpawnResource { request_id: id, .. } if id == request_id)));
        plane
            .feed(FrameKind::ResourceSpawned {
                request_id,
                result: if admitted {
                    SpawnResult::Replayed {
                        id: ResourceId::local(9),
                        instance: None,
                    }
                } else {
                    SpawnResult::Err(phux_protocol::wire::frame::SpawnError::GroupNotFound)
                },
            })
            .unwrap();
        assert_eq!(close_count(&mut plane), 1);
        assert_eq!(plane.terminal_is_admitted(&ResourceId::local(9)), admitted);
        assert_eq!(close_count(&mut plane), 0);
    }
}

#[test]
fn refused_parent_detach_preserves_agent_subscription_and_success_releases_once() {
    for refused in [false, true] {
        let (mut plane, attach_id) = subscribing(true);
        attach_to(&mut plane, attach_id, with_agent(ResourceId::local(8)));
        ready(&mut plane, attach_id, terminal());
        let request_id = plane.attach_terminal(&ResourceId::local(8));
        plane
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            })
            .unwrap();
        let subscriptions = attach_resources(&mut plane);
        let agent_request = subscriptions
            .iter()
            .find(|(_, id)| *id == ResourceId::local(AGENT))
            .unwrap()
            .0;
        plane
            .feed(FrameKind::CommandResult {
                request_id: agent_request,
                result: CommandResult::Ok,
            })
            .unwrap();
        assert_eq!(close_count(&mut plane), 0);

        let detach_request = plane.detach_terminal(&ResourceId::local(8));
        let inventory_request = plane.refresh_topology().unwrap();
        let _ = plane.take_outbound();
        plane
            .feed(FrameKind::CommandResult {
                request_id: inventory_request,
                result: CommandResult::OkWith(CommandValue::State(with_agent(ResourceId::local(
                    8,
                )))),
            })
            .unwrap();
        assert_eq!(close_count(&mut plane), 0, "detach admission is unsettled");
        plane
            .feed(FrameKind::CommandResult {
                request_id: detach_request,
                result: if refused {
                    CommandResult::Error {
                        code: ErrorCode::PermissionDenied,
                        message: "parent detach refused".into(),
                    }
                } else {
                    CommandResult::Ok
                },
            })
            .unwrap();
        assert_eq!(plane.terminal_is_admitted(&ResourceId::local(8)), refused);
        assert_eq!(close_count(&mut plane), usize::from(!refused));
        let releases = plane
            .take_outbound()
            .iter()
            .filter(|bytes| {
                matches!(decode(bytes), FrameKind::Command {
                command: Command::DetachResource { terminal_id }, ..
            } if terminal_id == ResourceId::local(AGENT))
            })
            .count();
        assert_eq!(releases, usize::from(!refused));
        assert_eq!(close_count(&mut plane), 0);

        // Refusing a detach for a never-admitted resource must not grant a
        // parent admission as a side effect of the restoration path.
        let request_id = plane.detach_terminal(&ResourceId::local(99));
        plane
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Error {
                    code: ErrorCode::PermissionDenied,
                    message: "unknown parent".into(),
                },
            })
            .unwrap();
        assert!(!plane.terminal_is_admitted(&ResourceId::local(99)));
        assert_eq!(close_count(&mut plane), 0);
    }
}

#[test]
fn parent_close_during_detach_cannot_be_undone_by_a_late_refusal() {
    let parent = ResourceId::local(8);
    let (mut plane, attach_id) = subscribing(true);
    attach_to(&mut plane, attach_id, with_agent(parent.clone()));
    ready(&mut plane, attach_id, terminal());
    let request_id = plane.attach_terminal(&parent);
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        })
        .unwrap();
    let subscriptions = attach_resources(&mut plane);
    let agent_request = subscriptions
        .iter()
        .find(|(_, id)| *id == ResourceId::local(AGENT))
        .unwrap()
        .0;
    plane
        .feed(FrameKind::CommandResult {
            request_id: agent_request,
            result: CommandResult::Ok,
        })
        .unwrap();
    assert_eq!(close_count(&mut plane), 0);

    let detach_request = plane.detach_terminal(&parent);
    let _ = plane.take_outbound();
    plane
        .feed(FrameKind::ResourceClosed {
            terminal_id: parent.clone(),
            exit_status: Some(0),
            signal: None,
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
        })
        .unwrap();
    assert!(plane.engine().unwrap().is_closed(&parent));
    plane
        .feed(FrameKind::CommandResult {
            request_id: detach_request,
            result: CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                message: "detach refused after parent closed".into(),
            },
        })
        .unwrap();
    assert!(!plane.terminal_is_admitted(&parent));
    assert_eq!(close_count(&mut plane), 1);
    assert!(attach_resources(&mut plane).is_empty());

    // An unversioned inventory can still contain the old AgentSession.
    // Repeated reads must not attach it under the closed parent.
    for _ in 0..3 {
        let request_id = plane.refresh_topology().unwrap();
        plane
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::State(with_agent(parent.clone()))),
            })
            .unwrap();
        assert!(attach_resources(&mut plane).is_empty());
        assert_eq!(close_count(&mut plane), 0);
    }
}

fn replace_without_loss(plane: &mut ControlPlane) {
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(agent_hello(true, 0xAB)).unwrap();
    let attach_id = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .unwrap();
    attach_ready_to(plane, attach_id, with_agent(terminal()));
}

fn lease_two_live_frames(
    plane: &mut ControlPlane,
) -> [std::sync::Arc<phux_client_runtime::control::AgentPublication>; 2] {
    agent_live(plane, 1, &[(3, "tool_start")]).unwrap();
    let first = lease_agent_records(plane);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].0, vec![3]);
    agent_live(plane, 1, &[(4, "tool_end")]).unwrap();
    let second = lease_agent_records(plane);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].0, vec![4]);
    [
        first.into_iter().next().unwrap().2,
        second.into_iter().next().unwrap().2,
    ]
}

#[test]
fn leased_internal_frames_replay_in_order_after_replacement_without_loss() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    commit_initial_agent(&mut plane, requests[0].0);
    let leased = lease_two_live_frames(&mut plane);
    // Two internal drains, neither accepted: the adapter has not cleared
    // effects. Replacement can open without a preceding connection_lost.
    replace_without_loss(&mut plane);
    let replay = lease_agent_records(&mut plane);
    assert_eq!(
        replay.iter().map(|row| row.0.clone()).collect::<Vec<_>>(),
        vec![vec![3], vec![4]]
    );
    assert!(std::sync::Arc::ptr_eq(&replay[0].2, &leased[0]));
    assert!(std::sync::Arc::ptr_eq(&replay[1].2, &leased[1]));
    let requests = agent_requests(&mut plane);
    assert_eq!(requests[0].1, Some(4));
    agent_begin(&mut plane, 2, Some(4), 5);
    agent_chunk(&mut plane, 2, 0, agent_payload(&[(5, "stop")])).unwrap();
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![5], false)]);
}

#[test]
fn explicit_accept_after_loss_removes_the_queued_replay() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    commit_initial_agent(&mut plane, requests[0].0);
    agent_live(&mut plane, 1, &[(3, "tool_start")]).unwrap();
    agent_live(&mut plane, 1, &[(4, "tool_end")]).unwrap();
    let leased = lease_agent_records(&mut plane);
    assert_eq!(
        leased.iter().map(|row| row.0.clone()).collect::<Vec<_>>(),
        vec![vec![3], vec![4]]
    );
    plane.connection_lost(None);
    replace_without_loss(&mut plane);
    // Readiness has queued the replay. Clearing one borrowed batch accepts
    // only that allocation; the uncleared sibling still replays.
    plane.accept_agent_publication(&leased[0].2);
    let replay = lease_agent_records(&mut plane);
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].0, vec![4]);
    assert!(std::sync::Arc::ptr_eq(&replay[0].2, &leased[1].2));
    plane.accept_agent_publication(&replay[0].2);
    replace_without_loss(&mut plane);
    assert!(lease_agent_records(&mut plane).is_empty());
}

#[test]
fn close_and_detach_do_not_replay_leased_publications() {
    for close in [true, false] {
        let (mut plane, attach_id) = subscribing_with_resume(true, true);
        attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
        let requests = agent_requests(&mut plane);
        commit_initial_agent(&mut plane, requests[0].0);
        agent_live(&mut plane, 1, &[(3, "stop")]).unwrap();
        let _leased = lease_agent_records(&mut plane);
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
        let closed: Vec<_> = plane
            .take_events_leased()
            .into_iter()
            .filter_map(|event| match event {
                Event::AgentSessionClosed {
                    terminal_id,
                    stream,
                    ..
                } if terminal_id == ResourceId::local(AGENT) => Some(stream),
                _ => None,
            })
            .collect();
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].map(|stream| stream.last_seq), Some(3));
        replace_without_loss(&mut plane);
        assert!(lease_agent_records(&mut plane).is_empty());
    }
}

fn replace_attached(plane: &mut ControlPlane, snapshot: SessionSnapshot) {
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(agent_hello(true, 0xAB)).unwrap();
    let attach_id = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .unwrap();
    attach_ready_to(plane, attach_id, snapshot);
}

fn declare_agent(plane: &mut ControlPlane) {
    plane
        .apply_engine_event(
            phux_client_runtime::engine::EngineEvent::AgentSessionDeclared {
                terminal_id: ResourceId::local(AGENT),
                parent: Some(terminal()),
                provider: Some("claude".into()),
                native_id: None,
                state: Some("working".into()),
            },
        )
        .unwrap();
}

fn publish_agent(plane: &mut ControlPlane) {
    agent_begin(plane, 1, None, 2);
    agent_chunk(plane, 1, 0, agent_payload(&[(1, "prompt"), (2, "ask")])).unwrap();
    agent_ready(plane, 1).unwrap();
}

#[test]
fn option_false_omitted_attach_withholds_until_explicit_declare() {
    let (mut plane, attach_id) = subscribing(false);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    publish_agent(&mut plane);
    let leased = lease_agent_records(&mut plane);
    assert_eq!(leased.len(), 1);
    assert_eq!(leased[0].0, vec![1, 2]);
    let cursor = plane
        .engine()
        .unwrap()
        .agent_stream_info(&ResourceId::local(AGENT))
        .unwrap();
    replace_attached(&mut plane, two_session_snapshot(false));
    assert!(
        lease_agent_records(&mut plane).is_empty(),
        "a partial ATTACHED must not release an omitted agent"
    );
    assert_eq!(
        plane
            .engine()
            .unwrap()
            .agent_stream_info(&ResourceId::local(AGENT)),
        Some(cursor),
        "a partial ATTACHED must not drop the owner cursor"
    );
    declare_agent(&mut plane);
    let replay = lease_agent_records(&mut plane);
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].0, vec![1, 2]);
    assert!(std::sync::Arc::ptr_eq(&replay[0].2, &leased[0].2));
}

#[test]
fn option_false_authoritative_omit_forgets_the_cursor_and_does_not_resurrect() {
    for accepted in [false, true] {
        let (mut plane, attach_id) = subscribing(false);
        attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
        publish_agent(&mut plane);
        let leased = lease_agent_records(&mut plane);
        assert_eq!(leased[0].0, vec![1, 2]);
        if accepted {
            plane.accept_agent_publication(&leased[0].2);
        }
        replace_attached(&mut plane, two_session_snapshot(false));
        assert!(lease_agent_records(&mut plane).is_empty());
        let omitted = two_session_snapshot(false);
        plane.reconcile_agent_delivery_catalog(&omitted);
        let after = plane.take_events_leased();
        assert!(after.iter().all(|event| !matches!(
        event,
        Event::AgentSessionClosed { terminal_id, .. } | Event::TerminalClosed { terminal_id, .. }
            if *terminal_id == ResourceId::local(AGENT)
    )));
        assert!(lease_agent_records(&mut plane).is_empty());
        assert!(
            plane
                .engine()
                .unwrap()
                .agent_stream_info(&ResourceId::local(AGENT))
                .is_none()
        );
        declare_agent(&mut plane);
        assert!(
            lease_agent_records(&mut plane).is_empty(),
            "discarded rows must not resurrect on re-admit"
        );
        agent_begin(&mut plane, 2, None, 1);
        agent_chunk(&mut plane, 2, 0, agent_payload(&[(1, "prompt")])).unwrap();
        agent_ready(&mut plane, 2).unwrap();
        assert_eq!(take_agent_records(&mut plane), [(vec![1], true)]);
    }
}

#[test]
fn authoritative_registry_omission_drops_queued_records_before_delivery() {
    let (mut plane, attach_id) = subscribing(false);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    publish_agent(&mut plane);
    assert_eq!(
        plane
            .engine()
            .unwrap()
            .agent_stream_info(&ResourceId::local(AGENT))
            .unwrap()
            .last_seq,
        2
    );
    plane.reconcile_agent_delivery_catalog(&two_session_snapshot(false));
    assert!(
        take_agent_records(&mut plane).is_empty(),
        "an absent resource must not return through a queued publication"
    );
    assert!(
        plane
            .engine()
            .unwrap()
            .agent_stream_info(&ResourceId::local(AGENT))
            .is_none()
    );
}

#[test]
fn authoritative_rebind_publishes_catalog_before_held_agent_records() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    commit_initial_agent(&mut plane, requests[0].0);
    agent_live(&mut plane, 1, &[(3, "tool_start")]).unwrap();
    let leased = lease_agent_records(&mut plane);
    replace_attached(&mut plane, two_session_snapshot(false));
    assert!(lease_agent_records(&mut plane).is_empty());
    let request_id = plane.refresh_topology().unwrap();
    // ATTACHED fences the inventory queued before its admission barrier.
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(two_session_snapshot(false))),
        })
        .unwrap();
    let request_id = plane.refresh_topology().unwrap();
    let _ = plane.take_events_leased();
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(with_agent(terminal()))),
        })
        .unwrap();
    let events = plane.take_events_leased();
    let catalog = events
        .iter()
        .position(|event| matches!(event, Event::TopologySnapshot { .. }))
        .expect("authoritative catalog");
    let records = events
        .iter()
        .position(|event| {
            matches!(
                event,
                Event::AgentPublication { publication, .. }
                    if std::sync::Arc::ptr_eq(publication, &leased[0].2)
            )
        })
        .expect("held publication");
    assert!(
        catalog < records,
        "a consumer must discover the resource before receiving its records"
    );
}
