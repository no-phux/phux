use phux_protocol::ids::{ResourceId, ResourceKind, SatelliteHost};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};
use phux_protocol::{SessionId, WindowId};

use super::satellite_host_inventory;

/// A satellite snapshot with two sessions: `build` (two windows, three
/// terminals, one agent-session child) and `logs` (one window whose
/// remembered focus is unset).
fn satellite_snapshot() -> SessionSnapshot {
    let build = SessionId::new(1);
    let logs = SessionId::new(2);
    SessionSnapshot::new(build, WindowId::new(10), ResourceId::local(100))
        .with_sessions(vec![
            SessionInfo::new(build, "build")
                .with_window_count(2)
                .with_attached_client_count(1)
                .with_active_window(Some(WindowId::new(11))),
            SessionInfo::new(logs, "logs").with_window_count(1),
        ])
        .with_windows(vec![
            WindowInfo::new(WindowId::new(10), build, "a")
                .with_active_resource(Some(ResourceId::local(100))),
            WindowInfo::new(WindowId::new(11), build, "b")
                .with_active_resource(Some(ResourceId::local(102))),
            WindowInfo::new(WindowId::new(20), logs, "tail"),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(100), WindowId::new(10), 80, 24),
            ResourceInfo::new(ResourceId::local(101), WindowId::new(10), 80, 24),
            ResourceInfo::new(ResourceId::local(102), WindowId::new(11), 80, 24),
            ResourceInfo::resource(ResourceId::local(103), ResourceKind::AgentSession)
                .with_parent(Some(ResourceId::local(100))),
            ResourceInfo::new(ResourceId::local(200), WindowId::new(20), 80, 24),
        ])
}

/// Sessions keep their satellite-local ids and names; counts come from
/// the satellite's own windows; the active pane is re-tagged for the hub.
#[test]
fn inventory_lists_satellite_sessions_without_renumbering() {
    let host = SatelliteHost::from("edge");
    let row = satellite_host_inventory(&host, &satellite_snapshot());

    assert!(row.is_reachable());
    assert_eq!(row.host, host);
    let build = &row.sessions[0];
    assert_eq!(build.id, SessionId::new(1), "satellite-local id, verbatim");
    assert_eq!(build.name, "build");
    assert_eq!(build.window_count, 2);
    assert_eq!(build.pane_count, 3, "agent sessions are not panes");
    assert_eq!(build.attached_client_count, 1);
    assert_eq!(
        build.active_resource,
        Some(ResourceId::satellite(host.clone(), 102)),
        "the active window's active pane, re-tagged"
    );
    let logs = &row.sessions[1];
    assert_eq!(logs.pane_count, 1);
    assert_eq!(
        logs.active_resource,
        Some(ResourceId::satellite(host, 200)),
        "no remembered focus falls back to the first window's first terminal"
    );
}

/// L1 §9.1: a satellite's window and client ids are its own. A merged
/// row names no hub window (so `window:N` selectors never reach it) and
/// no hub client: the input holder is the hub's lease-ledger holder or a
/// foreign id, and viewers are foreign ids.
#[test]
fn merged_satellite_rows_carry_no_satellite_window_or_client_ids() {
    use phux_protocol::ClientId as WireClientId;
    let host = SatelliteHost::from("edge");
    let mut aggregate =
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1));
    let mut held = ResourceInfo::new(ResourceId::local(100), WindowId::new(1), 80, 24)
        .with_viewers(vec![WireClientId::new(2)]);
    held.input_holder = Some(WireClientId::new(3));
    let mut foreign = ResourceInfo::new(ResourceId::local(101), WindowId::new(1), 80, 24);
    foreign.input_holder = Some(WireClientId::new(3));
    let holders = std::collections::HashMap::from([(100, crate::state::ClientId(7))]);
    super::merge_satellite_resources(&mut aggregate, &host, vec![held, foreign], &holders);

    let [held, foreign] = aggregate.resources.as_slice() else {
        panic!("two merged rows: {:?}", aggregate.resources);
    };
    for row in [held, foreign] {
        assert_eq!(row.window_id, WindowId::new(0), "no hub window owns it");
    }
    assert_eq!(
        held.input_holder,
        Some(WireClientId::new(7)),
        "the ledger holder"
    );
    assert_eq!(
        held.viewers,
        vec![crate::hub::relay::foreign_client(WireClientId::new(2))]
    );
    assert_eq!(
        foreign.input_holder,
        Some(crate::hub::relay::foreign_client(WireClientId::new(3))),
        "a satellite-local holder never reads as hub client 3"
    );
}

/// A satellite that reports a Satellite-tagged active pane (chaining)
/// yields no active pane rather than an unroutable id.
#[test]
fn chained_active_pane_is_dropped() {
    let session = SessionId::new(1);
    let chained = ResourceId::satellite(SatelliteHost::from("deeper"), 5);
    let sat = SessionSnapshot::new(session, WindowId::new(1), chained.clone())
        .with_sessions(vec![SessionInfo::new(session, "s").with_window_count(1)])
        .with_windows(vec![
            WindowInfo::new(WindowId::new(1), session, "w").with_active_resource(Some(chained)),
        ]);
    let row = satellite_host_inventory(&SatelliteHost::from("edge"), &sat);
    assert_eq!(row.sessions[0].active_resource, None);
}

/// A satellite that reports the session's recent input target
/// (`SESSION_STATE.recent_input`) is read over its legacy window.
#[test]
fn reported_recent_input_wins_over_the_legacy_window() {
    let host = SatelliteHost::from("edge");
    let mut sat = satellite_snapshot();
    sat.sessions[0] = sat.sessions[0]
        .clone()
        .with_recent_input(Some(ResourceId::local(101)));
    let row = satellite_host_inventory(&host, &sat);
    assert_eq!(
        row.sessions[0].active_resource,
        Some(ResourceId::satellite(host, 101))
    );
}
