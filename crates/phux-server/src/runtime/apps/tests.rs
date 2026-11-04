use super::*;
use phux_protocol::kinds::{KINDS, command_kind};

#[test]
fn every_app_is_listed() {
    for spec in &KINDS {
        let kind = crate::id_bridge::core_kind(spec.kind).expect("a served kind");
        assert!(
            APPS.iter()
                .any(|app| std::ptr::addr_eq(*app, app_for(kind))),
            "{kind} has an app that APPS does not list"
        );
    }
    assert_eq!(APPS.len(), KINDS.len(), "APPS lists an app twice");
}

#[test]
fn a_facet_command_routes_to_its_kind_and_a_host_command_to_none() {
    let screen = Command::GetScreen {
        terminal_id: phux_protocol::ids::ResourceId::default(),
        request_scrollback: None,
        cells: false,
        format: 0,
    };
    assert_eq!(owning_kind(&screen), Some(ResourceKind::Terminal));
    assert!(app_owning(&screen).is_some());
    assert_eq!(command_kind(&Command::DescribeApi), None);
    assert!(app_owning(&Command::DescribeApi).is_none());
}
