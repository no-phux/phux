use super::*;
use phux_protocol::kinds::{KINDS, command_kind};

#[test]
fn every_app_is_listed() {
    // By kind, not by address: the apps are zero-sized, so every app
    // reference may share one address.
    for spec in &KINDS {
        let kind = crate::id_bridge::core_kind(spec.kind).expect("a served kind");
        assert!(
            APP_KINDS.contains(&kind),
            "{kind} has an app that APPS does not list"
        );
    }
    assert_eq!(APP_KINDS.len(), KINDS.len(), "APPS lists an app twice");
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

/// A host-addressed method its catalog row says an app serves reaches that
/// app, though it names no resource; `tests/lifecycle/mirrors.rs` drives
/// `MIRROR` through the production dispatch to `mirror::handle`.
#[test]
fn a_mirror_routes_to_the_surface_app() {
    let mirror = Command::Mirror {
        host: None,
        op: phux_protocol::wire::frame::MirrorOp::Targets,
    };
    let provide = Command::MirrorProvide {
        targets: "[]".to_owned(),
    };
    assert_eq!(owning_kind(&mirror), Some(ResourceKind::PixelSurface));
    assert_eq!(owning_kind(&provide), Some(ResourceKind::PixelSurface));
    assert!(app_owning(&mirror).is_some());
}

/// An upgrade resume rebuilds in `APPS` order, and a child's rebuild finds
/// its parent only if the parent's kind already ran.
#[test]
fn a_parent_kind_resumes_before_its_children() {
    let kinds: Vec<ResourceKind> = KINDS
        .iter()
        .filter_map(|spec| crate::id_bridge::core_kind(spec.kind))
        .collect();
    let position = |kind| {
        APP_KINDS
            .iter()
            .position(|listed| *listed == kind)
            .expect("listed")
    };
    for child in kinds.iter().filter(|kind| kind.requires_parent()) {
        for parent in kinds.iter().filter(|kind| kind.can_parent()) {
            assert!(
                position(*parent) < position(*child),
                "{parent} must resume before {child}"
            );
        }
    }
}
