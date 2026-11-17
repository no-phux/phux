//! Slot tabs on the client (`docs/organization.md#slot-tabs`): one layout
//! leaf showing one of several terminals.
//!
//! The group trees hold the tab each slot shows, so every tree walk (tiling,
//! focus, zoom, swap, move) is unchanged and a slot moves as one leaf.
//! [`Slot`] holds the rest. Which tab a client shows is its own; only the
//! stored default is written back.

use phux_protocol::ResourceId;

use super::mutation::{MutationError, swap_leaves};
use super::{ArrangementDoc, SplitDir, leaves, split_at};

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

impl ArrangementDoc {
    /// The slot holding `pane` among its tabs.
    #[must_use]
    pub fn slot_of(&self, pane: &ResourceId) -> Option<&Slot> {
        self.slots.iter().find(|slot| slot.strip.contains(pane))
    }

    fn slot_index(&self, pane: &ResourceId) -> Option<usize> {
        self.slots.iter().position(|slot| slot.strip.contains(pane))
    }

    /// The group placing `pane`: in its tree, or as a tab of a slot there.
    #[must_use]
    pub fn group_of(&self, pane: &ResourceId) -> Option<usize> {
        let shown = self.slot_of(pane).map_or(pane, Slot::showing);
        self.groups.iter().position(|group| {
            group
                .state
                .tree
                .as_ref()
                .is_some_and(|tree| leaves(tree).contains(shown))
        })
    }

    /// Show `pane`, a tab of its slot, where that slot's tab shows now.
    /// Client-local: nothing is written. Focus follows when it was on the
    /// slot.
    pub fn show_tab(&mut self, pane: &ResourceId) -> bool {
        let (Some(index), Some(group)) = (self.slot_index(pane), self.group_of(pane)) else {
            return false;
        };
        let slot = &mut self.slots[index];
        let old = slot.showing().clone();
        slot.shown = slot
            .strip
            .iter()
            .position(|p| p == pane)
            .unwrap_or(slot.shown);
        let state = &mut self.groups[group].state;
        if let Some(tree) = state.tree.as_ref() {
            state.tree = Some(swap_leaves(tree, &old, pane));
        }
        if state.focus.as_ref() == Some(&old) {
            state.focus = Some(pane.clone());
        }
        true
    }

    /// Show again, in this document read fresh from the server, each tab
    /// `local` showed instead of its slot's default: which tab shows is the
    /// client's own and survives a re-read.
    pub fn keep_shown(&mut self, local: &Self) {
        for slot in local.slots.iter().filter(|slot| slot.shown != slot.default) {
            let pane = slot.showing().clone();
            if self.slot_of(&pane).is_some() {
                self.show_tab(&pane);
            }
        }
    }

    /// Show the tab `delta` places from the one showing in `pane`'s slot,
    /// wrapping; the tab now shown.
    pub fn cycle_tab(&mut self, pane: &ResourceId, delta: isize) -> Option<ResourceId> {
        let slot = self.slot_of(pane)?;
        let next = slot.strip[wrap(slot.shown, delta, slot.strip.len())].clone();
        self.show_tab(&next).then_some(next)
    }

    /// Move the stored default of `pane`'s slot `delta` places, wrapping,
    /// and show it: what a fresh attach and older clients show next.
    pub fn step_default(&mut self, pane: &ResourceId, delta: isize) -> Option<ResourceId> {
        let index = self.slot_index(pane)?;
        let slot = &mut self.slots[index];
        slot.default = wrap(slot.default, delta, slot.strip.len());
        let next = slot.strip[slot.default].clone();
        self.show_tab(&next);
        Some(next)
    }

    /// Make `pane` the last tab of the slot showing `to`, taking it from
    /// wherever it was (its own leaf, or another slot).
    ///
    /// # Errors
    ///
    /// Refuses the same pane twice, an unarranged `to`, or a move that would
    /// empty the arrangement.
    pub fn add_tab(&mut self, pane: &ResourceId, to: &ResourceId) -> Result<(), MutationError> {
        if pane == to
            || self
                .slot_of(to)
                .is_some_and(|slot| slot.strip.contains(pane))
        {
            return Err(MutationError::SamePane);
        }
        let shown = self.slot_of(to).map_or(to, Slot::showing).clone();
        self.group_of(&shown)
            .ok_or_else(|| MutationError::ForeignTarget(to.clone()))?;
        if self.group_of(pane).is_some() {
            self.close_pane(pane).map_err(|_| MutationError::LastPane)?;
        }
        match self.slot_index(&shown) {
            Some(index) => self.slots[index].strip.push(pane.clone()),
            None => self.slots.push(Slot {
                strip: vec![shown, pane.clone()],
                default: 0,
                shown: 0,
            }),
        }
        Ok(())
    }

    /// Give `pane`, a tab, a leaf of its own split beside its slot.
    ///
    /// # Errors
    ///
    /// Refuses a pane that is not a tab.
    pub fn take_tab(
        &mut self,
        pane: &ResourceId,
        dir: SplitDir,
        ratio: f32,
    ) -> Result<(), MutationError> {
        let foreign = || MutationError::ForeignTarget(pane.clone());
        let group = self.group_of(pane).ok_or_else(foreign)?;
        let beside = self.drop_tab(pane).ok_or_else(foreign)?;
        let state = &mut self.groups[group].state;
        let tree = state.tree.as_ref().ok_or_else(foreign)?;
        state.tree = Some(split_at(tree, &beside, pane, dir, ratio)?);
        state.focus = Some(pane.clone());
        Ok(())
    }

    /// Drop `pane` from its slot, if it is a tab: the slot shows its next
    /// tab when `pane` was showing, and becomes a plain leaf at one tab.
    /// The tab the slot shows after; `None` when `pane` is no tab.
    pub(crate) fn drop_tab(&mut self, pane: &ResourceId) -> Option<ResourceId> {
        let index = self.slot_index(pane)?;
        let group = self.group_of(pane);
        let slot = &mut self.slots[index];
        let at = slot.strip.iter().position(|p| p == pane).unwrap_or(0);
        let was_showing = at == slot.shown;
        slot.strip.remove(at);
        let shift = |i: usize| if i > at { i - 1 } else { i };
        slot.default = shift(slot.default).min(slot.strip.len() - 1);
        slot.shown = shift(slot.shown).min(slot.strip.len() - 1);
        let next = slot.showing().clone();
        if slot.strip.len() < 2 {
            self.slots.remove(index);
        }
        if was_showing && let Some(group) = group {
            let state = &mut self.groups[group].state;
            if let Some(tree) = state.tree.as_ref() {
                state.tree = Some(swap_leaves(tree, pane, &next));
            }
            if state.focus.as_ref() == Some(pane) {
                state.focus = Some(next.clone());
            }
        }
        Some(next)
    }
}

/// `at` moved `delta` places around `len`.
fn wrap(at: usize, delta: isize, len: usize) -> usize {
    let len = isize::try_from(len.max(1)).unwrap_or(isize::MAX);
    let at = isize::try_from(at).unwrap_or(0);
    usize::try_from((at + delta).rem_euclid(len)).unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use phux_protocol::ResourceId;
    use phux_protocol::ids::ResourceKey;
    use phux_protocol::wire::frame::{
        ArrangementDoc as WireDoc, ArrangementGroup as WireGroup, ArrangementNode as WireNode,
        EntryRef, EntryTarget, SplitDirection,
    };

    use crate::layout::wire::{HandleMap, from_protocol, to_protocol};
    use crate::layout::{ArrangementDoc, SplitDir, leaves};

    fn key(n: u8) -> ResourceKey {
        ResourceKey::new([n; 16]).unwrap()
    }

    fn entry(n: u8) -> EntryRef {
        EntryRef::new([0xe0 | n; 16], EntryTarget::Member(key(n)))
    }

    fn id(n: u32) -> ResourceId {
        ResourceId::local(n)
    }

    /// Group 1: pane 1 beside a slot of 2, 3 (default) and 4.
    fn stored() -> WireDoc {
        let mut slot = entry(3);
        slot.tabs = vec![entry(2), entry(4)];
        slot.shown_at = 1;
        WireDoc {
            groups: vec![WireGroup::new(
                [1; 16],
                Some("1".to_owned()),
                WireNode::Split {
                    dir: SplitDirection::Horizontal,
                    ratio: 0.5,
                    first: Box::new(WireNode::Leaf(entry(1))),
                    second: Box::new(WireNode::Leaf(slot)),
                },
            )],
        }
    }

    fn map() -> HandleMap {
        HandleMap::from_pairs((1..=5).map(|n| (key(n), Some(id(u32::from(n))))))
    }

    fn read() -> (ArrangementDoc, crate::layout::wire::EntryBinding) {
        from_protocol(&stored(), &map())
    }

    fn shown(doc: &ArrangementDoc) -> Vec<ResourceId> {
        leaves(doc.groups[0].state.tree.as_ref().unwrap())
    }

    #[test]
    fn a_stored_slot_reads_as_its_default_and_writes_back_unchanged() {
        let (doc, binding) = read();
        assert_eq!(shown(&doc), [id(1), id(3)]);
        assert_eq!(doc.slots[0].strip, [id(2), id(3), id(4)]);
        assert_eq!(to_protocol(&doc, &map(), &binding).unwrap(), stored());
    }

    /// Which tab shows is the client's own: the stored default is written.
    #[test]
    fn switching_tabs_shows_another_and_writes_the_default() {
        let (mut doc, binding) = read();
        assert_eq!(doc.cycle_tab(&id(3), 1), Some(id(4)));
        assert_eq!(shown(&doc), [id(1), id(4)]);
        assert_eq!(doc.cycle_tab(&id(4), 1), Some(id(2)), "wraps");
        assert_eq!(to_protocol(&doc, &map(), &binding).unwrap(), stored());
        assert_eq!(doc.step_default(&id(2), 1), Some(id(4)));
        let written = to_protocol(&doc, &map(), &binding).unwrap();
        assert!(matches!(
            &written.groups[0].tree,
            WireNode::Split { second, .. }
                if matches!(second.as_ref(), WireNode::Leaf(e) if e.entry == entry(4).entry)
        ));
    }

    /// Closing the tab that shows brings up the next; at one tab the slot
    /// is a plain leaf again, and the arrangement never loses the slot.
    #[test]
    fn closing_a_tab_keeps_its_slot() {
        let (mut doc, _) = read();
        doc.close_pane(&id(3)).unwrap();
        assert_eq!(shown(&doc), [id(1), id(4)]);
        assert_eq!(doc.slots[0].strip, [id(2), id(4)]);
        doc.close_pane(&id(2)).unwrap();
        assert_eq!(shown(&doc), [id(1), id(4)]);
        assert!(doc.slots.is_empty());
    }

    /// A re-read from the server shows each stored default; the tab this
    /// client chose is shown again over it.
    #[test]
    fn the_tab_a_client_shows_survives_a_re_read() {
        let (mut local, _) = read();
        local.cycle_tab(&id(3), 1);
        let (mut fresh, _) = read();
        assert_eq!(shown(&fresh), [id(1), id(3)]);
        fresh.keep_shown(&local);
        assert_eq!(shown(&fresh), [id(1), id(4)]);
    }

    #[test]
    fn a_pane_becomes_a_tab_and_comes_back_out_beside_its_slot() {
        let (mut doc, _) = read();
        doc.add_tab(&id(1), &id(3)).unwrap();
        assert_eq!(shown(&doc), [id(3)]);
        assert_eq!(doc.slots[0].strip, [id(2), id(3), id(4), id(1)]);
        doc.take_tab(&id(2), SplitDir::Horizontal, 0.5).unwrap();
        assert_eq!(shown(&doc), [id(3), id(2)]);
        assert_eq!(doc.slots[0].strip, [id(3), id(4), id(1)]);
        assert_eq!(doc.group_of(&id(1)), Some(0), "a hidden tab is arranged");
    }
}
