use sha2::{Digest, Sha256};

use super::{LayoutDecodeError, LayoutNode, LayoutState, TerminalId, WindowState, leaves};

pub(super) fn terminal_id(terminal: &TerminalId) -> [u8; 16] {
    let mut hash = Sha256::new();
    hash.update(b"phux.layout.window-id/v1\0");
    match terminal {
        TerminalId::Local { id } => {
            hash.update([0]);
            hash.update(id.to_be_bytes());
        }
        TerminalId::Satellite { host, id } => {
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

/// Reusing a removed seed must not reuse the surviving window's identity.
pub(super) fn fresh_id(seed: &TerminalId, windows: &[WindowState]) -> [u8; 16] {
    let mut id = terminal_id(seed);
    while id == [0; 16] || windows.iter().any(|window| window.id == id) {
        let mut hash = Sha256::new();
        hash.update(b"phux.layout.window-id/collision/v1\0");
        hash.update(id);
        id.copy_from_slice(&hash.finalize()[..16]);
    }
    id
}

pub(super) fn validate(windows: &[WindowState]) -> Result<(), LayoutDecodeError> {
    let mut seen = std::collections::HashSet::new();
    if windows
        .iter()
        .any(|window| window.id == [0; 16] || !seen.insert(window.id))
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
    use crate::layout::serialize::CborWorkspaceEnvelope;
    use crate::layout::{self, SplitDir, Workspace};

    fn split() -> Workspace {
        let mut ws = Workspace::single(TerminalId::local(1));
        ws.windows[0].state.tree = Some(
            layout::split_at(
                ws.windows[0].state.tree.as_ref().unwrap(),
                &TerminalId::local(1),
                &TerminalId::local(2),
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
        let id = ws.windows[0].id;
        ws.windows[0].state = LayoutState::single(TerminalId::local(2));
        ws.windows[0].name = "renamed".into();
        ws.add_window("other".into(), TerminalId::local(3));
        ws.windows.swap(0, 1);
        let decoded = Workspace::decode_cbor(&ws.encode_cbor().unwrap()).unwrap();
        assert_eq!(decoded.windows[1].id, id);
        assert_eq!(decoded, ws);
    }

    #[test]
    fn removed_seed_can_form_a_new_window_without_reusing_retained_identity() {
        let mut ws = split();
        let retained = ws.windows[0].id;
        ws.windows[0].state = LayoutState::single(TerminalId::local(2));
        ws.add_window("reused seed".into(), TerminalId::local(1));
        assert_eq!(ws.windows[0].id, retained);
        assert_ne!(ws.windows[1].id, retained);
        assert_eq!(
            Workspace::decode_cbor(&ws.encode_cbor().unwrap()).unwrap(),
            ws
        );
    }

    #[test]
    fn zero_duplicate_and_missing_ids_are_refused() {
        let mut ws = split();
        ws.add_window("two".into(), TerminalId::local(3));
        let bytes = ws.encode_cbor().unwrap();
        for invalid in [[0; 16], ws.windows[0].id] {
            let mut envelope: CborWorkspaceEnvelope =
                ciborium::de::from_reader(bytes.as_slice()).unwrap();
            envelope.windows[1].id = invalid;
            let mut forged = Vec::new();
            ciborium::ser::into_writer(&envelope, &mut forged).unwrap();
            assert!(Workspace::decode_cbor(&forged).is_err());
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
            Workspace::decode_cbor(&forged)
                .unwrap_err()
                .to_string()
                .contains("id")
        );
    }

    #[test]
    fn satellite_and_local_seed_domains_are_distinct() {
        assert_ne!(
            terminal_id(&TerminalId::local(1)),
            terminal_id(&TerminalId::satellite("peer", 1))
        );
        assert_ne!(
            terminal_id(&TerminalId::satellite("peer", 1)),
            terminal_id(&TerminalId::satellite("other", 1))
        );
    }
}
