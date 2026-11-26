//! Handoff cannot silently discard a lifecycle report refused by persistence.
use super::super::*;

#[test]
fn pending_executor_evidence_blocks_the_handoff_cut_until_persisted() {
    let state = SharedState::new();
    let (_, pane) = state.with_mut(|s| s.seed_session("pending"));
    let store = crate::org_store::MemOrgStore::default();
    let failing = store.fail_commits();
    failing.store(true, std::sync::atomic::Ordering::Relaxed);
    state.with_mut(|s| {
        s.set_org_store(Box::new(store));
        s.activation_ended(
            s.activation_token(pane).unwrap(),
            phux_core::ExitOutcome::UNKNOWN,
            true,
        );
    });
    let tree = state.with(|s| s.assemble_upgrade_blob(3));
    assert!(matches!(
        reassemble_unchanged_tree(&state, 3, 3, &tree, &[]),
        Err(UpgradeError::TreeChanged)
    ));
    assert!(state.with(crate::state::ServerState::has_pending_activation_reports));
    failing.store(false, std::sync::atomic::Ordering::Relaxed);
    state.with_mut(crate::state::ServerState::retry_activation_reports);
    let tree = state.with(|s| s.assemble_upgrade_blob(3));
    assert!(reassemble_unchanged_tree(&state, 3, 3, &tree, &[]).is_ok());
    assert!(!state.with(crate::state::ServerState::has_pending_activation_reports));
}

#[test]
fn pending_retirement_blocks_handoff_even_after_the_end_is_durable() {
    let state = SharedState::new();
    let (pane, token) = state.with_mut(|s| {
        let (_, pane) = s
            .create_terminal(
                crate::state::TerminalTarget::NewSession {
                    name: "pending-removal",
                    lifetime: phux_core::SessionLifetime::Persistent,
                },
                phux_core::TerminalLaunch::default(),
                phux_core::ExitPolicy::Retain,
                &phux_core::Actor::owner(),
            )
            .unwrap();
        (pane, s.activation_token(pane).unwrap())
    });
    state.with_mut(|s| s.activation_ended(token, phux_core::ExitOutcome::exited(0), false));
    let store = crate::org_store::MemOrgStore::default();
    let failing = store.fail_commits();
    failing.store(true, std::sync::atomic::Ordering::Relaxed);
    state.with_mut(|s| {
        s.set_org_store(Box::new(store));
        s.reap_resource(pane);
        assert!(s.activation_persistence_pending(token.resource));
        assert!(s.resources().handle(pane).is_none());
    });
    let tree = state.with(|s| s.assemble_upgrade_blob(3));
    assert!(matches!(
        reassemble_unchanged_tree(&state, 3, 3, &tree, &[]),
        Err(UpgradeError::TreeChanged)
    ));
    failing.store(false, std::sync::atomic::Ordering::Relaxed);
    state.with_mut(crate::state::ServerState::retry_activation_reports);
    assert!(state.with(|s| s.org().resource(&token.resource).is_none()));
    let tree = state.with(|s| s.assemble_upgrade_blob(3));
    assert!(reassemble_unchanged_tree(&state, 3, 3, &tree, &[]).is_ok());
}
