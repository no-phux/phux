//! A retained floating overlay (ADR-0124, ADR-0147) on its process's exit.

use phux_protocol::wire::frame::{ControlAction, ResourceLifecycle};

use super::*;

fn exited(status: i32) -> FrameKind {
    event(
        &tid(2),
        AgentEvent::TerminalControl {
            lifecycle: ResourceLifecycle::Exited,
            exit_status: Some(status),
            input_holder: None,
            action: ControlAction::Exited,
            actor: None,
        },
    )
}

/// Success closes the overlay and kills its Terminal, silently.
#[test]
fn a_retained_floating_overlay_is_killed_when_its_process_succeeds() {
    let mut h = floating_parked();
    h.send(spawned(tid(2)));
    let outcome = h.send(exited(0));
    assert_eq!(outcome.kill_orphans, vec![tid(2)]);
    assert!(outcome.layout_replaced);
    assert!(h.mirror.expected_closes.contains(&tid(2)));
    assert!(!h.mirror.panes.contains_key(&tid(2)));
}

/// A failure stays up, read-only, titled with the exit, until a key.
#[test]
fn a_failed_floating_overlay_holds_its_screen() {
    let mut h = floating_parked();
    h.send(spawned(tid(2)));
    let outcome = h.send(exited(1));
    assert!(outcome.kill_orphans.is_empty() && outcome.layout_replaced);
    let slot = &h.mirror.panes[&tid(2)];
    assert_eq!(
        slot.floating.as_deref(),
        Some("Agent Board · exited 1 · any key")
    );
    assert!(slot.exited.is_some());
}
