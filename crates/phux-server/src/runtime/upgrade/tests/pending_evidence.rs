//! Handoff cannot silently discard a lifecycle report refused by persistence.
#![allow(clippy::unwrap_used)]
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
        s.activation_ended(pane, phux_core::ExitOutcome::UNKNOWN, true);
    });
    let handoffs = UpgradeHandoffs::default();
    let tree = state.with(|s| s.assemble_upgrade_blob(3, &handoffs));
    assert!(matches!(
        reassemble_unchanged_tree(&state, 3, 3, &tree, &handoffs),
        Err(UpgradeError::TreeChanged)
    ));
    assert!(state.with(crate::state::ServerState::has_pending_activation_reports));
    failing.store(false, std::sync::atomic::Ordering::Relaxed);
    state.with_mut(crate::state::ServerState::retry_activation_reports);
    let tree = state.with(|s| s.assemble_upgrade_blob(3, &handoffs));
    assert!(reassemble_unchanged_tree(&state, 3, 3, &tree, &handoffs).is_ok());
    assert!(!state.with(crate::state::ServerState::has_pending_activation_reports));
}
