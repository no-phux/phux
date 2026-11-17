//! Slot tabs: one layout leaf holding several entries, one of them shown by
//! default (`docs/organization.md#slot-tabs`). The leaf's own entry is the
//! default; `tabs` holds the others in strip order.

use std::collections::HashSet;

use super::{ArrangementError, EntryRef, EntryTarget, Group};
use crate::keys::ResourceKey;

/// `groups`, from a replacement that doesn't carry slot tabs, with each
/// slot's tabs as they stand in `current`.
///
/// A slot is found by its leaf's entry id, else by its default's target
/// (the legacy layout adapter mints fresh ids). A tab the replacement
/// places elsewhere stays there, and a slot the replacement removed loses
/// its tabs, which stay members.
#[must_use]
pub fn carry_tabs(current: &[Group], groups: &[Group]) -> Vec<Group> {
    let slots: Vec<&EntryRef> = current
        .iter()
        .flat_map(|g| g.tree.leaves())
        .filter(|leaf| !leaf.tabs.is_empty())
        .collect();
    if slots.is_empty() {
        return groups.to_vec();
    }
    let placed: HashSet<&EntryTarget> = groups
        .iter()
        .flat_map(|g| g.tree.leaves())
        .flat_map(EntryRef::slot_entries)
        .map(|entry| &entry.target)
        .collect();
    groups
        .iter()
        .map(|g| Group {
            tree: g.tree.map(&mut |leaf: &EntryRef| {
                let old = slots
                    .iter()
                    .find(|s| s.entry == leaf.entry)
                    .or_else(|| slots.iter().find(|s| s.target == leaf.target));
                match old {
                    Some(old) if leaf.tabs.is_empty() => old.carried_onto(leaf, &placed),
                    _ => leaf.clone(),
                }
            }),
            ..g.clone()
        })
        .collect()
}

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's signature")]
pub(super) const fn is_zero(value: &u16) -> bool {
    *value == 0
}

impl EntryRef {
    /// This entry, then its slot's other tabs.
    pub fn slot_entries(&self) -> impl Iterator<Item = &Self> {
        std::iter::once(self).chain(&self.tabs)
    }

    /// Whether this slot shows `resource` in any of its tabs.
    #[must_use]
    pub fn names(&self, resource: ResourceKey) -> bool {
        self.slot_entries()
            .any(|entry| entry.resource() == Some(resource))
    }

    /// The slot's tabs in strip order, each alone, and the default's index.
    #[must_use]
    pub fn strip(&self) -> (Vec<Self>, usize) {
        let mut strip: Vec<Self> = self.tabs.iter().map(Self::alone).collect();
        let shown = usize::from(self.shown_at).min(strip.len());
        strip.insert(shown, self.alone());
        (strip, shown)
    }

    /// A slot over `strip` whose default is `strip[shown]` (clamped);
    /// `None` for an empty strip.
    #[must_use]
    pub fn from_strip(mut strip: Vec<Self>, shown: usize) -> Option<Self> {
        if strip.is_empty() {
            return None;
        }
        let shown = shown.min(strip.len() - 1);
        let mut slot = strip.remove(shown).alone();
        slot.tabs = strip;
        slot.shown_at = u16::try_from(shown).unwrap_or(u16::MAX);
        Some(slot)
    }

    /// This entry without its slot's other tabs.
    #[must_use]
    pub fn alone(&self) -> Self {
        Self::new(self.entry, self.target.clone())
    }

    /// The slot with every tab naming `resource` gone, `None` when none is
    /// left. Losing its default, the slot shows the next tab in strip order.
    #[must_use]
    pub fn without(&self, resource: ResourceKey) -> Option<Self> {
        let (strip, shown) = self.strip();
        let before = strip[..shown]
            .iter()
            .filter(|entry| entry.resource() != Some(resource))
            .count();
        let kept: Vec<Self> = strip
            .into_iter()
            .filter(|entry| entry.resource() != Some(resource))
            .collect();
        Self::from_strip(kept, before)
    }

    /// `leaf` (a replacement's default) with this slot's other tabs around
    /// it, less any the replacement placed itself.
    fn carried_onto(&self, leaf: &Self, placed: &HashSet<&EntryTarget>) -> Self {
        let (strip, shown) = self.strip();
        let mut at = shown;
        let mut kept = Vec::with_capacity(strip.len());
        for (index, entry) in strip.into_iter().enumerate() {
            if index == shown {
                at = kept.len();
                kept.push(leaf.alone());
            } else if !placed.contains(&entry.target) {
                kept.push(entry);
            }
        }
        Self::from_strip(kept, at).unwrap_or_else(|| leaf.clone())
    }

    /// A tab holds no tabs of its own, and the default sits within them.
    pub(super) fn validate_slot(&self) -> Result<(), ArrangementError> {
        let nested = self
            .tabs
            .iter()
            .any(|tab| !tab.tabs.is_empty() || tab.shown_at != 0);
        if nested || usize::from(self.shown_at) > self.tabs.len() {
            return Err(ArrangementError::InvalidSlot(self.entry));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::ResourceKey;

    fn key(n: u8) -> ResourceKey {
        ResourceKey::from_bytes([n; 16])
    }

    fn entry(n: u8) -> EntryRef {
        EntryRef::member([n; 16], key(n))
    }

    fn slot(tabs: &[u8], shown: usize) -> EntryRef {
        EntryRef::from_strip(tabs.iter().map(|n| entry(*n)).collect(), shown).unwrap()
    }

    fn order(slot: &EntryRef) -> (Vec<u8>, usize) {
        let (strip, shown) = slot.strip();
        (strip.iter().map(|e| e.entry[0]).collect(), shown)
    }

    #[test]
    fn the_strip_round_trips_with_the_default_in_place() {
        let s = slot(&[1, 2, 3], 1);
        assert_eq!(s.entry, [2; 16]);
        assert_eq!(order(&s), (vec![1, 2, 3], 1));
        assert!(s.validate_slot().is_ok());
    }

    #[test]
    fn losing_the_default_shows_the_next_tab_and_the_last_tab_ends_the_slot() {
        assert_eq!(
            order(&slot(&[1, 2, 3], 1).without(key(2)).unwrap()),
            (vec![1, 3], 1)
        );
        assert_eq!(
            order(&slot(&[1, 2, 3], 2).without(key(3)).unwrap()),
            (vec![1, 2], 1)
        );
        assert_eq!(
            order(&slot(&[1, 2, 3], 2).without(key(1)).unwrap()),
            (vec![2, 3], 1)
        );
        assert!(entry(1).without(key(1)).is_none());
        assert_eq!(entry(1).without(key(9)), Some(entry(1)));
    }

    fn group(tree: crate::LayoutNode<EntryRef>) -> Group {
        Group {
            id: [0xAA; 16],
            name: None,
            tree,
            locked: false,
        }
    }

    /// A writer that doesn't know tabs sends the default alone; the slot
    /// keeps its tabs, by entry id or (legacy, fresh ids) by target.
    #[test]
    fn a_replacement_without_tabs_keeps_each_slots_tabs() {
        let current = [group(crate::LayoutNode::Leaf(slot(&[1, 2, 3], 1)))];
        let carried = carry_tabs(&current, &[group(crate::LayoutNode::Leaf(entry(2)))]);
        assert_eq!(order(carried[0].tree.leaves()[0]), (vec![1, 2, 3], 1));
        let fresh = EntryRef::member([9; 16], key(2));
        let carried = carry_tabs(&current, &[group(crate::LayoutNode::Leaf(fresh))]);
        let (strip, shown) = carried[0].tree.leaves()[0].strip();
        assert_eq!((strip[shown].entry, strip.len()), ([9; 16], 3));
    }

    /// An explicit placement wins: a tab the old writer put in its own leaf
    /// leaves the slot; a removed slot's tabs are simply not arranged.
    #[test]
    fn a_tab_placed_elsewhere_leaves_its_slot() {
        let current = [group(crate::LayoutNode::Leaf(slot(&[1, 2, 3], 1)))];
        let split = crate::LayoutNode::Split {
            dir: crate::SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(crate::LayoutNode::Leaf(entry(2))),
            right: Box::new(crate::LayoutNode::Leaf(entry(3))),
        };
        let carried = carry_tabs(&current, &[group(split)]);
        let leaves = carried[0].tree.leaves();
        assert_eq!(order(leaves[0]), (vec![1, 2], 1));
        assert_eq!(order(leaves[1]), (vec![3], 0));
        let gone = carry_tabs(&current, &[group(crate::LayoutNode::Leaf(entry(7)))]);
        assert!(gone[0].tree.leaves()[0].tabs.is_empty());
    }

    #[test]
    fn nested_tabs_and_a_stray_default_are_invalid() {
        let mut s = slot(&[1, 2], 0);
        s.shown_at = 5;
        assert!(s.validate_slot().is_err());
        let mut s = slot(&[1, 2], 0);
        s.tabs[0].tabs.push(entry(3));
        assert!(s.validate_slot().is_err());
    }
}
