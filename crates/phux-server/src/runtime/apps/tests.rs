use super::*;
use phux_protocol::kinds::{KINDS, command_kind};

#[test]
fn every_catalog_kind_has_an_app() {
    for spec in &KINDS {
        let kind = crate::resource::core_kind(spec.kind).expect("a served kind");
        assert_eq!(app_for(kind).expect("an app").kind(), kind);
    }
}

#[test]
fn a_facet_command_routes_to_its_kind_and_a_host_command_to_none() {
    let screen = Command::GetScreen {
        terminal_id: phux_protocol::ids::ResourceId::default(),
        request_scrollback: None,
        cells: false,
        format: 0,
    };
    assert_eq!(
        app_owning(&screen).map(ResourceApp::kind),
        Some(ResourceKind::Terminal)
    );
    assert_eq!(command_kind(&Command::DescribeApi), None);
    assert!(app_owning(&Command::DescribeApi).is_none());
}

/// A resource frame reaches the app of the kind it names; a session-level
/// frame names none.
#[test]
fn a_frame_names_its_resource_only_when_it_is_about_one() {
    let ack = FrameKind::ResizeTerminal {
        terminal_id: phux_protocol::ids::ResourceId::local(7),
        cols: 80,
        rows: 24,
        cell_px: None,
    };
    assert!(frame_resource(&ack).is_some());
    assert!(frame_resource(&FrameKind::Detach).is_none());
}

/// An id that resolves to nothing local stays with the Terminal app, which
/// relays a satellite's and drops a stale one.
#[test]
fn an_unresolved_frame_goes_to_the_terminal_app() {
    let state = crate::state::SharedState::default();
    let frame = FrameKind::ResizeTerminal {
        terminal_id: phux_protocol::ids::ResourceId::local(7),
        cols: 80,
        rows: 24,
        cell_px: None,
    };
    assert_eq!(frame_app(&state, &frame).kind(), ResourceKind::Terminal);
}
