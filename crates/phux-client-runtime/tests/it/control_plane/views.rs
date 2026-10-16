//! A snapshot is a view, not a death certificate. `ATTACHED` keeps what the
//! grant observes and never lists a hub's satellites; `GET_STATE` keeps what
//! the grant inventories and does (workload-auth §6, L1 §9.1). Only the same
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
    plane
        .feed(FrameKind::Attached {
            attach_id,
            snapshot,
            initial_client_id: ClientId::new(1),
        })
        .expect("ATTACHED");
}

fn reattach(plane: &mut ControlPlane, snapshot: SessionSnapshot) -> Vec<FrameKind> {
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let attach_id = plane
        .take_outbound()
        .iter()
        .find_map(|bytes| match decode(bytes) {
            FrameKind::Attach { attach_id, .. } => Some(attach_id),
            _ => None,
        })
        .expect("reconnect attaches");
    attached(plane, attach_id, snapshot);
    plane
        .take_outbound()
        .iter()
        .map(|bytes| decode(bytes))
        .collect()
}

fn state_read(frames: &[FrameKind]) -> Option<u32> {
    frames.iter().find_map(|frame| match frame {
        FrameKind::Command {
            request_id,
            command: Command::GetState { .. },
        } => Some(*request_id),
        _ => None,
    })
}

fn answer(plane: &mut ControlPlane, request_id: u32, snapshot: SessionSnapshot) {
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        })
        .expect("GET_STATE reply");
}

/// One `GET_STATE` round trip; the events it caused.
fn inventory(plane: &mut ControlPlane, snapshot: SessionSnapshot) -> Vec<Event> {
    let request_id = plane.refresh_topology().expect("negotiated topology read");
    let _ = plane.take_outbound();
    answer(plane, request_id, snapshot);
    plane.take_events()
}

#[test]
fn a_narrower_inventory_hides_an_observed_pane_without_closing_it() {
    let (mut plane, attach_id) = negotiated();
    attached(
        &mut plane,
        attach_id,
        with_terminals(&[terminal(), sibling()]),
    );
    let _ = plane.take_events();

    let events = inventory(&mut plane, with_terminals(&[terminal()]));
    assert!(
        !closed(&events, &sibling()),
        "an INVENTORY view that omits an OBSERVE-listed pane is not its close"
    );
    assert!(!listed(&plane, &sibling()));

    refresh_to(&mut plane, with_terminals(&[terminal(), sibling()]));
    assert!(listed(&plane, &sibling()), "a hidden pane may reappear");
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
    answer(&mut plane, read, with_terminals(&[terminal(), satellite()]));
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
