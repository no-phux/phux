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
