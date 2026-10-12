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
        assert_eq!(
            plane.take_events().into_iter().filter(|event| matches!(event, Event::AgentSessionClosed { terminal_id, .. } if *terminal_id == ResourceId::local(AGENT))).count(),
            1
        );
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
