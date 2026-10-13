//! The legacy v3 CBOR envelope an [`ArrangementDoc`] persists as, under L3
//! key `phux.tui.layout/v1` (docs/spec/L3.md §3.2).
//!
//! The envelope's field names (`windows`, `focused_window_index`, ...) are
//! wire format and stay as they are; only the in-memory types were renamed.
//! Earlier envelope versions and missing group identities are refused, never
//! migrated. The envelope uses local serde shim types, so its schema stays
//! pinned independently of the in-memory tree.

use std::io::Cursor;

use phux_protocol::ResourceId;
use thiserror::Error;

use super::{ArrangementDoc, Group, LayoutState, identity, leaves};

/// CBOR shadow types + conversions for layout persistence (L3 metadata).
pub(in crate::layout) mod serialize;

use serialize::{CborLayoutNode, CborResourceId, CborWindow, CborWorkspaceEnvelope, VersionProbe};

/// Current layout envelope version; readers refuse any other.
pub(crate) const LAYOUT_ENVELOPE_VERSION: u8 = 3;

/// Errors returned by [`ArrangementDoc::decode_cbor`].
#[derive(Debug, Error)]
pub enum LayoutDecodeError {
    /// The envelope's `version` byte is one this build doesn't recognise.
    #[error("unsupported layout envelope version: {0}")]
    UnsupportedVersion(u8),
    /// The envelope decodes as CBOR but `Split.ratio` is NaN, infinite,
    /// or outside `(0.0, 1.0)`.
    #[error("malformed layout ratio: {0}")]
    MalformedRatio(f32),
    /// The envelope's CBOR shape failed to decode.
    #[error("cbor decode failure: {0}")]
    Cbor(String),
    /// A leaf names a resource that is not a Terminal. Only Terminal-kind
    /// resources occupy layout slots; an `AgentSession` has no grid to tile.
    #[error("layout leaf {0:?} is not a terminal resource")]
    NonTerminalLeaf(ResourceId),
}

/// Errors returned by [`ArrangementDoc::encode_cbor`].
#[derive(Debug, Error)]
pub enum LayoutEncodeError {
    /// Encoding a layout with no tree (`tree.is_none()`) or no focus
    /// (`focus.is_none()`) is meaningless; the envelope schema
    /// requires both.
    #[error("cannot encode empty layout state")]
    Empty,
    /// The ciborium encoder failed (typically an OOM on the
    /// in-memory `Vec<u8>` buffer — vanishingly rare in practice).
    #[error("cbor encode failure: {0}")]
    Cbor(String),
}

impl ArrangementDoc {
    /// Encode the arrangement as the v3 CBOR envelope (docs/spec/L3.md §3.2).
    ///
    /// # Errors
    /// * [`LayoutEncodeError::Empty`] if there are no groups, or any
    ///   group has no tree/focus (an un-seeded group can't be encoded).
    /// * [`LayoutEncodeError::Cbor`] if ciborium fails to encode.
    pub fn encode_cbor(&self) -> Result<Vec<u8>, LayoutEncodeError> {
        if self.groups.is_empty() {
            return Err(LayoutEncodeError::Empty);
        }
        self.encode_topology_cbor()
    }

    /// Encode shared topology, including an intentionally empty arrangement.
    /// Empty differs from missing metadata: consumers must not synthesize groups.
    ///
    /// # Errors
    /// Returns [`LayoutEncodeError::Empty`] for an unseeded group, or
    /// [`LayoutEncodeError::Cbor`] if serialization fails.
    pub fn encode_topology_cbor(&self) -> Result<Vec<u8>, LayoutEncodeError> {
        let mut windows = Vec::with_capacity(self.groups.len());
        for g in &self.groups {
            if g.id == [0; 16] {
                return Err(LayoutEncodeError::Empty);
            }
            let (Some(tree), Some(focus)) = (g.state.tree.as_ref(), g.state.focus.as_ref()) else {
                return Err(LayoutEncodeError::Empty);
            };
            windows.push(CborWindow {
                id: g.id,
                name: g.name.clone(),
                root: CborLayoutNode::from(tree),
                focused_terminal: CborResourceId::from(focus),
            });
        }
        let envelope = CborWorkspaceEnvelope {
            version: LAYOUT_ENVELOPE_VERSION,
            windows,
            focused_window_index: u32::try_from(self.active).unwrap_or(0),
        };
        let mut buf = Vec::with_capacity(128);
        ciborium::ser::into_writer(&envelope, &mut buf)
            .map_err(|e| LayoutEncodeError::Cbor(e.to_string()))?;
        Ok(buf)
    }

    /// Decode a current v3 layout blob into an [`ArrangementDoc`]. Missing
    /// identities and earlier schema versions are refused without migration
    /// or fallback.
    ///
    /// # Errors
    /// * [`LayoutDecodeError::UnsupportedVersion`] for any version byte
    ///   other than 3.
    /// * [`LayoutDecodeError::MalformedRatio`] if any `Split.ratio` is
    ///   NaN, infinite, or outside `(0.0, 1.0)`.
    /// * [`LayoutDecodeError::Cbor`] for malformed CBOR.
    pub fn decode_cbor(bytes: &[u8]) -> Result<Self, LayoutDecodeError> {
        // Probe the version, then decode the whole matching envelope.
        let probe: VersionProbe = ciborium::de::from_reader(Cursor::new(bytes))
            .map_err(|e| LayoutDecodeError::Cbor(e.to_string()))?;
        if probe.version != LAYOUT_ENVELOPE_VERSION {
            return Err(LayoutDecodeError::UnsupportedVersion(probe.version));
        }
        Self::decode_current(bytes)
    }

    fn decode_current(bytes: &[u8]) -> Result<Self, LayoutDecodeError> {
        let envelope: CborWorkspaceEnvelope = ciborium::de::from_reader(Cursor::new(bytes))
            .map_err(|e| LayoutDecodeError::Cbor(e.to_string()))?;
        let mut groups = Vec::with_capacity(envelope.windows.len());
        for w in envelope.windows {
            let tree = w.root.into_layout_node()?;
            let focus: ResourceId = w.focused_terminal.into();
            groups.push(Group {
                id: w.id,
                name: w.name,
                state: LayoutState {
                    tree: Some(tree),
                    focus: Some(focus),
                },
            });
        }
        identity::validate(&groups)?;
        let active = (envelope.focused_window_index as usize).min(groups.len().saturating_sub(1));
        Ok(Self { groups, active })
    }

    /// Decode a layout blob and reject any leaf `is_terminal` refuses.
    ///
    /// The envelope carries bare ids, so the caller supplies what it knows
    /// about resource kinds. Ids the caller cannot classify (a peer session's
    /// panes, say) must return `true`: the check refuses known non-terminal
    /// resources, it does not demand proof of terminal-ness.
    ///
    /// # Errors
    /// Every [`Self::decode_cbor`] error, plus
    /// [`LayoutDecodeError::NonTerminalLeaf`] naming the first refused leaf in
    /// group then depth-first order.
    pub fn decode_cbor_checked(
        bytes: &[u8],
        is_terminal: &dyn Fn(&ResourceId) -> bool,
    ) -> Result<Self, LayoutDecodeError> {
        let arrangement = Self::decode_cbor(bytes)?;
        let refused = arrangement
            .groups
            .iter()
            .filter_map(|group| group.state.tree.as_ref())
            .flat_map(leaves)
            .find(|leaf| !is_terminal(leaf));
        refused.map_or(Ok(arrangement), |leaf| {
            Err(LayoutDecodeError::NonTerminalLeaf(leaf))
        })
    }
}
