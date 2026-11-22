//! Agent sessions: badges, discovery, admission, and refusals.

use super::*;
use phux_protocol::wire::frame::AgentEvent;

/// A focused session holding one terminal pane and one `AgentSession`
/// resource bound to it, the way a RESOURCE_KINDS server reports them.
fn attached_with_agent(terminal: u32, agent: u32, cols: u16, rows: u16) -> FrameKind {
    let terminal_id = ResourceId::local(terminal);
    attached_snapshot(
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal_id.clone())
            .with_resources(vec![
                ResourceInfo::new(terminal_id.clone(), WindowId::new(1), cols, rows),
                ResourceInfo::new(ResourceId::local(agent), WindowId::new(0), 0, 0)
                    .with_kind(ResourceKind::AgentSession)
                    .with_parent(Some(terminal_id))
                    .with_agent(Some(AgentFacet::new("claude", "idle"))),
            ]),
    )
}

fn agent_record(seq: u64, kind: &str, data: &str) -> Bytes {
    Bytes::from(format!(
        "{{\"seq\":{seq},\"ts_ms\":{},\"type\":\"{kind}\",\"data\":{data}}}\n",
        seq * 10
    ))
}

fn badge_summary(session: &Connection) -> Vec<(String, String)> {
    session
        .agent_badges()
        .into_iter()
        .map(|badge| (badge.provider, badge.state))
        .collect()
}

fn badge(state: &str) -> Vec<(String, String)> {
    vec![("claude".to_owned(), state.to_owned())]
}

#[wasm_bindgen_test]
async fn agent_sessions_become_badges_and_never_panes() {
    let mut session = hello_session(10, 2).await;
    let pane = generation(8, 8, 8);
    let agent = generation(9, 9, 9);

    // The snapshot facet seeds the badge; the agent is not a pane.
    let attached = session.on_frame(attached_with_agent(8, 9, 10, 2));
    assert!(attached.fatal.is_none());
    assert!(attached.badges);
    agent_attach(&attached, 9);
    assert!(session.on_frame(spawned_agent(9, 8)).send.is_empty());
    assert_eq!(badge_summary(&session), badge("idle"));

    // The terminal alone completes the attach; the agent never bootstrapped.
    pane.publish(&mut session, RAW, (10, 2), &b"pane"[..]);
    assert!(session.on_frame(ATTACH_READY).render);
    assert!(session.render_visible());
    let key_frame = session.key_frame(key()).expect("terminal accepts input");
    assert!(
        matches!(decode_one(&key_frame), FrameKind::InputKey { terminal_id, .. } if terminal_id == pane.id),
        "input goes to the terminal, never the agent session"
    );

    // The agent stream publishes its retained backlog, then live records,
    // and each update refreshes the badge.
    let began = session.on_frame(agent.begin(BootstrapStreamProfile::AgentEventsJsonlV1, 0, 0, 1));
    assert!(began.fatal.is_none());
    assert!(!began.render, "an agent stream never paints");
    session.on_frame(agent.chunk(0, agent_record(1, "prompt", r#"{"length":4}"#)));
    let ready = session.on_frame(agent.ready());
    assert!(ready.badges);
    assert!(!ready.render);
    assert_eq!(badge_summary(&session), badge("working"));
    let live = session.on_frame(agent.output(2, agent_record(2, "ask", r#"{"question":"?"}"#)));
    assert!(live.badges);
    assert_eq!(badge_summary(&session), badge("blocked"));

    // A malformed record retires the agent generation but never the terminal.
    let broken = session.on_frame(agent.output(3, &b"{not json\n"[..]));
    assert!(broken.fatal.is_none());
    assert!(!session.is_failed());
    assert!(session.on_frame(pane.output(1, &b"!"[..])).render);
    let row0 = row0(&session);
    assert!(row0.starts_with("pane!"), "row 0 = {row0:?}");

    // Closing the agent retracts its badge and leaves the pane alone.
    let closed = session.on_frame(closed(agent.id, CloseReason::ParentClosed));
    assert!(closed.badges);
    assert!(closed.fatal.is_none());
    assert!(session.agent_badges().is_empty());
    assert!(session.render_visible());
    assert!(session.key_frame(key()).is_some());
}

fn spawned_agent(id: u32, parent: u32) -> FrameKind {
    FrameKind::Event {
        terminal: Some(ResourceId::local(id)),
        event: AgentEvent::ResourceSpawned {
            kind: ResourceKind::AgentSession,
            parent: Some(ResourceId::local(parent)),
        },
        stamp: None,
    }
}

fn closed_agent(id: u32) -> FrameKind {
    FrameKind::Event {
        terminal: Some(ResourceId::local(id)),
        event: AgentEvent::ResourceClosed { exit_status: None },
        stamp: None,
    }
}

fn agent_requests(outcome: &phux_web::Outcome) -> Vec<ResourceId> {
    outcome
        .send
        .iter()
        .filter_map(|bytes| match decode_one(bytes) {
            FrameKind::Command {
                command: Command::AttachResource { terminal_id, .. },
                ..
            } => Some(terminal_id),
            _ => None,
        })
        .collect()
}

#[wasm_bindgen_test]
async fn pre_snapshot_agent_announcements_survive_parent_admission() {
    let mut session = hello_session(20, 3).await;
    for (id, parent) in [(201, 101), (203, 101), (301, 999)] {
        assert!(session.on_frame(spawned_agent(id, parent)).send.is_empty());
    }
    // Both closure forms can overtake the captured snapshot, including a
    // child already in that snapshot whose spawn event preceded subscription.
    session.on_frame(closed_agent(202));
    session.on_frame(closed(ResourceId::local(203), CloseReason::Exited));
    let admitted = session.on_frame(attached_with_agent(101, 202, 20, 3));
    assert!(admitted.fatal.is_none());
    assert_eq!(agent_requests(&admitted), vec![ResourceId::local(201)]);
    assert_eq!(session.agent_badges().len(), 1);
    for (id, parent) in [(201, 101), (202, 101), (203, 101), (301, 999)] {
        assert!(session.on_frame(spawned_agent(id, parent)).send.is_empty());
    }
    assert!(!session.is_failed());
}

#[wasm_bindgen_test]
async fn pending_split_agent_announcements_wait_for_parent_commit() {
    for ack_first in [false, true] {
        let mut session = path_session(false, false).await;
        let spawn = requested_spawn(&mut session, "vertical");
        // Child events may even beat the reply naming the split's new parent.
        session.on_frame(spawned_agent(301, 202));
        session.on_frame(spawned_agent(302, 202));
        session.on_frame(spawned_agent(401, 999));
        session.on_frame(closed_agent(302));
        let attach = announce_split(&mut session, spawn, ResourceId::local(202));
        let ack = reply(attach, CommandResult::Ok);
        let committed = if ack_first {
            assert!(agent_requests(&session.on_frame(ack)).is_empty());
            assert!(session.agent_badges().is_empty());
            bootstrap_split(&mut session, ResourceId::local(202), 2, b"split")
        } else {
            let split = bootstrap_split(&mut session, ResourceId::local(202), 2, b"split");
            assert!(agent_requests(&split).is_empty());
            assert!(session.agent_badges().is_empty());
            session.on_frame(ack)
        };
        assert!(committed.fatal.is_none());
        assert_eq!(agent_requests(&committed), vec![ResourceId::local(301)]);
        assert_eq!(session.agent_badges().len(), 1);
        assert!(session.on_frame(spawned_agent(301, 202)).send.is_empty());
        assert!(session.on_frame(spawned_agent(302, 202)).send.is_empty());
        assert!(session.on_frame(spawned_agent(401, 999)).send.is_empty());
        assert!(session.key_frame(key()).is_some());
    }
}

#[wasm_bindgen_test]
async fn failed_split_discards_unadmitted_agent_announcements() {
    let mut session = path_session(false, false).await;
    let spawn = requested_spawn(&mut session, "vertical");
    session.on_frame(spawned_agent(301, 202));
    session.on_frame(command_err(
        spawn,
        ErrorCode::TerminalNotFound,
        "split refused",
    ));
    let spawn = requested_spawn(&mut session, "vertical");
    let attach = announce_split(&mut session, spawn, ResourceId::local(202));
    bootstrap_split(&mut session, ResourceId::local(202), 2, b"new split");
    let committed = session.on_frame(reply(attach, CommandResult::Ok));
    assert!(agent_requests(&committed).is_empty());
    assert!(session.agent_badges().is_empty());
}

#[wasm_bindgen_test]
async fn closing_split_parent_discards_its_pending_agent_discovery() {
    let mut session = path_session(false, false).await;
    let spawn = requested_spawn(&mut session, "vertical");
    announce_split(&mut session, spawn, ResourceId::local(202));
    session.on_frame(spawned_agent(301, 202));
    let closed = session.on_frame(closed(ResourceId::local(202), CloseReason::Exited));
    assert!(closed.fatal.is_none());
    assert!(agent_requests(&closed).is_empty());
    assert!(!session.pane_pending());
    assert!(session.on_frame(spawned_agent(301, 202)).send.is_empty());
    assert!(session.agent_badges().is_empty());
    assert!(session.key_frame(key()).is_some());
}

#[wasm_bindgen_test]
async fn agent_discovery_admission_is_bounded_and_duplicate_safe() {
    let mut session = hello_session(20, 3).await;
    for _ in 0..300 {
        assert!(session.on_frame(spawned_agent(2000, 999)).fatal.is_none());
    }
    for id in 2001..2256 {
        assert!(session.on_frame(spawned_agent(id, 999)).fatal.is_none());
    }
    let overflow = session.on_frame(spawned_agent(2256, 999));
    assert!(
        overflow
            .fatal
            .as_deref()
            .unwrap()
            .contains("discovery admission buffer exhausted")
    );
    assert!(session.is_failed());
    assert!(session.agent_badges().is_empty());
}

fn agent_attach(outcome: &phux_web::Outcome, expected: u32) -> u32 {
    assert!(outcome.fatal.is_none());
    assert!(outcome.badges);
    assert_eq!(outcome.send.len(), 1);
    match decode_one(&outcome.send[0]) {
        FrameKind::Command {
            request_id,
            command: Command::AttachResource { terminal_id, .. },
        } => {
            assert_eq!(terminal_id, ResourceId::local(expected));
            request_id
        }
        other => panic!("expected child attach, got {other:?}"),
    }
}

#[wasm_bindgen_test]
async fn live_agent_discovery_attaches_once_and_closure_never_resurrects() {
    let mut session = path_session(false, false).await;
    let attached = session.on_frame(spawned_agent(201, 101));
    let request_id = agent_attach(&attached, 201);
    assert_eq!(session.agent_badges().len(), 1);
    assert!(session.on_frame(spawned_agent(201, 101)).send.is_empty());
    assert!(session.on_frame(spawned_agent(202, 999)).send.is_empty());
    assert!(session.on_frame(spawned_agent(203, 201)).send.is_empty());

    let closed = session.on_frame(closed_agent(201));
    assert!(closed.badges);
    assert!(session.agent_badges().is_empty());
    // A queued bootstrap and reply after closure are harmless to the parent.
    let late = generation(201, 201, 201).begin(BootstrapStreamProfile::AgentEventsJsonlV1, 0, 0, 0);
    assert!(session.on_frame(late).fatal.is_none());
    assert!(
        session
            .on_frame(reply(request_id, CommandResult::Ok))
            .fatal
            .is_none()
    );
    assert!(session.on_frame(spawned_agent(201, 101)).send.is_empty());
    assert!(session.agent_badges().is_empty());
    assert!(session.key_frame(key()).is_some());
    assert!(!session.is_failed());
}

#[wasm_bindgen_test]
async fn refused_agent_attach_retracts_badge_without_failing_terminal() {
    let mut session = path_session(false, false).await;
    let request_id = agent_attach(&session.on_frame(spawned_agent(201, 101)), 201);
    let refused = session.on_frame(command_err(
        request_id,
        ErrorCode::TerminalNotFound,
        "child already closed",
    ));
    assert!(refused.fatal.is_none());
    assert!(session.agent_badges().is_empty());
    assert!(session.key_frame(key()).is_some());
    assert!(!session.pane_pending());
}
