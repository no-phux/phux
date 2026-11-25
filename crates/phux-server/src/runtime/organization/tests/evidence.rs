//! One core->wire projection and current-membership authority on durable reads.
use super::*;
use phux_core::Op;

#[test]
fn missing_exit_evidence_never_becomes_zero_in_inventory() {
    let state = SharedState::new();
    let (session, id) = state.with_mut(|s| s.seed_session("failed"));
    state.with_mut(|s| {
        s.activation_ended(
            s.activation_token(id).unwrap(),
            phux_core::ExitOutcome::UNKNOWN,
            true,
        );
    });
    let key = state.with(|s| ow::resource_key(s.registry().record(id).unwrap().key).unwrap());
    let session =
        state.with(|s| ow::session_key(s.registry().session_key(session).unwrap()).unwrap());
    let all = page(&state, OrgFilter::Session(session));
    let exact = page(&state, OrgFilter::Resource(key));
    assert_eq!(exact.resources, all.resources);
    let activation = exact.resources[0].activation.unwrap();
    assert_eq!(activation.state, ExecState::Failed);
    assert_eq!(
        activation.outcome,
        Some(phux_protocol::wire::frame::Outcome::Unobserved)
    );
    assert!(!activation.launch_interrupted, "a real failure");
    assert!(!exact.resources[0].ended_by_restart());
}

/// A stop that cuts a launch off before any process identity is recorded
/// leaves no proof the OS never ran it: the next start reports it
/// unresolved (`Lost`), and restore never relaunches it. A launch that
/// really failed is not restorable either (above).
#[test]
fn a_launch_a_stop_cut_off_is_restorable_and_a_failed_one_is_not() {
    let only = |state: &SharedState, session| {
        let session =
            state.with(|s| ow::session_key(s.registry().session_key(session).unwrap()).unwrap());
        page(state, OrgFilter::Session(session)).resources[0].clone()
    };
    let stopping = SharedState::new();
    let (session, id) = stopping.with_mut(|s| s.seed_session("cut-off"));
    let token = stopping.with(|s| s.activation_token(id).unwrap());
    // Shutdown starts while the launch is in flight; the effect's own failure
    // proves no process started, and is the stop's doing, not the program's.
    stopping.with_mut(ServerState::begin_shutdown);
    super::effects::fail_launch(&stopping, token);
    let record = only(&stopping, session);
    let activation = record.activation.unwrap();
    assert_eq!(activation.state, ExecState::Failed);
    assert!(activation.launch_interrupted);
    assert!(record.ended_by_restart(), "restore relaunches it");
    stopping.with_mut(|s| {
        let reconciled = s.reconcile_with(None, |_| panic!("must not probe"));
        assert_eq!((reconciled.unresolved, reconciled.lost), (0, 0));
    });

    // The same failure outside a stop is the program's: not restorable.
    let running = SharedState::new();
    let (session, id) = running.with_mut(|s| s.seed_session("failed"));
    let token = running.with(|s| s.activation_token(id).unwrap());
    super::effects::fail_launch(&running, token);
    let record = only(&running, session);
    assert_eq!(record.activation.unwrap().state, ExecState::Failed);
    assert!(!record.ended_by_restart());
}

/// A crash leaves Starting without an identity and without the effect's
/// failure: no proof it never launched, so it stays unresolved.
#[test]
fn a_starting_record_without_evidence_stays_unresolved_and_is_never_restorable() {
    let state = SharedState::new();
    let (session, _) = state.with_mut(|s| s.seed_session("cut-off"));
    state.with_mut(|s| {
        let reconciled = s.reconcile_with(None, |_| panic!("must not probe"));
        assert_eq!((reconciled.unresolved, reconciled.lost), (1, 0));
    });
    let session =
        state.with(|s| ow::session_key(s.registry().session_key(session).unwrap()).unwrap());
    let record = page(&state, OrgFilter::Session(session)).resources[0].clone();
    let activation = record.activation.unwrap();
    assert_eq!(activation.state, ExecState::Lost);
    assert!(!activation.launch_interrupted, "no proof it never launched");
    assert!(!record.ended_by_restart(), "it may still be running");
}

#[test]
fn durable_inspection_follows_current_membership_and_filters_each_read() {
    let state = SharedState::new();
    let (from, id) = state.with_mut(|s| s.seed_session("source"));
    let (to, _) = state.with_mut(|s| s.seed_session("destination"));
    let (key, source, destination) = state.with(|s| {
        (
            ow::resource_key(s.registry().record(id).unwrap().key).unwrap(),
            s.registry().session_key(from).unwrap(),
            s.registry().session_key(to).unwrap(),
        )
    });
    let grant = |selector: String| {
        crate::policy::ConnectionGrant::scoped(
            phux_protocol::scope::TerminalScopeSet::parse_all(&[&selector]).unwrap(),
            Some("sha256:test".into()),
        )
        .unwrap()
    };
    state.with_mut(|s| {
        s.set_connection_grant(
            CLIENT,
            grant(format!(
                "inventory@session:{}",
                ow::session_key(source).unwrap()
            )),
        );
    });
    assert_eq!(page(&state, OrgFilter::Resource(key)).resources.len(), 1);
    state.with_mut(|s| {
        s.commit(
            Op::ResourceMove {
                resource: ow::core_resource_key(&key),
                from: source,
                from_expected: s.org().session(&source).unwrap().revision,
                to: destination,
                to_expected: s.org().session(&destination).unwrap().revision,
            },
            &phux_core::Actor::owner(),
        )
        .unwrap();
    });
    assert!(
        page(&state, OrgFilter::Resource(key)).resources.is_empty(),
        "original session scope must not follow a moved record"
    );
    state.with_mut(|s| s.set_connection_grant(CLIENT, grant(format!("inventory@resource:{key}"))));
    let row = page(&state, OrgFilter::Resource(key))
        .resources
        .pop()
        .unwrap();
    assert_eq!(row.session, ow::session_key(destination).unwrap());
    state.with_mut(|s| {
        s.activation_ended(
            s.activation_token(id).unwrap(),
            phux_core::ExitOutcome::UNKNOWN,
            true,
        );
        let record = s.org().resource(&ow::core_resource_key(&key)).unwrap();
        s.commit(
            Op::ResourceRelaunch {
                resource: record.key,
                expected: record.revision,
                launch: None,
            },
            &phux_core::Actor::owner(),
        )
        .unwrap();
    });
    assert_eq!(
        page(&state, OrgFilter::Resource(key)).resources[0]
            .activation
            .unwrap()
            .id
            .get(),
        2
    );
    state.with_mut(|s| {
        s.activation_ended(
            s.activation_token(id).unwrap(),
            phux_core::ExitOutcome::UNKNOWN,
            true,
        );
        s.commit(
            Op::ResourceRemove {
                resource: ow::core_resource_key(&key),
            },
            &phux_core::Actor::owner(),
        )
        .unwrap();
    });
    assert!(page(&state, OrgFilter::Resource(key)).resources.is_empty());
}
