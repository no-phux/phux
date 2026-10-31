use std::collections::HashSet;

use super::*;
use crate::resource::ResourceHandle;
use phux_core::ids::ResourceId;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, ColorSupport, LayerSet,
};
use phux_terminal::actor::TerminalHandle;

use phux_protocol::wire::frame::{FrameKind, Scope};

#[test]
fn server_incarnation_is_stable_per_state_and_distinct_between_states() {
    let first = ServerState::new();
    let stable = first.server_incarnation();
    assert_eq!(first.server_incarnation(), stable);
    assert_eq!(format!("{stable:?}"), "ServerIncarnation(<redacted>)");

    let second = ServerState::new();
    assert_ne!(stable, second.server_incarnation());
}
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

fn mk_tx() -> mpsc::Sender<Outbound> {
    let (tx, _rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    tx
}
pub(super) fn mk_handle() -> ResourceHandle {
    let (core, channels) = crate::resource::ResourceCore::new(
        crate::resource::ResourceKind::Terminal,
        None,
        CancellationToken::new(),
        8,
    );
    drop(core);
    ResourceHandle::new(
        None,
        channels.output,
        channels.control,
        TerminalHandle::detached_for_test(80, 24),
    )
}

#[test]
fn new_client_id_is_monotonic_from_one() {
    let mut s = ServerState::new();
    assert_eq!(s.new_client_id(), ClientId(1));
    assert_eq!(s.new_client_id(), ClientId(2));
    assert_eq!(s.new_client_id(), ClientId(3));
}

#[test]
fn one_shot_session_create_results_are_bounded_and_connection_scoped() {
    let mut state = ServerState::new();
    let client_id = state.new_client_id();
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
fn attach_unknown_session_returns_error() {
    let mut s = ServerState::new();
    let cid = s.new_client_id();
    let err = s.attach_default_caps(cid, "ghost", mk_tx()).unwrap_err();
    assert_eq!(err, AttachError::UnknownSession("ghost".to_owned()));
}

#[test]
fn attach_records_client_and_subscribes_to_active_pane() {
    let mut s = ServerState::new();
    let (sid, pid) = s.seed_session("default");
    let cid = s.new_client_id();
    let returned_sid = s.attach_default_caps(cid, "default", mk_tx()).unwrap();
    assert_eq!(returned_sid, sid);
    assert!(s.attached().contains_key(&cid));
    assert_eq!(s.subscribers_for_resource(pid), &[cid]);
}

#[test]
fn attach_subscribes_to_every_pane_not_just_the_active_one() {
    // A multi-pane client is subscribed to all its panes, not just the
    // active one (else input to other panes is dropped).
    let mut s = ServerState::new();
    let (sid, pid1) = s.seed_session("default");
    let pid2 = s
        .add_pane_to_session(sid)
        .expect("add a second pane to the session");
    assert_ne!(pid1, pid2);
    let cid = s.new_client_id();
    s.attach_default_caps(cid, "default", mk_tx()).unwrap();
    assert!(
        s.subscribers_for_resource(pid1).contains(&cid),
        "client not subscribed to first pane"
    );
    assert!(
        s.subscribers_for_resource(pid2).contains(&cid),
        "client not subscribed to second pane (the regression)"
    );
}

#[test]
fn terminal_fanout_reaches_attach_terminal_only_subscribers_exactly_once() {
    // ATTACH_RESOURCE-only consumers resolve to their mailbox too
    // (L1 §3.1, §5.1), each exactly once.
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");

    let attached = s.new_client_id();
    let (attached_tx, _attached_rx) = mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
    s.attach_default_caps(attached, "default", attached_tx)
        .expect("attach the session-scoped client");

    let watcher = s.new_client_id();
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

// ADR-0033 input-lease state machine (the gate's backing store).

#[test]
fn input_open_by_default_blocks_nobody() {
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let a = s.new_client_id();
    assert_eq!(s.input_lease_holder(pid), None);
    assert!(!s.input_blocked(pid, a), "an Open pane blocks no one");
}

#[test]
fn acquired_lease_blocks_others_not_holder() {
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let a = s.new_client_id();
    let b = s.new_client_id();
    assert_eq!(
        s.set_input_lease(pid, a),
        None,
        "first acquire has no prior"
    );
    assert_eq!(s.input_lease_holder(pid), Some(a));
    assert!(!s.input_blocked(pid, a), "the holder is never blocked");
    assert!(s.input_blocked(pid, b), "a non-holder is blocked");
}

#[test]
fn seize_returns_prior_holder() {
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let a = s.new_client_id();
    let b = s.new_client_id();
    s.set_input_lease(pid, a);
    assert_eq!(
        s.set_input_lease(pid, b),
        Some(a),
        "seizing returns the preempted holder"
    );
    assert_eq!(s.input_lease_holder(pid), Some(b));
    assert!(
        s.input_blocked(pid, a),
        "the preempted client is now blocked"
    );
}

#[test]
fn release_is_holder_scoped_and_idempotent() {
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let a = s.new_client_id();
    let b = s.new_client_id();
    s.set_input_lease(pid, a);
    assert!(!s.release_input_lease(pid, b), "non-holder cannot release");
    assert_eq!(s.input_lease_holder(pid), Some(a));
    assert!(
        s.release_input_lease(pid, a),
        "holder releases its own lease"
    );
    assert_eq!(s.input_lease_holder(pid), None);
    assert!(!s.release_input_lease(pid, a), "double release is a no-op");
}

#[test]
fn detach_releases_the_wheel() {
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let a = s.new_client_id();
    s.set_input_lease(pid, a);
    assert_eq!(s.leases_held_by(a), vec![pid]);
    s.detach(a);
    assert_eq!(
        s.input_lease_holder(pid),
        None,
        "a disconnect must never strand the wheel"
    );
    assert!(s.leases_held_by(a).is_empty());
}

#[test]
fn same_client_may_rebootstrap_same_session_but_not_switch_sessions() {
    // Re-ATTACHing the same session is a re-bootstrap; only switching
    // sessions on one connection is refused.
    let mut s = ServerState::new();
    let (default, original) = s.seed_session("default");
    let _ = s.seed_session("other");
    let cid = s.new_client_id();
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
        s.subscribers_for_resource(original),
        &[cid],
        "reattach must not duplicate an existing subscription",
    );
    assert_eq!(
        s.subscribers_for_resource(added),
        &[cid],
        "reattach must subscribe panes created after the first attach",
    );
    let err = s.attach_default_caps(cid, "other", mk_tx()).unwrap_err();
    assert_eq!(err, AttachError::AlreadyAttached(cid));
}

#[test]
fn attach_stores_hello_selected_bootstrap_contract_unchanged() {
    let mut s = ServerState::new();
    let _ = s.seed_session("default");
    let cid = s.new_client_id();
    let profile = BootstrapProfile::SynthesizedVtStateSync;
    let limits = BootstrapLimits::new(64 * 1024, 128 * 1024).unwrap();
    s.attach(
        cid,
        "default",
        mk_tx(),
        ClientCapabilities::default(),
        profile,
        limits,
    )
    .unwrap();
    let attached = &s.attached()[&cid];
    assert_eq!(attached.bootstrap_profile, profile);
    assert_eq!(attached.bootstrap_limits, limits);
}

#[test]
fn two_clients_attach_same_session_see_same_active_pane() {
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    let a = s.new_client_id();
    let b = s.new_client_id();
    s.attach_default_caps(a, "default", mk_tx()).unwrap();
    s.attach_default_caps(b, "default", mk_tx()).unwrap();
    let subs = s.subscribers_for_resource(pid);
    assert!(subs.contains(&a) && subs.contains(&b));
    assert_eq!(subs.len(), 2);
}

#[test]
fn attached_client_color_support_defaults_to_truecolor() {
    // `attach_default_caps` keeps the most permissive tier.
    let mut s = ServerState::new();
    let _ = s.seed_session("default");
    let cid = s.new_client_id();
    s.attach_default_caps(cid, "default", mk_tx()).unwrap();
    let client = s.attached().get(&cid).unwrap();
    assert_eq!(client.client_caps.color_support, ColorSupport::TrueColor);
}

#[test]
fn attach_records_advertised_color_support() {
    // Production path: HELLO advertised a tier, ATTACH consumes it.
    let mut s = ServerState::new();
    let _ = s.seed_session("default");
    let cid = s.new_client_id();
    s.attach(
        cid,
        "default",
        mk_tx(),
        ClientCapabilities::new().with_color_support(ColorSupport::Indexed16),
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    )
    .unwrap();
    let client = s.attached().get(&cid).unwrap();
    assert_eq!(client.client_caps.color_support, ColorSupport::Indexed16);
}

#[test]
fn attach_snapshot_panes_collects_live_handles_for_session_tree() {
    let mut s = ServerState::new();
    let (sid, pid_a) = s.seed_session("default");
    let pid_b = s.add_pane_to_session(sid).expect("second pane");
    let pid_c = s.add_pane_to_session(sid).expect("third pane");

    let _ = s.register_resource_handle(pid_a, mk_handle(), CancellationToken::new());
    let _ = s.register_resource_handle(pid_c, mk_handle(), CancellationToken::new());
    // pid_b intentionally has no handle and must be excluded.

    let panes = s.attach_snapshot_panes(sid);
    let ids: HashSet<ResourceId> = panes.iter().map(|p| p.terminal_id).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&pid_a));
    assert!(ids.contains(&pid_c));
    assert!(!ids.contains(&pid_b));
    for pane in panes {
        assert_eq!(
            s.resource_from_wire(&pane.wire_terminal_id),
            Some(pane.terminal_id),
            "wire id should resolve back to the same pane",
        );
    }
}

#[test]
fn most_recently_touched_session_starts_none_and_tracks_touch_order() {
    let mut s = ServerState::new();
    assert!(
        s.most_recently_touched_session().is_none(),
        "fresh state has no prior activity memory",
    );
    assert!(
        !s.has_session_touch_history(),
        "fresh state has no stale activity memory either",
    );
    let (sid, _pid) = s.seed_session("default");
    s.touch_session(sid);
    assert_eq!(s.most_recently_touched_session(), Some(sid));
    assert!(s.has_session_touch_history());

    // Later touches win, regardless of attach order.
    let (sid2, _p) = s.seed_session("other");
    s.touch_session(sid2);
    assert_eq!(s.most_recently_touched_session(), Some(sid2));
    s.touch_session(sid);
    assert_eq!(s.most_recently_touched_session(), Some(sid));
}

#[test]
fn shared_state_with_and_with_mut_round_trip() {
    let shared = SharedState::new();
    let (_sid, pid) = shared.with_mut(|s| s.seed_session("default"));
    let count = shared.with(|s| s.subscribers_for_resource(pid).len());
    assert_eq!(count, 0);
}

mod geometry;
mod metadata;
mod reap;
