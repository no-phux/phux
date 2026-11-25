//! An unseen mark (`phux.unseen/v1`) across an upgrade.

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{AgentEvent, PaneUnseen, RESOURCE_UNSEEN_KEY, Scope};

use crate::state::SharedState;

fn held(state: &SharedState, pane: &WireResourceId) -> Option<PaneUnseen> {
    let scope = Scope::Resource(pane.clone());
    let value = state.with(|s| s.metadata().get(&scope, RESOURCE_UNSEEN_KEY));
    value.as_deref().and_then(PaneUnseen::decode)
}

#[test]
fn a_mark_crosses_an_upgrade_with_its_pane() {
    let state = SharedState::new();
    let (_session, pane) = state.with_mut(|s| s.seed_session("work"));
    let pane = state.with_mut(|s| s.idspace.intern_resource(pane));
    state.with_mut(|s| s.mark_unseen_event(&pane, &AgentEvent::Bell));
    let blobs: Vec<_> = state.with(|s| s.unseen_blobs().collect());
    let [carried] = blobs.as_slice() else {
        panic!("one mark: {blobs:?}");
    };
    assert_eq!(carried.pane, pane.local_id());
    // A pane that did not cross leaves no stray mark.
    let blob = super::validation::one_pane_blob();
    let next = SharedState::new();
    next.with_mut(|s| s.restore_unseen(&blob, 4242, &carried.value));
    assert_eq!(held(&next, &WireResourceId::local(4242)), None);
    next.with_mut(|s| s.restore_unseen(&blob, blob.panes[0].wire_id, &carried.value));
    let crossed = WireResourceId::local(blob.panes[0].wire_id);
    assert_eq!(held(&next, &crossed), Some(PaneUnseen::Bell));
}
