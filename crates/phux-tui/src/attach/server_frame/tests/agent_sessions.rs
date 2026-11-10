//! Resource kinds: `AgentSession` children ride the attach as record
//! streams, never as pane slots.

use super::resource_kinds::mixed_kind_snapshot;
use super::*;

/// A client attached to [`mixed_kind_snapshot`] of panes 1 and 2.
fn kind_attached() -> (H, FrameOutcome) {
    let mut h = H::new().with_viewport((100, 30));
    let outcome = h.send(attached(mixed_kind_snapshot(&tid(1), &tid(2)), 1));
    (h, outcome)
}

#[test]
fn attach_participants_and_agent_sessions_split_a_mixed_kind_snapshot() {
    let (pane, agent) = (tid(1), tid(2));
    let snapshot = mixed_kind_snapshot(&pane, &agent);
    let participants = attach_participants(&snapshot);
    assert_eq!(
        participants,
        vec![pane],
        "only the Terminal is a barrier participant"
    );
    let children: Vec<&ResourceId> = attach_agent_sessions(&snapshot, &participants)
        .into_iter()
        .map(|info| &info.id)
        .collect();
    assert_eq!(children, vec![&agent]);
    assert!(
        attach_agent_sessions(&snapshot, &[]).is_empty(),
        "a child whose parent is not attached is not declared"
    );
}

#[test]
fn attached_seeds_a_slot_only_for_the_terminal_and_declares_the_agent_session() {
    let (pane, agent) = (tid(1), tid(2));
    let (h, outcome) = kind_attached();
    assert_eq!(
        h.mirror.panes.keys().collect::<Vec<_>>(),
        vec![&pane],
        "no slot for a 0x0 stream"
    );
    assert_eq!(
        h.mirror.engine_kernel.resource_kind(&agent),
        Some(ResourceKind::AgentSession)
    );
    let view = h
        .mirror
        .engine_kernel
        .agent_session(&agent)
        .expect("declared");
    assert_eq!(view.parent, Some(&pane));
    assert_eq!(view.state.provider.as_deref(), Some("claude"));
    assert_eq!(
        view.state.status,
        phux_client_core::session::agent_stream::AgentSessionStatus::Working
    );
    assert!(outcome.pane_cwds.iter().all(|(id, _)| id != &agent));
    assert_eq!(h.leaves(), vec![pane], "the layout holds the pane alone");
}

#[test]
fn an_agent_stream_bootstraps_without_a_slot_and_dirties_the_chrome() {
    let (pane, agent) = (tid(1), tid(2));
    let (mut h, _) = kind_attached();
    let begin = h
        .try_send(FrameKind::BootstrapBegin {
            terminal_id: agent.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            profile: phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1,
            cols: 0,
            rows: 0,
            // L1 §4.8: base_seq is the record counter at the cut.
            base_seq: 1,
        })
        .expect("agent BEGIN at 0x0 is legal");
    assert!(!begin.chrome_dirty);
    h.send(chunk_frame(
        &agent,
        b"{\"seq\":1,\"ts_ms\":1,\"type\":\"ask\",\"data\":{}}\n",
    ));
    let ready = h.send(ready_frame(&agent));
    assert!(ready.chrome_dirty, "the published log changes the sidebar");
    assert!(
        !ready.layout_replaced,
        "no pane repaint for a record stream"
    );
    let live = h.send(output_frame(
        &agent,
        2,
        b"{\"seq\":2,\"ts_ms\":2,\"type\":\"stop\",\"data\":{}}\n",
    ));
    assert!(live.chrome_dirty);
    assert!(
        !h.mirror.panes.contains_key(&agent),
        "a stream never seeds a slot"
    );

    let rows = crate::attach::agent_rows::agent_session_rows(&h.mirror.engine_kernel);
    let under_pane = &rows[&pane];
    assert_eq!(under_pane.len(), 1);
    assert_eq!(under_pane[0].id, agent);
    assert_eq!(under_pane[0].provider.as_deref(), Some("claude"));
    assert_eq!(
        under_pane[0].state,
        phux_client::agent_meta::AgentMetaState::Done
    );
}

#[test]
fn closing_an_agent_session_removes_only_its_row() {
    let (pane, agent) = (tid(1), tid(2));
    let (mut h, _) = kind_attached();
    let before = h.mirror.workspace.clone();
    let outcome = h.send(FrameKind::ResourceClosed {
        terminal_id: agent.clone(),
        exit_status: None,
        reason: CloseReason::ParentClosed,
        signal: None,
    });
    assert!(!outcome.exit && outcome.chrome_dirty);
    assert!(!outcome.layout_replaced && !outcome.emit_set_metadata && !outcome.reflow_panes);
    assert!(
        outcome.notices.is_empty(),
        "no pane-exit notice for a stream"
    );
    assert_eq!(h.mirror.workspace, before);
    assert!(h.mirror.panes.contains_key(&pane));
    assert!(h.mirror.engine_kernel.agent_session(&agent).is_none());
    assert!(crate::attach::agent_rows::agent_session_rows(&h.mirror.engine_kernel).is_empty());
}

/// Rune places native agent panes: an `AgentSession` leaf is adopted as a
/// placeholder tile, not refused, and never attached as a Terminal.
#[test]
fn a_layout_naming_an_agent_session_tiles_it_as_a_placeholder() {
    let (mut h, _) = kind_attached();
    let mut mixed = ArrangementDoc::single(tid(1));
    mixed.groups[0].state = split2(1, 2, 1);
    let outcome = h.send(layout_changed(1, Some(mixed.encode_cbor().unwrap())));
    assert!(outcome.layout_replaced && outcome.attach_panes.is_empty());
    assert_eq!(h.leaves(), vec![tid(1), tid(2)]);
    assert!(!h.mirror.panes.contains_key(&tid(2)));
}

#[test]
fn a_live_spawned_agent_session_is_declared_and_attached_as_a_stream() {
    let (pane, late) = (tid(1), tid(3));
    let (mut h, _) = kind_attached();
    let spawn = |parent: ResourceId| AgentEvent::ResourceSpawned {
        kind: ResourceKind::AgentSession,
        parent: Some(parent),
    };
    let outcome = h.send(event(&late, spawn(pane.clone())));
    assert_eq!(outcome.attach_panes, vec![late.clone()]);
    assert!(outcome.chrome_dirty && !outcome.foreign_pane_set_dirty);
    assert!(!h.mirror.panes.contains_key(&late));
    assert_eq!(
        h.mirror
            .engine_kernel
            .agent_session(&late)
            .expect("declared")
            .parent,
        Some(&pane)
    );

    // A child of a pane this client does not hold is a peer's business.
    let outcome = h.send(event(&tid(4), spawn(tid(99))));
    assert!(outcome.attach_panes.is_empty() && outcome.foreign_pane_set_dirty);
    assert!(h.mirror.engine_kernel.agent_session(&tid(4)).is_none());
}
