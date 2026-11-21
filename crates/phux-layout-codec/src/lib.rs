//! Frozen v3 CBOR envelope for the `phux.tui.layout/v1` metadata bridge.
//! Field names, versions and representation are kept independent of client
//! focus state and durable organization records.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::collections::HashSet;
use std::io::Cursor;

use phux_core::SplitDir;
use phux_protocol::ResourceId;
use thiserror::Error;

mod serialize;

use serialize::{
    CborLayoutNode, CborResourceId, CborSlot, CborWindow, CborWorkspaceEnvelope, VersionProbe,
};

const VERSION: u8 = 3;

/// The envelope's split tree in the legacy live-handle space.
pub type LayoutNode = phux_core::LayoutNode<ResourceId>;

/// One decoded group.
#[derive(Debug)]
pub struct Group {
    /// Required nonzero stable group id.
    pub id: [u8; 16],
    /// Display name.
    pub name: String,
    /// Live-handle topology.
    pub tree: LayoutNode,
    /// Focus reported by the writer.
    pub focus: ResourceId,
    /// The saved tab lock bit.
    pub locked: bool,
}

impl Group {
    /// View this group for encoding without cloning the tree.
    #[must_use]
    pub fn borrow(&self) -> GroupRef<'_> {
        GroupRef {
            id: self.id,
            name: &self.name,
            tree: &self.tree,
            focus: &self.focus,
            locked: self.locked,
        }
    }
}

/// One legacy group to encode.
#[derive(Debug)]
pub struct GroupRef<'a> {
    /// Required nonzero stable group id.
    pub id: [u8; 16],
    /// Display name.
    pub name: &'a str,
    /// Live-handle topology.
    pub tree: &'a LayoutNode,
    /// Focus reported by the writer.
    pub focus: &'a ResourceId,
    /// The saved tab lock bit.
    pub locked: bool,
}

/// One slot's tabs, in strip order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// Every tab, in strip order.
    pub strip: Vec<ResourceId>,
    /// The stored default: what a fresh attach and older clients show.
    pub default: usize,
    /// What this client shows: the leaf its group's tree holds.
    pub shown: usize,
}

impl Slot {
    /// The tab this client shows.
    #[must_use]
    pub fn showing(&self) -> &ResourceId {
        &self.strip[self.shown]
    }
}

/// Decoded topology and the active group index.
#[derive(Debug)]
pub struct Document {
    /// Groups in presentation order.
    pub groups: Vec<Group>,
    /// Active group, clamped when reading older values.
    pub active: usize,
    /// Slots carried by an additive v3 field when present.
    pub slots: Vec<Slot>,
}

/// Errors from decoding the legacy layout envelope.
#[derive(Debug, Error)]
pub enum LayoutDecodeError {
    /// The envelope's `version` byte is one this build doesn't recognise.
    #[error("unsupported layout envelope version: {0}")]
    UnsupportedVersion(u8),
    /// The envelope decodes as CBOR but `Split.ratio` is NaN, infinite,
    /// or outside `[0.0, 1.0]`.
    #[error("malformed layout ratio: {0}")]
    MalformedRatio(f32),
    /// The envelope's CBOR shape failed to decode.
    #[error("cbor decode failure: {0}")]
    Cbor(String),
}

/// Errors from encoding the legacy layout envelope.
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

/// Encode legacy topology, including an intentionally empty arrangement.
///
/// # Errors
/// A group lacks an identity or the encoder cannot write the envelope.
pub fn encode<'a>(
    groups: impl IntoIterator<Item = GroupRef<'a>>,
    active: usize,
    slots: &[Slot],
) -> Result<Vec<u8>, LayoutEncodeError> {
    let windows = groups
        .into_iter()
        .map(|group| {
            if group.id == [0; 16] {
                return Err(LayoutEncodeError::Empty);
            }
            Ok(CborWindow {
                id: group.id,
                name: group.name.to_owned(),
                root: CborLayoutNode::from(group.tree),
                focused_terminal: CborResourceId::from(group.focus),
                locked: group.locked,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let envelope = CborWorkspaceEnvelope {
        version: VERSION,
        windows,
        focused_window_index: u32::try_from(active).unwrap_or(0),
        slots: slots
            .iter()
            .map(|slot| CborSlot {
                strip: slot.strip.iter().map(CborResourceId::from).collect(),
                default: u32::try_from(slot.default).unwrap_or(0),
                shown: u32::try_from(slot.shown).unwrap_or(0),
            })
            .collect(),
    };
    let mut bytes = Vec::with_capacity(128);
    ciborium::ser::into_writer(&envelope, &mut bytes)
        .map_err(|e| LayoutEncodeError::Cbor(e.to_string()))?;
    Ok(bytes)
}

/// Decode the current v3 envelope. Earlier versions are refused.
///
/// # Errors
/// Unsupported version, invalid identity or ratio, or malformed CBOR.
pub fn decode(bytes: &[u8]) -> Result<Document, LayoutDecodeError> {
    let probe: VersionProbe = ciborium::de::from_reader(Cursor::new(bytes))
        .map_err(|e| LayoutDecodeError::Cbor(e.to_string()))?;
    if probe.version != VERSION {
        return Err(LayoutDecodeError::UnsupportedVersion(probe.version));
    }
    let envelope: CborWorkspaceEnvelope = ciborium::de::from_reader(Cursor::new(bytes))
        .map_err(|e| LayoutDecodeError::Cbor(e.to_string()))?;
    let mut groups = Vec::with_capacity(envelope.windows.len());
    for window in envelope.windows {
        let tree = window.root.into_layout_node()?;
        groups.push(Group {
            id: window.id,
            name: window.name,
            tree,
            focus: window.focused_terminal.into(),
            locked: window.locked,
        });
    }
    let mut seen = HashSet::new();
    if groups
        .iter()
        .any(|group| group.id == [0; 16] || !seen.insert(group.id))
    {
        return Err(LayoutDecodeError::Cbor(
            "zero or duplicate layout window identity".into(),
        ));
    }
    let active = (envelope.focused_window_index as usize).min(groups.len().saturating_sub(1));
    let slots = envelope
        .slots
        .into_iter()
        .map(|slot| {
            let strip: Vec<ResourceId> = slot.strip.into_iter().map(Into::into).collect();
            let at = |i: u32| usize::try_from(i).ok().filter(|&i| i < strip.len());
            match (at(slot.default), at(slot.shown)) {
                (Some(default), Some(shown)) if strip.len() > 1 => Ok(Slot {
                    strip,
                    default,
                    shown,
                }),
                _ => Err(LayoutDecodeError::Cbor("malformed slot".to_owned())),
            }
        })
        .collect::<Result<_, _>>()?;
    Ok(Document {
        groups,
        active,
        slots,
    })
}

#[cfg(test)]
mod tests;
