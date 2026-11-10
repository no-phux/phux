use super::{InputOperationId, ResourceKind};

#[test]
fn resource_kind_tags_are_stable_and_unknown_round_trips() {
    assert_eq!(ResourceKind::Terminal.as_wire(), 0);
    assert_eq!(ResourceKind::AgentSession.as_wire(), 1);
    assert_eq!(ResourceKind::from_wire(0), ResourceKind::Terminal);
    assert_eq!(ResourceKind::from_wire(1), ResourceKind::AgentSession);
    assert_eq!(
        ResourceKind::from_wire(200),
        ResourceKind::Unknown { tag: 200 }
    );
    assert_eq!(ResourceKind::Unknown { tag: 200 }.as_wire(), 200);
    assert_eq!(ResourceKind::default(), ResourceKind::Terminal);
    assert!(ResourceKind::Terminal.is_terminal());
    assert!(!ResourceKind::AgentSession.is_terminal());
    assert_eq!(ResourceKind::AgentSession.to_string(), "agent_session");
    assert_eq!(ResourceKind::Unknown { tag: 9 }.to_string(), "unknown(9)");
}

#[test]
fn document_and_leaf_kinds_are_derived_from_one_place() {
    let documents = [
        ResourceKind::AccessibilityTree,
        ResourceKind::PixelSurface,
        ResourceKind::Document,
    ];
    for kind in documents {
        assert!(kind.is_document() && kind.can_occupy_leaf(), "{kind}");
    }
    for kind in [ResourceKind::Terminal, ResourceKind::Unknown { tag: 90 }] {
        assert!(!kind.is_document() && kind.can_occupy_leaf(), "{kind}");
    }
    let agent = ResourceKind::AgentSession;
    assert!(!agent.is_document() && !agent.can_occupy_leaf());
}

#[test]
fn input_operation_id_rejects_zero_and_redacts_debug() {
    assert!(InputOperationId::new([0; 16]).is_none());
    let id = InputOperationId::new([0x5a; 16]).expect("non-zero id");
    assert_eq!(id.as_bytes(), &[0x5a; 16]);
    assert_eq!(format!("{id:?}"), "InputOperationId(<redacted>)");
    assert!(!format!("{id:?}").contains("5a"));
}
