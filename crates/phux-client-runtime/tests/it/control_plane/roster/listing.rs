//! `Topology::resources` follows a live retitle and a close like `panes`.
use super::*;

#[test]
fn the_resource_listing_takes_live_titles_and_drops_a_closed_resource() {
    let (mut plane, read) = discovered();
    answer_metadata(&mut plane, read, None);
    let listed_title = |plane: &ControlPlane| {
        let topology = plane.topology().unwrap();
        let resource = topology.resources.iter().find(|r| r.id == terminal());
        resource.and_then(|resource| resource.title.clone())
    };
    event(
        &mut plane,
        Some(terminal()),
        AgentEvent::TitleChanged {
            title: "opencode".into(),
        },
    );
    assert_eq!(listed_title(&plane).as_deref(), Some("opencode"));
    let request_id = plane.refresh_topology().unwrap();
    let _ = outbound(&mut plane);
    answer_state(&mut plane, request_id, snapshot());
    assert_eq!(
        listed_title(&plane).as_deref(),
        Some("opencode"),
        "a refresh keeps the live title, in the listing as in the pane"
    );
    event(
        &mut plane,
        Some(terminal()),
        AgentEvent::ResourceClosed { exit_status: None },
    );
    let topology = plane.topology().unwrap();
    assert!(topology.pane(&terminal()).is_none());
    assert!(!topology.resources.iter().any(|r| r.id == terminal()));
}
