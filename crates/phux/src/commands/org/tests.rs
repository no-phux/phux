use phux_protocol::ids::KEY_MIN_PREFIX;

use super::*;

/// A key prints as its shortest prefix no other listed key shares, at
/// least six digits, and that prefix is the protocol key's own text.
#[test]
fn keys_print_as_their_shortest_unique_prefix() {
    let mut near = [7; 16];
    near[15] = 8;
    let (a, b) = (
        ResourceKey::new([7; 16]).expect("non-zero"),
        ResourceKey::new(near).expect("non-zero"),
    );
    let alone = KeyNames::new([*a.as_bytes()]).display(*a.as_bytes());
    assert_eq!(alone.len(), 1 + KEY_MIN_PREFIX);
    assert!(a.to_string().starts_with(&alone[1..]), "{alone} vs {a}");
    let crowded = KeyNames::new([*a.as_bytes(), *b.as_bytes()]);
    let (da, db) = (
        crowded.display(*a.as_bytes()),
        crowded.display(*b.as_bytes()),
    );
    assert!(da.len() > 1 + KEY_MIN_PREFIX, "{da}");
    assert_ne!(da, db);
    assert!(!b.to_string().starts_with(&da[1..]), "{da} also names {b}");
}

/// A bare session name renames the session; every pane form labels a
/// resource.
#[test]
fn rename_tells_a_pane_from_a_session() {
    assert!(!names_resource("work"));
    assert!(names_resource("@3"));
    assert!(names_resource("work:0.1"));
    assert!(names_resource("^ab23cd"));
    assert!(!names_resource(""));
}

#[test]
fn restore_summarizes_and_names_each_refusal() {
    use phux_protocol::ids::OperationId;
    use phux_protocol::wire::frame::Refusal;
    let ok = ResourceKey::new([1; 16]).expect("non-zero");
    let refused = ResourceKey::new([2; 16]).expect("non-zero");
    let operation = OperationId::new([3; 16]).expect("non-zero");
    let results = vec![
        (
            ok,
            Ok(Committed::new(
                operation,
                phux_protocol::ids::Revision::new(2),
            )),
        ),
        (refused, Err(OrgError::Refused(Refusal::ExecutionUncertain))),
    ];
    let lines = restore_lines(&KeyNames::default(), &results, Some("work"), false);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[0], "relaunched 1 of 2 panes in work");
    assert!(lines[1].contains("refused"), "{lines:?}");
    let lines = restore_lines(&KeyNames::default(), &results, None, true);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(lines[1].ends_with("relaunched"), "{lines:?}");
}
