#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]

use super::*;

fn group(n: u8) -> Group {
    let focus = ResourceId::satellite("edge", u32::from(n));
    Group {
        id: [n; 16],
        name: format!("group {n}"),
        tree: LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(ResourceId::local(u32::from(n)))),
            right: Box::new(LayoutNode::Leaf(focus.clone())),
        },
        focus,
        locked: true,
    }
}

fn encode_raw(value: &impl serde::Serialize) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(value, &mut bytes).unwrap();
    bytes
}

fn envelope() -> CborWorkspaceEnvelope {
    let groups = [group(1), group(2)];
    let bytes = encode(groups.iter().map(Group::borrow), 1, &[]).unwrap();
    ciborium::de::from_reader(bytes.as_slice()).unwrap()
}

#[test]
fn roundtrip_retains_wire_values_and_clamps_active_index() {
    let mut envelope = envelope();
    assert_eq!(envelope.version, VERSION);
    envelope.focused_window_index = 99;
    let document = decode(&encode_raw(&envelope)).unwrap();
    assert_eq!(document.active, 1);
    assert_eq!(document.groups[0].focus, group(1).focus);
    assert_eq!(document.groups[0].tree, group(1).tree);
    assert!(document.groups[0].locked);
}

#[test]
fn version_must_match() {
    for version in [1, 2, 99] {
        let mut envelope = envelope();
        envelope.version = version;
        assert!(matches!(
            decode(&encode_raw(&envelope)),
            Err(LayoutDecodeError::UnsupportedVersion(found)) if found == version
        ));
    }
}

#[test]
fn invalid_ratio_and_zero_duplicate_or_missing_id_are_refused() {
    let mut malformed = envelope();
    let CborLayoutNode::Split { ratio, .. } = &mut malformed.windows[0].root else {
        panic!("split");
    };
    *ratio = 2.0;
    assert!(matches!(
        decode(&encode_raw(&malformed)),
        Err(LayoutDecodeError::MalformedRatio(_))
    ));
    for invalid in [[0; 16], [1; 16]] {
        let mut malformed = envelope();
        malformed.windows[1].id = invalid;
        assert!(decode(&encode_raw(&malformed)).is_err());
    }
    let mut missing: ciborium::Value =
        ciborium::de::from_reader(encode_raw(&envelope()).as_slice()).unwrap();
    let (_, windows) = missing
        .as_map_mut()
        .unwrap()
        .iter_mut()
        .find(|(key, _)| key.as_text() == Some("windows"))
        .unwrap();
    windows.as_array_mut().unwrap()[0]
        .as_map_mut()
        .unwrap()
        .retain(|(key, _)| key.as_text() != Some("id"));
    assert!(
        decode(&encode_raw(&missing))
            .unwrap_err()
            .to_string()
            .contains("id")
    );
}

#[test]
fn slot_tabs_roundtrip_and_malformed_slots_are_refused() {
    let groups = [group(1), group(2)];
    let slot = Slot {
        strip: vec![
            ResourceId::local(1),
            ResourceId::local(2),
            ResourceId::local(3),
        ],
        default: 1,
        shown: 2,
    };
    let bytes = encode(
        groups.iter().map(Group::borrow),
        1,
        std::slice::from_ref(&slot),
    )
    .unwrap();
    assert_eq!(decode(&bytes).unwrap().slots, [slot]);
    let mut forged: CborWorkspaceEnvelope = ciborium::de::from_reader(bytes.as_slice()).unwrap();
    forged.slots[0].shown = 3;
    assert!(matches!(
        decode(&encode_raw(&forged)),
        Err(LayoutDecodeError::Cbor(message)) if message == "malformed slot"
    ));
}
