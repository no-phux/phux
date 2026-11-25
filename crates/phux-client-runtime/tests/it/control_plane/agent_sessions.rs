//! `ControlOptions::subscribe_agent_sessions`: which `AgentSession` streams
//! the runtime subscribes itself.

use super::*;
use phux_protocol::AgentFacet;

const AGENT: u32 = 31;

mod own_spawn;
mod admission;
mod authority;
mod replay;
mod stream;
use stream::*;

fn subscribing(subscribe_agent_sessions: bool) -> (ControlPlane, u32) {
    subscribing_with_resume(subscribe_agent_sessions, false)
}

fn subscribing_with_resume(subscribe_agent_sessions: bool, resume: bool) -> (ControlPlane, u32) {
    let mut plane = ControlPlane::new(std::time::Instant::now(), ControlOptions {
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

fn replace_attached(plane: &mut ControlPlane, snapshot: SessionSnapshot) {
    reopen(plane, agent_hello(true, 0xAB));
    let attach_id = plane.queued_attach_id();
    attach_ready_to(plane, attach_id, snapshot);
}

fn publish_agent(plane: &mut ControlPlane) {
    agent_bootstrap(plane, 1, None, &[(1, "prompt"), (2, "ask")]);
}

