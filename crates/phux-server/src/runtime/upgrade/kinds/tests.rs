use super::*;
use phux_protocol::kinds::KINDS;

#[test]
fn every_app_is_listed() {
    for spec in &KINDS {
        let kind = crate::id_bridge::core_kind(spec.kind).expect("a served kind");
        assert!(
            KIND_ORDER.contains(&kind),
            "{kind} is missing from upgrade order"
        );
    }
    assert_eq!(KIND_ORDER.len(), KINDS.len(), "a kind is listed twice");
}

/// An upgrade resume rebuilds in `KIND_ORDER` order, and a child's rebuild finds
/// its parent only if the parent's kind already ran.
#[test]
fn a_parent_kind_resumes_before_its_children() {
    let kinds: Vec<ResourceKind> = KINDS
        .iter()
        .filter_map(|spec| crate::id_bridge::core_kind(spec.kind))
        .collect();
    let position = |kind| {
        KIND_ORDER
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
