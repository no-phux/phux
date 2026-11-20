use super::*;
use phux_protocol::kinds::KINDS;

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

/// Exactly one kind is driven interactively: the `InteractiveApp` helpers
/// ask the first app that answers, so a second would be silently ignored.
#[test]
fn exactly_one_app_is_interactive() {
    let interactive = APP_KINDS
        .iter()
        .filter(|kind| app_for(**kind).interactive().is_some())
        .count();
    assert_eq!(interactive, 1, "one app runs the interactive half");
    assert!(super::interactive().is_some());
}
