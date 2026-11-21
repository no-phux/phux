//! A snapshot is a view, not a death certificate. `ATTACHED` keeps what the
//! grant observes and never lists a hub's satellites; `GET_STATE` keeps what
//! the grant inventories and does. Only the same
//! view's previous listing proves a close by absence.

use super::*;
use phux_protocol::wire::info::HostInventory;

const fn sibling() -> ResourceId {
    ResourceId::local(8)
}

fn satellite() -> ResourceId {
    ResourceId::satellite("sat", 3)
}

fn with_terminals(ids: &[ResourceId]) -> SessionSnapshot {
    snapshot().with_resources(
        ids.iter()
            .map(|id| ResourceInfo::new(id.clone(), WindowId::new(1), 20, 4))
            .collect(),
    )
}

fn closed(events: &[Event], id: &ResourceId) -> bool {
    events.iter().any(
        |event| matches!(event, Event::TerminalClosed { terminal_id, .. } if terminal_id == id),
    )
}

fn listed(plane: &ControlPlane, id: &ResourceId) -> bool {
    plane.topology().unwrap().pane(id).is_some()
}

fn attached(plane: &mut ControlPlane, attach_id: u32, snapshot: SessionSnapshot) {
    plane.attached(attach_id, snapshot);
}

fn reattach(plane: &mut ControlPlane, snapshot: SessionSnapshot) -> Vec<FrameKind> {
    plane.connection_lost(None);
    reopen(plane, hello_ok(PROTOCOL_VERSION.patch));
    let attach_id = plane.queued_attach_id();
    attached(plane, attach_id, snapshot);
    outbound(plane)
}

/// One `GET_STATE` round trip; the events it caused.
fn inventory(plane: &mut ControlPlane, snapshot: SessionSnapshot) -> Vec<Event> {
    let request_id = plane.refresh_topology().expect("negotiated topology read");
    let _ = plane.take_outbound();
    answer_state(plane, request_id, snapshot);
    plane.take_events()
}

/// A restarted server's first listing omits what the old one listed, then a
/// relaunched pane takes the recycled id. That pane must show.
#[test]
fn a_new_incarnation_can_list_an_id_its_predecessor_listed() {
    let mut plane = opened(ControlOptions::default(), hello_ok(PROTOCOL_VERSION.patch));
    inventory(&mut plane, with_terminals(&[terminal()]));

    plane.connection_lost(None);
    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_id, .. } = &mut hello {
        server_id.fill(0xCD);
    }
    plane.feed(hello).unwrap();
    inventory(&mut plane, with_terminals(&[]));
    inventory(&mut plane, with_terminals(&[terminal()]));

    let topology = plane.topology().unwrap();
    assert!(topology.panes.iter().any(|p| p.terminal_id == terminal()));
}

#[test]
fn an_attach_that_omits_satellites_neither_closes_them_nor_hides_them_for_long() {
    let (mut plane, attach_id) = negotiated();
    attached(&mut plane, attach_id, with_terminals(&[terminal()]));
    refresh_to(&mut plane, with_terminals(&[terminal(), satellite()]));
    let _ = plane.take_events();

    let frames = reattach(&mut plane, with_terminals(&[terminal()]));
    assert!(!closed(&plane.take_events(), &satellite()));
    let read = state_read(&frames).expect("the attach hid inventoried panes, so it re-reads");
    answer_state(&mut plane, read, with_terminals(&[terminal(), satellite()]));
    assert!(listed(&plane, &satellite()));
}

#[test]
fn the_same_view_still_proves_a_close_missed_while_disconnected() {
    let (mut plane, attach_id) = negotiated();
    attached(
        &mut plane,
        attach_id,
        with_terminals(&[terminal(), sibling()]),
    );
    let _ = plane.take_events();

    let frames = reattach(&mut plane, with_terminals(&[terminal()]));
    assert!(closed(&plane.take_events(), &sibling()));
    assert!(
        state_read(&frames).is_none(),
        "nothing is hidden, so nothing re-reads"
    );

    refresh_to(&mut plane, with_terminals(&[terminal(), sibling()]));
    assert!(
        !listed(&plane, &sibling()),
        "a proven close is never resurrected"
    );
}

#[test]
fn an_unreachable_satellite_is_not_evidence_its_terminals_closed() {
    let (mut plane, attach_id) = negotiated();
    attached(&mut plane, attach_id, with_terminals(&[terminal()]));
    let listing = |ids: &[ResourceId]| {
        with_terminals(ids).with_hosts(vec![HostInventory::reachable("sat".into(), Vec::new())])
    };
    refresh_to(&mut plane, listing(&[terminal(), satellite()]));

    let down = HostInventory::unreachable("sat".into(), "link down");
    let events = inventory(
        &mut plane,
        with_terminals(&[terminal()]).with_hosts(vec![down]),
    );
    assert!(!closed(&events, &satellite()));
    refresh_to(&mut plane, listing(&[terminal(), satellite()]));
    assert!(
        listed(&plane, &satellite()),
        "the link came back; so did it"
    );

    let events = inventory(&mut plane, listing(&[terminal()]));
    assert!(
        closed(&events, &satellite()),
        "a reachable satellite that no longer lists the terminal proves its close"
    );
    refresh_to(&mut plane, listing(&[terminal(), satellite()]));
    assert!(
        !listed(&plane, &satellite()),
        "a proven close is never resurrected"
    );
}
