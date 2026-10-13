use sha2::{Digest, Sha256};

use super::{Group, LayoutDecodeError, LayoutNode, LayoutState, ResourceId, leaves};

pub(super) fn terminal_id(terminal: &ResourceId) -> [u8; 16] {
    let mut hash = Sha256::new();
    hash.update(b"phux.layout.window-id/v1\0");
    match terminal {
        ResourceId::Local { id } => {
            hash.update([0]);
            hash.update(id.to_be_bytes());
        }
        ResourceId::Satellite { host, id } => {
            hash.update([1]);
            hash.update(host.as_str().as_bytes());
            hash.update([0]);
            hash.update(id.to_be_bytes());
        }
    }
    let digest = hash.finalize();
    let mut id = [0; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

pub(super) fn tree_id(tree: &LayoutNode) -> [u8; 16] {
    leaves(tree).first().map_or([0; 16], terminal_id)
}

pub(super) fn seed_id(state: &LayoutState) -> [u8; 16] {
    state.tree.as_ref().map_or([0; 16], tree_id)
}

/// Reusing a removed seed must not reuse the surviving group's identity.
pub(super) fn fresh_id(seed: &ResourceId, groups: &[Group]) -> [u8; 16] {
    let mut id = terminal_id(seed);
    while id == [0; 16] || groups.iter().any(|group| group.id == id) {
        let mut hash = Sha256::new();
        hash.update(b"phux.layout.window-id/collision/v1\0");
        hash.update(id);
        id.copy_from_slice(&hash.finalize()[..16]);
    }
    id
}

pub(super) fn validate(groups: &[Group]) -> Result<(), LayoutDecodeError> {
    let mut seen = std::collections::HashSet::new();
    if groups
        .iter()
        .any(|group| group.id == [0; 16] || !seen.insert(group.id))
    {
        return Err(LayoutDecodeError::Cbor(
            "zero or duplicate layout window identity".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::legacy::serialize::CborWorkspaceEnvelope;
    use crate::layout::{self, ArrangementDoc, SplitDir};

    fn split() -> ArrangementDoc {
        let mut ws = ArrangementDoc::single(ResourceId::local(1));
        ws.groups[0].state.tree = Some(
            layout::split_at(
                ws.groups[0].state.tree.as_ref().unwrap(),
                &ResourceId::local(1),
                &ResourceId::local(2),
                SplitDir::Horizontal,
                0.5,
            )
            .unwrap(),
        );
        ws
    }

    #[test]
    fn identity_survives_seed_removal_rename_reorder_and_roundtrip() {
        let mut ws = split();
        let id = ws.groups[0].id;
        ws.groups[0].state = LayoutState::single(ResourceId::local(2));
        ws.groups[0].name = "renamed".into();
        ws.add_group("other".into(), ResourceId::local(3));
        ws.groups.swap(0, 1);
        let decoded = ArrangementDoc::decode_cbor(&ws.encode_cbor().unwrap()).unwrap();
        assert_eq!(decoded.groups[1].id, id);
        assert_eq!(decoded, ws);
    }

    #[test]
    fn removed_seed_can_form_a_new_window_without_reusing_retained_identity() {
        let mut ws = split();
        let retained = ws.groups[0].id;
        ws.groups[0].state = LayoutState::single(ResourceId::local(2));
        ws.add_group("reused seed".into(), ResourceId::local(1));
        assert_eq!(ws.groups[0].id, retained);
        assert_ne!(ws.groups[1].id, retained);
        assert_eq!(
            ArrangementDoc::decode_cbor(&ws.encode_cbor().unwrap()).unwrap(),
            ws
        );
    }

    #[test]
    fn zero_duplicate_and_missing_ids_are_refused() {
        let mut ws = split();
        ws.add_group("two".into(), ResourceId::local(3));
        let bytes = ws.encode_cbor().unwrap();
        for invalid in [[0; 16], ws.groups[0].id] {
            let mut envelope: CborWorkspaceEnvelope =
                ciborium::de::from_reader(bytes.as_slice()).unwrap();
            envelope.windows[1].id = invalid;
            let mut forged = Vec::new();
            ciborium::ser::into_writer(&envelope, &mut forged).unwrap();
            assert!(ArrangementDoc::decode_cbor(&forged).is_err());
        }
        let mut value: ciborium::Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        let ciborium::Value::Map(fields) = &mut value else {
            panic!("envelope")
        };
        let (_, ciborium::Value::Array(windows)) = fields
            .iter_mut()
            .find(|(key, _)| key.as_text() == Some("windows"))
            .unwrap()
        else {
            panic!("windows")
        };
        let ciborium::Value::Map(window) = &mut windows[0] else {
            panic!("window")
        };
        window.retain(|(key, _)| key.as_text() != Some("id"));
        let mut forged = Vec::new();
        ciborium::ser::into_writer(&value, &mut forged).unwrap();
        assert!(
            ArrangementDoc::decode_cbor(&forged)
                .unwrap_err()
                .to_string()
                .contains("id")
        );
    }

    #[test]
    fn satellite_and_local_seed_domains_are_distinct() {
        assert_ne!(
            terminal_id(&ResourceId::local(1)),
            terminal_id(&ResourceId::satellite("peer", 1))
        );
        assert_ne!(
            terminal_id(&ResourceId::satellite("peer", 1)),
            terminal_id(&ResourceId::satellite("other", 1))
        );
    }
}
