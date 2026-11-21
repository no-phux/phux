//! Adapts the legacy metadata envelope to client presentation state.

use phux_layout_codec::GroupRef;

use super::{ArrangementDoc, Group, LayoutState};

pub use phux_layout_codec::{LayoutDecodeError, LayoutEncodeError};

impl ArrangementDoc {
    /// Encode a nonempty arrangement as the legacy v3 CBOR envelope.
    ///
    /// # Errors
    /// An empty or unseeded arrangement, or a codec failure.
    pub fn encode_cbor(&self) -> Result<Vec<u8>, LayoutEncodeError> {
        if self.groups.is_empty() {
            return Err(LayoutEncodeError::Empty);
        }
        self.encode_topology_cbor()
    }

    /// Encode shared topology, including an intentionally empty arrangement.
    ///
    /// # Errors
    /// An unseeded group or a codec failure.
    pub fn encode_topology_cbor(&self) -> Result<Vec<u8>, LayoutEncodeError> {
        let groups = self
            .groups
            .iter()
            .map(|group| {
                let (Some(tree), Some(focus)) = (&group.state.tree, &group.state.focus) else {
                    return Err(LayoutEncodeError::Empty);
                };
                Ok(GroupRef {
                    id: group.id,
                    name: &group.name,
                    tree,
                    focus,
                    locked: group.locked,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        phux_layout_codec::encode(groups, self.active, &self.slots)
    }

    /// Decode an arrangement from the legacy v3 CBOR envelope.
    ///
    /// # Errors
    /// Unsupported or malformed payloads are refused by the codec.
    pub fn decode_cbor(bytes: &[u8]) -> Result<Self, LayoutDecodeError> {
        let document = phux_layout_codec::decode(bytes)?;
        Ok(Self {
            groups: document
                .groups
                .into_iter()
                .map(|group| Group {
                    id: group.id,
                    name: group.name,
                    locked: group.locked,
                    state: LayoutState {
                        tree: Some(group.tree),
                        focus: Some(group.focus),
                    },
                })
                .collect(),
            active: document.active,
            slots: document.slots,
        })
    }
}
