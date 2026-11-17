//! The tree walk between stored and client documents, slot tabs included:
//! a stored leaf's tabs become a [`Slot`] beside the client tree, and a slot
//! is written back as its stored default with the others as tabs.

use std::collections::{HashMap, HashSet};

use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{
    ArrangementNode as WireNode, EntryRef, EntryTarget, SplitDirection,
};

use super::{EntryBinding, HandleMap, WireError};
use crate::layout::{LayoutNode, Slot, SplitDir, identity};

/// Smallest ratio a written split carries; the wire requires the open
/// interval `(0, 1)`, while a client may bank the closed endpoints.
const MIN_WIRE_RATIO: f32 = 0.001;

pub(super) fn node_from_wire(
    node: &WireNode,
    map: &HandleMap,
    entries: &mut HashMap<ResourceId, [u8; 16]>,
    unplaced: &mut Vec<EntryRef>,
    slots: &mut Vec<Slot>,
) -> Option<LayoutNode> {
    match node {
        WireNode::Leaf(entry) => {
            // A second entry for one handle cannot be shown twice.
            let Some(shown) = place(entry, map, entries) else {
                unplaced.push(entry.clone());
                return None;
            };
            if !entry.tabs.is_empty() {
                slots.extend(slot_from_wire(entry, &shown, map, entries, unplaced));
            }
            Some(LayoutNode::Leaf(shown))
        }
        WireNode::Split {
            dir,
            ratio,
            first,
            second,
        } => {
            let first = node_from_wire(first, map, entries, unplaced, slots);
            let second = node_from_wire(second, map, entries, unplaced, slots);
            match (first, second) {
                (Some(left), Some(right)) => Some(LayoutNode::Split {
                    dir: split_from_wire(*dir),
                    ratio: *ratio,
                    left: Box::new(left),
                    right: Box::new(right),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            }
        }
    }
}

pub(super) fn node_to_wire(
    node: &LayoutNode,
    map: &HandleMap,
    binding: &EntryBinding,
    slots: &[Slot],
    used: &mut HashSet<[u8; 16]>,
) -> Result<WireNode, WireError> {
    match node {
        LayoutNode::Leaf(handle) => {
            let Some(slot) = slots.iter().find(|slot| slot.showing() == handle) else {
                return Ok(WireNode::Leaf(unique_entry(
                    entry_for(handle, map, binding)?,
                    used,
                )));
            };
            let mut leaf = unique_entry(entry_for(&slot.strip[slot.default], map, binding)?, used);
            for (index, tab) in slot.strip.iter().enumerate() {
                if index != slot.default {
                    leaf.tabs
                        .push(unique_entry(entry_for(tab, map, binding)?, used));
                }
            }
            leaf.shown_at = u16::try_from(slot.default).unwrap_or(u16::MAX);
            Ok(WireNode::Leaf(leaf))
        }
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Ok(WireNode::Split {
            dir: split_to_wire(*dir),
            ratio: wire_ratio(*ratio),
            first: Box::new(node_to_wire(left, map, binding, slots, used)?),
            second: Box::new(node_to_wire(right, map, binding, slots, used)?),
        }),
    }
}

/// `entry` with an id no earlier entry of this document used.
pub(super) fn unique_entry(mut entry: EntryRef, used: &mut HashSet<[u8; 16]>) -> EntryRef {
    while entry.entry == [0; 16] || !used.insert(entry.entry) {
        entry.entry = identity::fresh_id();
    }
    entry
}

/// `tree` with `entry` split in below it.
pub(super) fn append_leaf(tree: WireNode, entry: EntryRef) -> WireNode {
    WireNode::Split {
        dir: SplitDirection::Vertical,
        ratio: 0.5,
        first: Box::new(tree),
        second: Box::new(WireNode::Leaf(entry)),
    }
}

fn wire_ratio(ratio: f32) -> f32 {
    if ratio.is_nan() {
        0.5
    } else {
        ratio.clamp(MIN_WIRE_RATIO, 1.0 - MIN_WIRE_RATIO)
    }
}

const fn split_from_wire(dir: SplitDirection) -> SplitDir {
    match dir {
        SplitDirection::Horizontal => SplitDir::Horizontal,
        SplitDirection::Vertical => SplitDir::Vertical,
    }
}

const fn split_to_wire(dir: SplitDir) -> SplitDirection {
    match dir {
        SplitDir::Horizontal => SplitDirection::Horizontal,
        SplitDir::Vertical => SplitDirection::Vertical,
    }
}

/// The live handle `entry` shows, claimed in `entries`; `None` when it has
/// none (a target kind this build does not know, or a member not executing
/// here) or another entry already shows it.
fn place(
    entry: &EntryRef,
    map: &HandleMap,
    entries: &mut HashMap<ResourceId, [u8; 16]>,
) -> Option<ResourceId> {
    let handle = match &entry.target {
        EntryTarget::Member(key) => map.handle(key).cloned(),
        EntryTarget::Satellite { host, id } => Some(ResourceId::Satellite {
            host: host.clone(),
            id: *id,
        }),
        _ => None,
    }?;
    if entries.contains_key(&handle) {
        return None;
    }
    entries.insert(handle.clone(), entry.entry);
    Some(handle)
}

/// The slot a stored leaf showing `shown` describes. A tab with no handle
/// is kept for the write-back like any unplaced entry.
fn slot_from_wire(
    entry: &EntryRef,
    shown: &ResourceId,
    map: &HandleMap,
    entries: &mut HashMap<ResourceId, [u8; 16]>,
    unplaced: &mut Vec<EntryRef>,
) -> Option<Slot> {
    let at = usize::from(entry.shown_at).min(entry.tabs.len());
    let mut strip = Vec::with_capacity(entry.tabs.len() + 1);
    let mut default = 0;
    for (index, tab) in entry.tabs.iter().enumerate() {
        if index == at {
            default = strip.len();
            strip.push(shown.clone());
        }
        match place(tab, map, entries) {
            Some(handle) => strip.push(handle),
            None => unplaced.push(tab.clone()),
        }
    }
    if at == entry.tabs.len() {
        default = strip.len();
        strip.push(shown.clone());
    }
    (strip.len() > 1).then_some(Slot {
        strip,
        default,
        shown: default,
    })
}

/// The stored entry for `handle`: its key (or satellite address) under the
/// entry id it was read with, else a fresh one.
fn entry_for(
    handle: &ResourceId,
    map: &HandleMap,
    binding: &EntryBinding,
) -> Result<EntryRef, WireError> {
    let target = match handle {
        ResourceId::Local { .. } => EntryTarget::Member(
            map.key(handle)
                .ok_or_else(|| WireError::UnknownHandle(handle.clone()))?,
        ),
        ResourceId::Satellite { host, id } => EntryTarget::Satellite {
            host: host.clone(),
            id: *id,
        },
    };
    Ok(EntryRef::new(
        binding.entry(handle).unwrap_or_else(identity::fresh_id),
        target,
    ))
}
