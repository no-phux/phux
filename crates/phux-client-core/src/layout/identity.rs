//! Group and entry identities: random 16-byte ids, minted by whoever creates
//! the group or entry and stable while it exists.
//!
//! Identities are never derived from live resource handles: a handle is
//! reused after a server restart, so a derived identity would collide across
//! restarts. The bytes come from the standard library's per-process random
//! hasher keys mixed with a process-wide counter, which keeps this crate free
//! of an RNG dependency on every target.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{Group, LayoutDecodeError};

static MINTED: AtomicU64 = AtomicU64::new(0);

/// A fresh non-zero random 16-byte identity for a group or an arrangement
/// entry.
#[must_use]
pub fn fresh_id() -> [u8; 16] {
    loop {
        let n = MINTED.fetch_add(1, Ordering::Relaxed);
        let state = RandomState::new();
        let mut id = [0_u8; 16];
        for (half, chunk) in id.as_chunks_mut::<8>().0.iter_mut().enumerate() {
            let mut hasher = state.build_hasher();
            hasher.write_u64(n);
            hasher.write_usize(half);
            chunk.copy_from_slice(&hasher.finish().to_be_bytes());
        }
        if id != [0; 16] {
            return id;
        }
    }
}

/// A fresh identity no group in `groups` already holds.
pub(super) fn unused_id(groups: &[Group]) -> [u8; 16] {
    loop {
        let id = fresh_id();
        if groups.iter().all(|group| group.id != id) {
            return id;
        }
    }
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
    use crate::layout::{self, ArrangementDoc, LayoutState, SplitDir};
    use phux_protocol::ResourceId;

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

    /// The same seed pane does not yield the same identity: ids are random,
    /// so a handle reused after a restart cannot collide with a group that
    /// outlived it.
    #[test]
    fn identities_are_random_not_derived_from_the_seed_handle() {
        let a = ArrangementDoc::single(ResourceId::local(1));
        let b = ArrangementDoc::single(ResourceId::local(1));
        assert_ne!(a.groups[0].id, b.groups[0].id);
        let ids: std::collections::HashSet<[u8; 16]> = (0..256).map(|_| fresh_id()).collect();
        assert_eq!(ids.len(), 256);
        assert!(!ids.contains(&[0; 16]));
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
}
