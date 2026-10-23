//! One core->wire projection and current-membership authority on durable reads.
use super::*;

#[test]
fn missing_exit_evidence_never_becomes_zero_in_inventory() {
    let state = SharedState::new();
    let (session, id) = state.with_mut(|s| s.seed_session("failed"));
    state.with_mut(|s| s.activation_ended(id, phux_core::ExitOutcome::UNKNOWN, true));
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
    for code in [0, 7, 127] {
        assert_eq!(
            ow::outcome(phux_core::Outcome::Exit(phux_core::ExitOutcome::exited(
                code
            ))),
            phux_protocol::wire::frame::Outcome::Exited(code)
        );
    }
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
        s.activation_ended(id, phux_core::ExitOutcome::UNKNOWN, true);
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
        s.activation_ended(id, phux_core::ExitOutcome::UNKNOWN, true);
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
