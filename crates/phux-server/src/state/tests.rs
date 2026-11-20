use super::*;
use crate::resource::ResourceHandle;
use phux_core::ids::ResourceId;
use phux_protocol::caps::LayerSet;

use phux_protocol::wire::frame::{FrameKind, Scope};

/// The incarnation is stable per state, fresh per start, and redacted in
/// Debug so traces do not leak a correlation token.
#[test]
fn the_incarnation_is_per_state_and_redacted() {
    let first = ServerState::new();
    let stable = first.lifecycle().server_incarnation();
    assert_eq!(first.lifecycle().server_incarnation(), stable);
    assert_eq!(format!("{stable:?}"), "ServerIncarnation(<redacted>)");

    let second = ServerState::new();
    assert_ne!(stable, second.lifecycle().server_incarnation());
}
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn mk_tx() -> mpsc::Sender<Outbound> {
    let (tx, _rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    tx
}
pub(super) fn mk_handle() -> ResourceHandle {
    crate::runtime::apps::terminal::test_support::detached_terminal()
}

pub(super) fn mk_agent_handle(parent: ResourceId, token: CancellationToken) -> ResourceHandle {
    phux_agent_session::session::AgentSessionActor::build(
        parent,
        "test-producer",
        None,
        token,
        1024,
    )
    .handle
}

#[test]
fn one_shot_session_create_results_are_bounded_and_connection_scoped() {
    let mut state = ServerState::new();
    let client_id = state.clients_mut().new_client_id();
    let scope = Scope::Global;
    for index in 0..257 {
        let key = format!("phux.session.created/v1/{index}");
        let _ = state.metadata_set(&scope, &key, vec![b'x']);
        state.track_session_create_result(client_id, key);
    }

    assert!(
        state
            .metadata()
            .get(&scope, "phux.session.created/v1/0")
            .is_none(),
        "the oldest unread result must be evicted at the cap",
    );
    assert_eq!(
        state
            .clients
            .session_create_results
            .get(&client_id)
            .map(std::collections::VecDeque::len),
        Some(256),
    );

    state.consume_session_create_result("phux.session.created/v1/1");
    assert!(
        state
            .metadata()
            .get(&scope, "phux.session.created/v1/1")
            .is_none(),
    );
    state.detach(client_id);
    assert!(
        state
            .metadata()
            .get(&scope, "phux.session.created/v1/256")
            .is_none(),
        "disconnect cleanup must remove abandoned one-shot results",
    );
    assert!(
        !state
            .clients
            .session_create_results
            .contains_key(&client_id)
    );
}

#[test]
fn terminal_fanout_reaches_attach_terminal_only_subscribers_exactly_once() {
    // ATTACH_RESOURCE-only consumers resolve to their mailbox too
    // (L1 §3.1, §5.1), each exactly once.
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");

    let attached = s.clients_mut().new_client_id();
    let (attached_tx, _attached_rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    s.attach_default_caps(attached, "default", attached_tx)
        .expect("attach the session-scoped client");

    let watcher = s.clients_mut().new_client_id();
    let (watcher_tx, _watcher_rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    s.subscribe_resource(watcher, pid, Some(watcher_tx));

    assert_eq!(
        s.terminal_fanout_targets(pid).len(),
        2,
        "both the session-attached client and the ATTACH_RESOURCE-only \
         watcher must resolve to a mailbox",
    );

    // A session-attached client re-attaching a pane per-Terminal still
    // resolves to one mailbox.
    let (upgrade_tx, _upgrade_rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    s.subscribe_resource(attached, pid, Some(upgrade_tx));
    assert_eq!(
        s.terminal_fanout_targets(pid).len(),
        2,
        "a client subscribed both ways must be delivered exactly one frame",
    );

    // Disconnect drops the per-Terminal mailbox with the subscription.
    s.detach(watcher);
    assert_eq!(
        s.terminal_fanout_targets(pid).len(),
        1,
        "a detached watcher must leave neither a subscription nor a mailbox",
    );
}

#[test]
fn input_lease_blocks_non_holders_until_released_or_detached() {
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let a = s.clients_mut().new_client_id();
    let b = s.clients_mut().new_client_id();
    assert_eq!(s.leases().holder(pid), None);
    assert!(!s.leases().blocked(pid, a), "an Open pane blocks no one");

    assert_eq!(s.set_input_lease(pid, a), None);
    assert!(!s.leases().blocked(pid, a) && s.leases().blocked(pid, b));
    assert_eq!(
        s.set_input_lease(pid, b),
        Some(a),
        "a seize returns the prior holder"
    );
    assert!(s.leases().blocked(pid, a));

    assert!(!s.release_input_lease(pid, a), "only the holder releases");
    assert!(s.release_input_lease(pid, b));
    assert!(!s.release_input_lease(pid, b), "double release is a no-op");
    assert_eq!(s.leases().holder(pid), None);

    s.set_input_lease(pid, a);
    assert_eq!(s.leases().held_by(a), vec![pid]);
    s.detach(a);
    assert_eq!(
        s.leases().holder(pid),
        None,
        "a disconnect never strands the wheel"
    );
    assert!(s.leases().held_by(a).is_empty());
}

#[test]
fn same_client_may_rebootstrap_same_session_but_not_switch_sessions() {
    // Re-ATTACHing the same session is a re-bootstrap; only switching
    // sessions on one connection is refused.
    let mut s = ServerState::new();
    let (default, original) = s.seed_session("default");
    let _ = s.seed_session("other");
    let cid = s.clients_mut().new_client_id();
    s.attach_default_caps(cid, "default", mk_tx()).unwrap();
    let added = s
        .add_pane_to_session(default)
        .expect("pane added after initial attach");
    assert_eq!(
        s.attach_default_caps(cid, "default", mk_tx()).unwrap(),
        default,
        "same-connection recovery reuses the live session identity"
    );
    assert_eq!(
        s.resources().subscribers_for(original),
        &[cid],
        "reattach must not duplicate an existing subscription",
    );
    assert_eq!(
        s.resources().subscribers_for(added),
        &[cid],
        "reattach must subscribe panes created after the first attach",
    );
    let err = s.attach_default_caps(cid, "other", mk_tx()).unwrap_err();
    assert_eq!(err, AttachError::AlreadyAttached(cid));
}

mod geometry;
mod metadata;
mod reap;
