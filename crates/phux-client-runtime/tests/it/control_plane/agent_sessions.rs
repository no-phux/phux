//! `ControlOptions::subscribe_agent_sessions`: which `AgentSession` streams
//! the runtime subscribes itself.

use super::*;
use phux_protocol::AgentFacet;

const AGENT: u32 = 31;

mod own_spawn;
mod stream;
use stream::*;

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
    reopen(&mut plane, agent_hello(resume, 0xAB));
    let attach_id = plane.queued_attach_id();
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
    plane.attached(attach_id, snapshot);
}

fn attach_ready_to(plane: &mut ControlPlane, attach_id: u32, snapshot: SessionSnapshot) {
    let focused = snapshot.focused_resource.clone();
    attach_to(plane, attach_id, snapshot);
    ready(plane, attach_id, &focused);
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
    // The agent runs in a pane this attach does not stream.
    attach_to(&mut plane, attach_id, with_agent(ResourceId::local(8)));
    assert!(attach_resources(&mut plane).is_empty());

    let pane_request = plane.attach_terminal(&ResourceId::local(8));
    assert_eq!(
        attach_resources(&mut plane),
        vec![(pane_request, ResourceId::local(8))]
    );
    plane.reply(pane_request, CommandResult::Ok);
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

#[test]
fn same_incarnation_reconnect_requests_the_kernel_committed_agent_cursor() {
    let (mut plane, attach_id) = subscribing_with_resume(true, true);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    let requests = agent_requests(&mut plane);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, None, "no committed generation yet");
    commit_initial_agent(&mut plane, requests[0].0);
    let old = agent_info(&plane).unwrap();
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, Some(2));
    assert_eq!(agent_info(&plane), Some(old));
    agent_begin(&mut plane, 2, Some(2), 4);
    agent_chunk(&mut plane, 2, 0, agent_payload(&[(3, "tool_start")])).unwrap();
    agent_chunk(&mut plane, 2, 1, agent_payload(&[(4, "provider_raw")])).unwrap();
    assert!(
        take_agent_records(&mut plane).is_empty(),
        "no staging publication"
    );
    assert_eq!(agent_cursor(&plane), Some(2));
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![3, 4], false)]);
    assert_eq!(agent_cursor(&plane), Some(4));
    agent_live(&mut plane, 2, &[(5, "stop")]).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![5], false)]);
    assert_eq!(agent_cursor(&plane), Some(5));
    assert!(agent_live(&mut plane, 1, &[(3, "stop")]).is_err());
    assert!(take_agent_records(&mut plane).is_empty());
}

#[test]
fn a_full_bootstrap_answering_an_evicted_resume_cursor_replaces() {
    let mut plane = resumed_with_initial_agent();
    assert_eq!(reconnect_agents(&mut plane, true, 0xAB).len(), 1);
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
    agent_bootstrap(&mut plane, 3, None, &[(7, "prompt"), (8, "stop")]);
    assert_eq!(take_agent_records(&mut plane), [(vec![7, 8], true)]);
}

#[test]
fn new_incarnation_forgets_agent_cursors_before_subscribing_recycled_ids() {
    let mut plane = resumed_with_initial_agent();
    let requests = reconnect_agents(&mut plane, true, 0xAC);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].1, None);
    assert_eq!(agent_cursor(&plane), None);
    assert!(agent_info(&plane).is_none());
    assert_eq!(
        plane.take_events().into_iter().filter(|event| matches!(event, Event::AgentSessionClosed { terminal_id, .. } if *terminal_id == ResourceId::local(AGENT))).count(),
        1
    );
    agent_bootstrap(&mut plane, 1, None, &[(1, "prompt")]);
    assert_eq!(take_agent_records(&mut plane), [(vec![1], true)]);
    assert_eq!(agent_cursor(&plane), Some(1));
}

#[test]
fn zero_missed_agent_records_resume_without_duplicates_then_handoff_to_live() {
    let mut plane = resumed_with_initial_agent();
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests[0].1, Some(2));
    agent_begin(&mut plane, 2, Some(2), 2);
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![], false)]);
    let committed = agent_info(&plane).unwrap();
    assert_eq!((committed.stream_id, committed.bootstrap_id), agent_ids(2));
    assert_eq!(committed.last_seq, 2);
    agent_live(&mut plane, 2, &[(3, "stop")]).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![3], false)]);
    // Explicit same-connection reattach also appends only the missed suffix.
    agent_bootstrap(&mut plane, 3, Some(3), &[(4, "prompt")]);
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
        let mut plane = resumed_with_initial_agent();
        let committed = agent_info(&plane);
        agent_begin(&mut plane, 2, Some(2), 4);
        let _ = agent_chunk(&mut plane, 2, chunk_seq, payload);
        assert!(agent_ready(&mut plane, 2).is_err());
        assert!(take_agent_records(&mut plane).is_empty());
        assert_eq!(agent_cursor(&plane), Some(2));
        assert_eq!(agent_info(&plane), committed);
        let requests = reconnect_agents(&mut plane, true, 0xAB);
        assert_eq!(requests[0].1, Some(2));
        agent_bootstrap(&mut plane, 3, Some(2), &[(3, "stop")]);
        assert_eq!(take_agent_records(&mut plane), [(vec![3], false)]);
    }
}

#[test]
fn empty_full_agent_bootstrap_is_a_consumer_visible_replacement() {
    let mut plane = resumed_with_initial_agent();
    agent_begin(&mut plane, 2, None, 2);
    agent_ready(&mut plane, 2).unwrap();
    assert_eq!(take_agent_records(&mut plane), [(vec![], true)]);
    assert_eq!(agent_cursor(&plane), Some(2));
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
    assert_eq!(agent_cursor(&plane), None);
    let requests = reconnect_agents(&mut plane, true, 0xAB);
    assert_eq!(requests[0].1, None);
    agent_bootstrap(
        &mut plane,
        2,
        None,
        &[(1, "prompt"), (2, "ask"), (3, "stop")],
    );
    assert_eq!(take_agent_records(&mut plane), [(vec![1, 2, 3], true)]);
    assert_eq!(agent_cursor(&plane), Some(3));
}

#[test]
fn resource_close_and_parent_detach_clear_the_committed_agent_cursor() {
    for close in [true, false] {
        let mut plane = resumed_with_initial_agent();
        let closed = retire_agent(&mut plane, close);
        assert_eq!(agent_cursor(&plane), None, "close={close}");
        assert!(agent_info(&plane).is_none(), "close={close}");
        assert_eq!(closed, [Some(2)], "close={close}");
    }
}

fn ready(plane: &mut ControlPlane, attach_id: u32, parent: &ResourceId) {
    let generation = u64::from(attach_id);
    empty_bootstrap(plane, parent, [generation, generation]);
    plane.feed(FrameKind::AttachReady { attach_id }).unwrap();
}

/// Answer the agent's `ATTACH_RESOURCE` among the queued subscriptions.
fn confirm_agent_subscription(plane: &mut ControlPlane) {
    let agent_request = attach_resources(plane)
        .into_iter()
        .find(|(_, id)| *id == ResourceId::local(AGENT))
        .expect("agent subscription")
        .0;
    plane.reply(agent_request, CommandResult::Ok);
}

/// `OK`, or a `PERMISSION_DENIED` refusal saying `message`.
fn refused_or_ok(refused: bool, message: &str) -> CommandResult {
    if refused {
        CommandResult::Error {
            code: ErrorCode::PermissionDenied,
            message: message.into(),
        }
    } else {
        CommandResult::Ok
    }
}

const fn state(snapshot: SessionSnapshot) -> CommandResult {
    CommandResult::OkWith(CommandValue::State(snapshot))
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
    reopen(plane, hello_ok(PROTOCOL_VERSION.patch));
    let attach_id = outbound_attach_id(plane);
    // A caller can request inventory before ATTACHED. That read must not
    // turn an in-progress session attach into a ready browsing connection.
    let request_id = plane.refresh_topology().unwrap();
    plane.reply(request_id, state(beta_snapshot()));
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
    plane.reply(subscriptions[0].0, CommandResult::Ok);
    ready(&mut plane, attach_id, &terminal());
    let _ = plane.take_outbound();
    assert_eq!(close_count(&mut plane), 0);
    assert!(!plane.attach_session(AttachTarget::ByName("beta".into())));
    let requests = attach_resources(&mut plane);
    for (request_id, _) in requests {
        plane.reply(request_id, CommandResult::Ok);
    }
    assert_eq!(close_count(&mut plane), 0, "live switch retains home pumps");
    plane
}

#[test]
fn reconnect_after_switch_retires_home_subscription_once_and_can_follow_it_again() {
    let mut plane = followed_home();
    let attach_id = reconnect_to_beta(&mut plane);
    assert_eq!(close_count(&mut plane), 0, "wait for ATTACH_READY");
    ready(&mut plane, attach_id, &ResourceId::local(8));
    assert_eq!(close_count(&mut plane), 1);
    assert!(!plane.engine().unwrap().is_closed(&ResourceId::local(AGENT)));

    let request_id = plane.refresh_topology().unwrap();
    // ATTACHED fences its earlier inventory; answer that and then the fresh read.
    plane.reply(request_id, state(with_agent(terminal())));
    assert_eq!(close_count(&mut plane), 0);
    let request_id = plane.attach_terminal(&terminal());
    let _ = plane.take_outbound();
    plane.reply(request_id, CommandResult::Ok);
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
        ready(&mut plane, attach_id, &ResourceId::local(8));
        // Explicit geometry-preserving admission outside the attached session
        // survives reconnect, unlike an ordinary old home/spawn pump.
        let request_id = plane.attach_terminal_preserving_geometry(&terminal());
        plane.reply(request_id, CommandResult::Ok);
        confirm_agent_subscription(&mut plane);
        // Set the reconnect target to the current home session.
        assert!(!plane.attach_session(AttachTarget::ByName("beta".into())));
        let _ = plane.take_outbound();
        let _ = plane.take_events();
        let attach_id = reconnect_to_beta(&mut plane);
        ready(&mut plane, attach_id, &ResourceId::local(8));
        assert_eq!(close_count(&mut plane), 0, "replayed attach still pending");
        let requests = attach_resources(&mut plane);
        let request_id = requests.iter().find(|(_, id)| *id == terminal()).unwrap().0;
        // An inventory may optimistically subscribe the agent while its
        // parent's replay is pending; a refusal must still retire it.
        let inventory_request = plane.refresh_topology().unwrap();
        plane.reply(inventory_request, state(beta_snapshot()));
        let subscriptions = attach_resources(&mut plane);
        assert_eq!(subscriptions.len(), 1);
        assert_eq!(subscriptions[0].1, ResourceId::local(AGENT));
        plane.reply(subscriptions[0].0, CommandResult::Ok);
        assert_eq!(close_count(&mut plane), 0);
        plane.reply(request_id, refused_or_ok(refused, "denied"));
        assert_eq!(close_count(&mut plane), usize::from(refused));
        assert!(attach_resources(&mut plane).is_empty());
    }
}

#[test]
fn refused_parent_detach_preserves_agent_subscription_and_success_releases_once() {
    for refused in [false, true] {
        let (mut plane, attach_id) = subscribing(true);
        attach_to(&mut plane, attach_id, with_agent(ResourceId::local(8)));
        ready(&mut plane, attach_id, &terminal());
        let request_id = plane.attach_terminal(&ResourceId::local(8));
        plane.reply(request_id, CommandResult::Ok);
        confirm_agent_subscription(&mut plane);
        assert_eq!(close_count(&mut plane), 0);

        let detach_request = plane.detach_terminal(&ResourceId::local(8));
        let inventory_request = plane.refresh_topology().unwrap();
        let _ = plane.take_outbound();
        plane.reply(inventory_request, state(with_agent(ResourceId::local(8))));
        assert_eq!(close_count(&mut plane), 0, "detach admission is unsettled");
        plane.reply(
            detach_request,
            refused_or_ok(refused, "parent detach refused"),
        );
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
            .feed(command_err(
                request_id,
                ErrorCode::PermissionDenied,
                "unknown parent",
            ))
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
    ready(&mut plane, attach_id, &terminal());
    let request_id = plane.attach_terminal(&parent);
    plane.reply(request_id, CommandResult::Ok);
    confirm_agent_subscription(&mut plane);
    assert_eq!(close_count(&mut plane), 0);

    let detach_request = plane.detach_terminal(&parent);
    let _ = plane.take_outbound();
    plane
        .feed(phux_wire_fixtures::closed(&parent, Some(0)))
        .unwrap();
    assert!(plane.engine().unwrap().is_closed(&parent));
    plane
        .feed(command_err(
            detach_request,
            ErrorCode::PermissionDenied,
            "detach refused after parent closed",
        ))
        .unwrap();
    assert!(!plane.terminal_is_admitted(&parent));
    assert_eq!(close_count(&mut plane), 1);
    assert!(attach_resources(&mut plane).is_empty());

    // An unversioned inventory can still contain the old AgentSession.
    // Repeated reads must not attach it under the closed parent.
    for _ in 0..3 {
        let request_id = plane.refresh_topology().unwrap();
        plane.reply(request_id, state(with_agent(parent.clone())));
        assert!(attach_resources(&mut plane).is_empty());
        assert_eq!(close_count(&mut plane), 0);
    }
}

#[test]
fn undrained_frames_replay_in_order_after_replacement_without_loss() {
    let mut plane = resumed_with_initial_agent();
    agent_live(&mut plane, 1, &[(3, "tool_start")]).unwrap();
    agent_live(&mut plane, 1, &[(4, "tool_end")]).unwrap();
    // Replacement can open without a preceding connection_lost.
    replace_attached(&mut plane, with_agent(terminal()));
    assert_eq!(
        take_agent_records(&mut plane),
        [(vec![3], false), (vec![4], false)]
    );
    let requests = agent_requests(&mut plane);
    assert_eq!(requests[0].1, Some(4));
    agent_bootstrap(&mut plane, 2, Some(4), &[(5, "stop")]);
    assert_eq!(take_agent_records(&mut plane), [(vec![5], false)]);
}

#[test]
fn close_and_detach_do_not_replay_undrained_publications() {
    for close in [true, false] {
        let mut plane = resumed_with_initial_agent();
        agent_live(&mut plane, 1, &[(3, "stop")]).unwrap();
        assert_eq!(retire_agent(&mut plane, close), [Some(3)], "close={close}");
        replace_attached(&mut plane, with_agent(terminal()));
        assert!(take_agent_records(&mut plane).is_empty());
    }
}

fn replace_attached(plane: &mut ControlPlane, snapshot: SessionSnapshot) {
    reopen(plane, agent_hello(true, 0xAB));
    let attach_id = plane.queued_attach_id();
    attach_ready_to(plane, attach_id, snapshot);
}

fn declare_agent(plane: &mut ControlPlane) {
    plane
        .apply_engine_events(vec![
            phux_client_runtime::engine::EngineEvent::AgentSessionDeclared {
                terminal_id: ResourceId::local(AGENT),
                parent: Some(terminal()),
                provider: Some("claude".into()),
                native_id: None,
                state: Some("working".into()),
            },
        ])
        .unwrap();
}

fn publish_agent(plane: &mut ControlPlane) {
    agent_bootstrap(plane, 1, None, &[(1, "prompt"), (2, "ask")]);
}

#[test]
fn option_false_omitted_attach_withholds_until_explicit_declare() {
    let (mut plane, attach_id) = subscribing(false);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    publish_agent(&mut plane);
    let cursor = agent_info(&plane).unwrap();
    replace_attached(&mut plane, two_session_snapshot(false));
    assert!(
        take_agent_records(&mut plane).is_empty(),
        "a partial ATTACHED must not release an omitted agent"
    );
    assert_eq!(
        agent_info(&plane),
        Some(cursor),
        "a partial ATTACHED must not drop the owner cursor"
    );
    declare_agent(&mut plane);
    assert_eq!(take_agent_records(&mut plane), [(vec![1, 2], true)]);
}

#[test]
fn option_false_authoritative_omit_forgets_the_cursor_and_does_not_resurrect() {
    for drained in [false, true] {
        let (mut plane, attach_id) = subscribing(false);
        attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
        publish_agent(&mut plane);
        if drained {
            assert_eq!(take_agent_records(&mut plane), [(vec![1, 2], true)]);
        }
        replace_attached(&mut plane, two_session_snapshot(false));
        assert!(take_agent_records(&mut plane).is_empty());
        let omitted = two_session_snapshot(false);
        plane.reconcile_agent_delivery_catalog(&omitted);
        let after = plane.take_events();
        assert!(after.iter().all(|event| !matches!(
        event,
        Event::AgentSessionClosed { terminal_id, .. } | Event::TerminalClosed { terminal_id, .. }
            if *terminal_id == ResourceId::local(AGENT)
    )));
        assert!(agent_info(&plane).is_none());
        declare_agent(&mut plane);
        assert!(
            take_agent_records(&mut plane).is_empty(),
            "discarded rows must not resurrect on re-admit"
        );
        agent_bootstrap(&mut plane, 2, None, &[(1, "prompt")]);
        assert_eq!(take_agent_records(&mut plane), [(vec![1], true)]);
    }
}

#[test]
fn authoritative_registry_omission_drops_queued_records_before_delivery() {
    let (mut plane, attach_id) = subscribing(false);
    attach_ready_to(&mut plane, attach_id, with_agent(terminal()));
    publish_agent(&mut plane);
    assert_eq!(agent_info(&plane).unwrap().last_seq, 2);
    plane.reconcile_agent_delivery_catalog(&two_session_snapshot(false));
    assert!(
        take_agent_records(&mut plane).is_empty(),
        "an absent resource must not return through a queued publication"
    );
    assert!(agent_info(&plane).is_none());
}

#[test]
fn authoritative_rebind_publishes_catalog_before_held_agent_records() {
    let mut plane = resumed_with_initial_agent();
    agent_live(&mut plane, 1, &[(3, "tool_start")]).unwrap();
    replace_attached(&mut plane, two_session_snapshot(false));
    assert!(take_agent_records(&mut plane).is_empty());
    let request_id = plane.refresh_topology().unwrap();
    // ATTACHED fences the inventory queued before its admission barrier.
    plane.reply(request_id, state(two_session_snapshot(false)));
    let request_id = plane.refresh_topology().unwrap();
    let _ = plane.take_events();
    plane.reply(request_id, state(with_agent(terminal())));
    let events = plane.take_events();
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
                    if publication.records.iter().map(|record| record.seq).eq([3])
            )
        })
        .expect("held publication");
    assert!(
        catalog < records,
        "a consumer must discover the resource before receiving its records"
    );
}
