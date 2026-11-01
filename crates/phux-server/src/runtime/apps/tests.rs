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

#[test]
fn a_stream_frame_addresses_its_resource_and_a_viewport_vote_none() {
    use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
    use phux_protocol::wire::frame::{FrameKind, ViewportInfo};

    let pane = ResourceId::local(7);
    let ack = FrameKind::FrameAck {
        terminal_id: pane.clone(),
        stream_id: StreamId::new(1).expect("nonzero"),
        bootstrap_id: BootstrapId::new(1).expect("nonzero"),
        seq: 3,
    };
    assert_eq!(addressed_resource(&ack), Some(&pane));
    let resize = FrameKind::ResizeTerminal {
        terminal_id: pane.clone(),
        cols: 80,
        rows: 24,
        cell_px: None,
    };
    assert_eq!(addressed_resource(&resize), Some(&pane));
    let vote = FrameKind::ViewportResize {
        viewport: ViewportInfo::new(80, 24),
    };
    assert_eq!(addressed_resource(&vote), None);
}

#[test]
fn an_unresolved_stream_frame_takes_the_terminal_path() {
    let state = crate::state::SharedState::default();
    let unknown = phux_protocol::ids::ResourceId::local(999_999);
    assert_eq!(
        app_addressed(&state, &unknown).kind(),
        ResourceKind::Terminal
    );
}
