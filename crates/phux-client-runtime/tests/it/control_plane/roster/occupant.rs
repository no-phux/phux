//! The `phux.pane-occupant/v1` foreground names the pane (#2377) and
//! survives a refresh the snapshot cannot carry it through.
use super::*;
use phux_protocol::wire::frame::RESOURCE_PANE_OCCUPANT_KEY;

fn occupant_changed(plane: &mut ControlPlane, foreground: &str) {
    let record = format!(r#"{{"foreground":"{foreground}","is_pane_shell":false}}"#);
    metadata_changed(plane, RESOURCE_PANE_OCCUPANT_KEY, Some(record.as_bytes()));
}

#[test]
fn the_foreground_names_the_pane_and_follows_it() {
    let mut plane = settled();
    let label = |plane: &ControlPlane| plane.topology().unwrap().pane(&terminal()).unwrap().label();
    occupant_changed(&mut plane, "claude");
    assert!(label(&plane).starts_with("claude"), "{}", label(&plane));
    assert!(
        plane
            .take_events()
            .iter()
            .any(|event| matches!(event, Event::BackgroundWork { .. })),
        "the event bindings re-read the label on"
    );
    occupant_changed(&mut plane, "npm");
    let _ = refresh_after_settling(&mut plane, snapshot());
    assert!(
        label(&plane).starts_with("npm"),
        "a refresh keeps it: {}",
        label(&plane)
    );
}

/// `phux.parent/v1` names the pane a pane was started from, and a refresh
/// keeps it as it keeps the foreground.
#[test]
fn the_parent_folds_the_pane_and_survives_a_refresh() {
    use phux_protocol::wire::frame::RESOURCE_PARENT_KEY;
    let mut plane = settled();
    let parent = |plane: &ControlPlane| {
        plane
            .topology()
            .unwrap()
            .pane(&terminal())
            .unwrap()
            .parent
            .clone()
    };
    metadata_changed(&mut plane, RESOURCE_PARENT_KEY, Some(b"7"));
    assert_eq!(parent(&plane), Some(ResourceId::local(7)));
    assert!(
        (plane.take_events().iter()).any(|event| matches!(event, Event::TopologyChanged)),
        "lists re-read the topology"
    );
    let _ = refresh_after_settling(&mut plane, snapshot());
    assert_eq!(
        parent(&plane),
        Some(ResourceId::local(7)),
        "a refresh keeps it"
    );
    metadata_changed(&mut plane, RESOURCE_PARENT_KEY, None);
    assert_eq!(parent(&plane), None, "a delete makes it a root");
}
