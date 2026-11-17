//! The `phux.pane-occupant/v1` foreground names the pane (#2377) and
//! survives a refresh the snapshot cannot carry it through.
use super::*;
use phux_protocol::wire::frame::RESOURCE_PANE_OCCUPANT_KEY;

fn occupant_changed(plane: &mut ControlPlane, foreground: &str) {
    let record = format!(r#"{{"foreground":"{foreground}","is_pane_shell":false}}"#);
    plane
        .feed(FrameKind::MetadataChanged {
            scope: Scope::Resource(terminal()),
            key: RESOURCE_PANE_OCCUPANT_KEY.to_owned(),
            value: Some(record.into_bytes()),
            actor: None,
        })
        .unwrap();
}

#[test]
fn the_foreground_names_the_pane_and_follows_it() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, None);
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
    let request_id = plane.refresh_topology().unwrap();
    let _ = outbound(&mut plane);
    answer_state(&mut plane, request_id, snapshot());
    assert!(
        label(&plane).starts_with("npm"),
        "a refresh keeps it: {}",
        label(&plane)
    );
}
