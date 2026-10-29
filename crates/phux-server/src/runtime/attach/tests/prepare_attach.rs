//! `prepare_attach` refusals before consumer registration.
#![allow(
    clippy::wildcard_imports,
    reason = "the attach test topics share the parent test namespace"
)]

use super::*;

#[test]
fn prepare_attach_reports_snapshot_seed_without_an_actor_as_closed() {
    let state = SharedState::new();
    let (_catalog_session, _catalog_pane) = state.with_mut(|server| server.seed_session("catalog"));
    let (working_window, _seed) = state.with_mut(|server| server.seed_session("working"));
    let (horizontal, vertical) = state.with_mut(|server| {
        let horizontal = server
            .add_pane_to_session(working_window)
            .expect("horizontal pane");
        let vertical = server
            .add_pane_to_session(working_window)
            .expect("vertical pane");
        (horizontal, vertical)
    });
    let mut actors = Vec::new();
    for terminal in <[_; 2]>::from((horizontal, vertical)) {
        let token = CancellationToken::new();
        let bundle = crate::terminal_actor::TerminalActor::build_with_token(
            80,
            24,
            None,
            phux_config::ScrollbackLimits::default(),
            token.clone(),
        )
        .expect("test terminal actor");
        state.with_mut(|server| {
            server.register_resource_handle(terminal, bundle.handle.clone(), token);
        });
        actors.push(bundle.actor);
    }
    let client_id = state.with_mut(crate::state::ServerState::new_client_id);
    let (out_tx, _out_rx) = tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);

    let working = state
        .with(|s| s.find_session_by_name("working"))
        .expect("the seeded session");
    let (snapshot, _initial_client_id, bootstrapped, closed) = prepare_attach(
        &state,
        client_id,
        working,
        &out_tx,
        ClientCapabilities::default(),
        BootstrapProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
    )
    .expect("prepare attach");

    let bootstrapped: std::collections::HashSet<_> = bootstrapped
        .iter()
        .map(|pane| pane.wire_terminal_id.clone())
        .collect();
    let seed = snapshot
        .resources
        .iter()
        .find(|pane| pane.id == snapshot.focused_resource)
        .expect("seed remains in ATTACHED catalog")
        .id
        .clone();
    assert_eq!(snapshot.sessions.len(), 2, "whole session catalog survives");
    assert_eq!(
        snapshot.resources.len(),
        4,
        "ATTACHED keeps every catalog pane"
    );
    assert_eq!(bootstrapped.len(), 2);
    assert!(!bootstrapped.contains(&seed));
    assert_eq!(closed, vec![seed]);
    drop(actors);
}

#[test]
fn prepare_attach_rejects_pane_source_count_before_registration() {
    let state = SharedState::new();
    let (window, _pane) = state.with_mut(|server| server.seed_session("bounded"));
    state.with_mut(|server| {
        for _ in 0..MAX_AGGREGATE_BOOTSTRAP_PANES {
            server
                .add_pane_to_session(window)
                .expect("bounded test pane");
        }
    });
    let client_id = state.with_mut(crate::state::ServerState::new_client_id);
    let (out_tx, _out_rx) = tokio::sync::mpsc::channel(crate::state::DEFAULT_CLIENT_MAILBOX);
    let bounded = state
        .with(|s| s.find_session_by_name("bounded"))
        .expect("the seeded session");
    assert!(matches!(
        prepare_attach(
            &state,
            client_id,
            bounded,
            &out_tx,
            ClientCapabilities::default(),
            BootstrapProfile::SynthesizedVtRaw,
            BootstrapLimits::default(),
        ),
        Err(crate::state::AttachError::ResourceLimit)
    ));
    assert!(!state.with(|server| server.attached().contains_key(&client_id)));
}
