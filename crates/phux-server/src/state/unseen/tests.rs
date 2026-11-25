use super::*;
use crate::state::SharedState;

fn attach(state: &SharedState) -> ClientId {
    let client = state.with_mut(|s| s.clients_mut().new_client_id());
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    state
        .with_mut(|s| s.attach_default_caps(client, "work", tx))
        .expect("attach");
    client
}

fn held(state: &SharedState, pane: &WireResourceId) -> Option<PaneUnseen> {
    let scope = Scope::Resource(pane.clone());
    let value = state.with(|s| s.metadata().get(&scope, RESOURCE_UNSEEN_KEY));
    value.as_deref().and_then(PaneUnseen::decode)
}

fn declare(state: &SharedState, pane: &WireResourceId, agent: &str) {
    let record = format!(r#"{{"name":"build","state":"{agent}"}}"#);
    let scope = Scope::Resource(pane.clone());
    let key = phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
    state.with_mut(|s| s.metadata_set(&scope, key, record.into_bytes()));
}

#[test]
fn a_mark_grows_until_any_view_looks_and_holds_off_while_one_does() {
    let state = SharedState::new();
    let (_session, pane) = state.with_mut(|s| s.seed_session("work"));
    let pane = state.with_mut(|s| s.idspace.intern_resource(pane));
    let event = |event| state.with_mut(|s| s.mark_unseen_event(&pane, &event));
    let (desk, phone) = (attach(&state), attach(&state));

    event(AgentEvent::Bell);
    event(AgentEvent::Dirty);
    assert_eq!(held(&state, &pane), Some(PaneUnseen::Bell), "no downgrade");
    declare(&state, &pane, "done");
    assert_eq!(
        held(&state, &pane),
        Some(PaneUnseen::Bell),
        "a first sight of done is no finish"
    );
    declare(&state, &pane, "working");
    declare(&state, &pane, "done");
    assert_eq!(held(&state, &pane), Some(PaneUnseen::Finished));

    state.with_mut(|s| s.set_in_view(desk, &pane, true));
    assert_eq!(held(&state, &pane), Some(PaneUnseen::Seen));
    event(AgentEvent::Bell);
    assert_eq!(held(&state, &pane), Some(PaneUnseen::Seen), "in view");

    // Another view leaving changes nothing; the holder's end does.
    state.with_mut(|s| s.set_in_view(phone, &pane, false));
    event(AgentEvent::Bell);
    assert_eq!(held(&state, &pane), Some(PaneUnseen::Seen));
    state.with_mut(|s| s.detach(desk));
    event(AgentEvent::Dirty);
    assert_eq!(held(&state, &pane), Some(PaneUnseen::Output));

    // An unattached client (the CLI) clears it once and holds nothing.
    let _ = state.with_mut(|s| s.note_seen(ClientId(99), &pane));
    assert_eq!(held(&state, &pane), Some(PaneUnseen::Seen));
    event(AgentEvent::Bell);
    assert_eq!(held(&state, &pane), Some(PaneUnseen::Bell));
}

#[test]
fn a_client_may_not_write_a_mark_or_a_malformed_report() {
    let pane = Scope::Resource(WireResourceId::local(1));
    assert!(refusal(&pane, RESOURCE_UNSEEN_KEY, b"seen").is_some());
    assert!(refusal(&pane, RESOURCE_IN_VIEW_KEY, b"yes").is_some());
    assert!(refusal(&Scope::Global, RESOURCE_IN_VIEW_KEY, b"true").is_some());
    assert_eq!(refusal(&pane, RESOURCE_IN_VIEW_KEY, b"true"), None);
}
