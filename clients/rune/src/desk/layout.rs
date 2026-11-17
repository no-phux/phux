//! Splits: the selected session's arrangement, the server-stored split tree
//! the terminal UI uses too, and the views of every pane it shows.
//!
//! Focus and zoom stay in this window, as the arrangement contract says.
//! A pane the arrangement does not place (a spawn nobody arranged) shows on
//! its own, like a zoom. Splitting spawns a terminal and writes the new
//! tree back; another client sees it on its next read.

use phux_client_core::layout::{
    ArrangementDoc, Direction, LayoutNode, NodePath, focus_direction, kill_pane, leaves,
    set_ratio_at,
};
use phux_client_runtime::Client;
use phux_protocol::ids::{ResourceId, SessionKey};

use super::Desk;

/// The steps from the root to a split: `false` left/top, `true` right/bottom.
pub type SplitPath = Vec<bool>;

/// The smallest share a divider drag leaves either side.
const MIN_RATIO: f32 = 0.1;

impl Desk {
    /// The selected session's key, and its arrangement when the replica
    /// holds it (fetching it once when it does not).
    pub(super) fn arrangement(&mut self, client: &Client) -> Option<(SessionKey, ArrangementDoc)> {
        let selected = client.selected_session()?;
        let session = self
            .connection
            .topology
            .as_ref()?
            .sessions
            .iter()
            .find(|s| s.id == selected)?;
        let key = session.key?;
        if let Some((_, _, mut doc, _)) = client.organization().arrangement_document(&key) {
            // Edits land on what this window shows: its tabs, not the
            // stored defaults, which the write keeps.
            for tab in &self.connection.slot_choice {
                doc.show_tab(tab);
            }
            let shown = self.connection.shown.as_ref().map(|s| &s.resource);
            super::tabs::focus_group(&mut doc, shown);
            return Some((key, doc));
        }
        if let Some(arrangement) = session.arrangement
            && self.connection.fetched.insert(arrangement)
        {
            let _ = client.fetch_arrangement(arrangement);
        }
        None
    }

    /// The layout owes the screen something no event will bring: the keyboard
    /// moved to a terminal it doesn't hold (another tab), a pane has no view
    /// yet, or a spawn is waiting to be placed.
    pub(super) fn layout_is_stale(&self) -> bool {
        if self.splits_waiting() {
            return true;
        }
        match (self.navigation_tree(), &self.connection.shown) {
            (Some(tree), Some(shown)) => {
                let on_screen = leaves(tree);
                !on_screen.contains(&shown.resource)
                    || on_screen
                        .iter()
                        .any(|id| id != &shown.resource && !self.connection.others.contains_key(id))
            }
            (None, None) => false,
            _ => true,
        }
    }

    /// Recompute what the terminal area shows: the arrangement's active
    /// group when it holds the focused pane, else the focused pane alone.
    pub(super) fn refresh_layout(&mut self, client: &Client) -> bool {
        let Some(shown) = self.connection.shown.as_ref().map(|s| s.resource.clone()) else {
            let listed = self.listed_resources();
            self.sync_tabs(client, &listed);
            let changed = self.connection.layout.take().is_some();
            if let Some(phone) = self.phone.as_mut() {
                phone.reconcile(None, None);
            }
            return changed | self.sync_views(client);
        };
        self.place_pending_splits(client);
        let listed = self.listed_resources();
        let arrangement = self.arrangement(client).map(|(key, mut doc)| {
            self.choose_slot_tabs(&mut doc, &shown);
            (key, doc)
        });
        let context = arrangement
            .as_ref()
            .and_then(|(session, doc)| Some((*session, doc.groups.get(doc.active)?.id)));
        let tree = arrangement
            .and_then(|(_, doc)| doc.active_group().and_then(|g| g.tree.clone()))
            // A leaf whose terminal is gone draws nothing; the server owns
            // the stored tree, so it is pruned here, never written.
            .and_then(|tree| prune(tree, &listed, &shown))
            .filter(|tree| leaves(tree).contains(&shown));
        self.sync_tabs(client, &listed);
        // Retain the full group for the camera and its views. Activity and
        // sizing still see only the focused leaf, not everything we paint.
        let tree = if let Some(phone) = self.phone.as_mut() {
            phone.reconcile(
                tree.or_else(|| Some(LayoutNode::Leaf(shown.clone()))),
                context,
            );
            None
        } else {
            tree
        };
        let tree = match (&self.connection.zoomed, tree) {
            (Some(zoomed), Some(tree)) if leaves(&tree).contains(zoomed) => {
                LayoutNode::Leaf(zoomed.clone())
            }
            (_, Some(tree)) => tree,
            (_, None) => LayoutNode::Leaf(shown),
        };
        let tree = match &self.connection.dragging {
            Some((path, ratio)) => with_ratio(&tree, path, *ratio).unwrap_or(tree),
            None => tree,
        };
        let changed = self.connection.layout.as_ref() != Some(&tree);
        // Everything on screen counts as seen, not only the keyboard's pane.
        if self.focused {
            for id in leaves(&tree) {
                self.activities.seen(&id);
            }
        }
        self.connection.layout = Some(tree);
        changed | self.sync_views(client)
    }

    /// Open views for the active layout (the full phone group), closing the
    /// ones that left it. Retaining a view does not make its pane seen.
    fn sync_views(&mut self, client: &Client) -> bool {
        let mut wanted: Vec<ResourceId> = self.navigation_tree().map(leaves).unwrap_or_default();
        // A slot's hidden tabs keep views: attached, sized, ready to show.
        let hidden: Vec<ResourceId> = (self.connection.slots.iter())
            .filter(|slot| wanted.contains(slot.showing()))
            .flat_map(|slot| {
                slot.strip
                    .iter()
                    .filter(|tab| *tab != slot.showing())
                    .cloned()
            })
            .collect();
        wanted.extend(hidden);
        let shown = self.connection.shown.as_ref().map(|s| s.resource.clone());
        let mut changed = false;
        self.connection.others.retain(|id, view| {
            let keep = wanted.contains(id) && Some(id) != shown.as_ref();
            if !keep {
                if let Some(view) = view {
                    let _ = client.destroy_view(*view);
                }
                changed = true;
            }
            keep
        });
        for id in wanted {
            self.subscribe_document(client, &id);
            if Some(&id) == shown.as_ref() || self.connection.others.contains_key(&id) {
                continue;
            }
            // Not streaming yet: the next drain tries again.
            if self.is_document(&id) {
                self.connection.others.insert(id, None);
                changed = true;
            } else if let Some(view) = self.create_view(client, &id) {
                self.connection.others.insert(id, Some(view));
                changed = true;
            }
        }
        changed
    }

    /// Show each slot's tab this window chose, the keyboard's above all, and
    /// keep the slots for sizing and badging their tabs.
    fn choose_slot_tabs(&mut self, doc: &mut ArrangementDoc, shown: &ResourceId) {
        let choice = &mut self.connection.slot_choice;
        if let Some(slot) = doc.slot_of(shown) {
            choice.retain(|tab| !slot.strip.contains(tab));
            choice.push(shown.clone());
        }
        choice.retain(|tab| doc.slot_of(tab).is_some());
        for tab in choice.iter() {
            doc.show_tab(tab);
        }
        self.connection.slots.clone_from(&doc.slots);
    }

    /// Show the next tab of the focused pane's slot, `step` places on.
    pub fn cycle_slot_tab(&mut self, step: isize) {
        let Some(shown) = self.connection.shown.as_ref().map(|s| s.resource.clone()) else {
            return;
        };
        let Some(slot) = self
            .connection
            .slots
            .iter()
            .find(|s| s.strip.contains(&shown))
        else {
            return;
        };
        let at = slot.strip.iter().position(|tab| *tab == shown).unwrap_or(0);
        let len = isize::try_from(slot.strip.len()).unwrap_or(1);
        let next = isize::try_from(at).unwrap_or(0) + step;
        let next = slot.strip[usize::try_from(next.rem_euclid(len)).unwrap_or(0)].clone();
        self.focus_pane(&next);
    }

    /// The area a hidden slot tab takes: its slot's, wherever it shows.
    pub(super) fn slot_area(&self, terminal: &ResourceId) -> Option<[f32; 2]> {
        let slot = self
            .connection
            .slots
            .iter()
            .find(|s| s.strip.contains(terminal))?;
        self.connection.areas.get(slot.showing()).copied()
    }

    /// Move the keyboard to a pane in the active layout, keeping its view.
    pub fn focus_pane(&mut self, id: &ResourceId) {
        if self
            .connection
            .shown
            .as_ref()
            .is_some_and(|s| &s.resource == id)
        {
            self.connection.resources.returning = None;
            return;
        }
        let Some(view) = self.connection.others.remove(id) else {
            return;
        };
        self.connection.resources.returning = None;
        if let Some(old) = self.connection.shown.take() {
            self.connection.others.insert(old.resource, old.view);
        }
        self.activities.seen(id);
        self.connection.shown = Some(super::Shown {
            resource: id.clone(),
            view,
        });
        if let Some(phone) = self.phone.as_mut() {
            phone.reset_navigation();
            self.connection.layout = Some(LayoutNode::Leaf(id.clone()));
            // Never propose a retained sibling's previous offscreen size.
            self.connection.areas.remove(id);
        }
        self.connection.painted.replace(None);
        self.connection.preedit = None;
        self.generation += 1;
    }

    /// Focus the pane beside the focused one.
    pub fn focus_toward(&mut self, direction: Direction) {
        let (Some(tree), Some(shown)) = (self.navigation_tree(), &self.connection.shown) else {
            return;
        };
        let next = if self.phone.is_some() {
            super::phone::spatial::toward(tree, &shown.resource, direction)
        } else {
            focus_direction(tree, &shown.resource, direction)
        };
        if let Some(next) = next {
            self.focus_pane(&next);
        }
    }

    /// The phone retains its full group while `layout` stays a single leaf.
    pub(super) fn navigation_tree(&self) -> Option<&LayoutNode> {
        self.phone
            .as_ref()
            .and_then(|phone| phone.tree.as_ref())
            .or(self.connection.layout.as_ref())
    }

    /// Show the focused pane alone, or the whole group again.
    pub fn toggle_zoom(&mut self) {
        self.connection.zoomed = match (&self.connection.zoomed, &self.connection.shown) {
            (Some(_), _) | (None, None) => None,
            (None, Some(shown)) => Some(shown.resource.clone()),
        };
        if let Some(client) = self.connection.client.clone() {
            self.refresh_layout(&client);
        }
    }

    /// A split's container moved or resized.
    pub fn split_laid_out(&mut self, path: SplitPath, size: [f32; 2]) {
        self.connection.split_sizes.insert(path, size);
    }

    /// The whole split tree's size from its last layout, once it has one.
    pub fn split_area(&self) -> Option<[f32; 2]> {
        self.connection.split_sizes.get(&Vec::new()).copied()
    }

    /// The length a divider at `path` travels along.
    pub fn split_span(&self, path: &SplitPath, horizontal: bool) -> f32 {
        self.connection.split_sizes
            .get(path)
            // The divider's own width is not either side's.
            .map_or(0.0, |size| (if horizontal { size[0] } else { size[1] }) - crate::ui::DIVIDER)
    }

    /// A divider drag: preview the ratio locally, and write it on release.
    pub fn drag_divider(&mut self, path: SplitPath, ratio: f32, done: bool) {
        let ratio = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        if !done {
            self.connection.dragging = Some((path, ratio));
            self.refresh_layout(&client);
            return;
        }
        self.connection.dragging = None;
        if let Some((session, mut doc)) = self.arrangement(&client)
            && let Some(group) = doc.active_group_mut()
            && let Some(tree) = group
                .tree
                .as_ref()
                .and_then(|t| with_ratio(t, &path, ratio))
        {
            group.tree = Some(tree);
            self.write_arrangement(&client, session, &doc, "Divider not saved");
        }
        self.refresh_layout(&client);
        self.fit();
    }
}

/// `tree` without leaves whose terminal the server no longer lists (the
/// focused one stays). `None` when nothing is left.
fn prune(tree: LayoutNode, listed: &[ResourceId], keep: &ResourceId) -> Option<LayoutNode> {
    leaves(&tree)
        .into_iter()
        .filter(|id| id != keep && !listed.contains(id))
        .try_fold(tree, |tree, gone| kill_pane(&tree, &gone).ok().flatten())
}

/// The pane that takes the keyboard when `closed` goes: the leaf before it in
/// reading order, else the one after.
pub fn neighbour(tree: &LayoutNode, closed: &ResourceId) -> Option<ResourceId> {
    let all = leaves(tree);
    let at = all.iter().position(|id| id == closed)?;
    at.checked_sub(1)
        .and_then(|i| all.get(i))
        .or_else(|| all.get(at + 1))
        .cloned()
}

/// `tree` with the split at `path` (`true` for each right step) given
/// `ratio`; `None` when `path` does not end at a split.
pub fn with_ratio(tree: &LayoutNode, path: &[bool], ratio: f32) -> Option<LayoutNode> {
    set_ratio_at(tree, &NodePath::from_sides(path), ratio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client_core::layout::SplitDir;

    fn leaf(n: u32) -> LayoutNode {
        LayoutNode::Leaf(ResourceId::local(n))
    }

    fn split(dir: SplitDir, ratio: f32, left: LayoutNode, right: LayoutNode) -> LayoutNode {
        LayoutNode::Split {
            dir,
            ratio,
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    #[test]
    fn a_ratio_lands_on_the_addressed_split_only() {
        let tree = split(
            SplitDir::Horizontal,
            0.5,
            leaf(1),
            split(SplitDir::Vertical, 0.5, leaf(2), leaf(3)),
        );
        let root = with_ratio(&tree, &[], 0.3).expect("root split");
        assert!(matches!(root, LayoutNode::Split { ratio, .. } if (ratio - 0.3).abs() < 1e-6));
        let inner = with_ratio(&tree, &[true], 0.7).expect("inner split");
        let LayoutNode::Split { ratio, right, .. } = inner else {
            panic!("split")
        };
        assert!((ratio - 0.5).abs() < 1e-6, "the root keeps its ratio");
        assert!(matches!(*right, LayoutNode::Split { ratio, .. } if (ratio - 0.7).abs() < 1e-6));
    }

    #[test]
    fn a_path_through_a_leaf_is_refused() {
        let tree = split(SplitDir::Horizontal, 0.5, leaf(1), leaf(2));
        assert!(with_ratio(&tree, &[false], 0.4).is_none());
        assert!(with_ratio(&leaf(1), &[], 0.4).is_none());
    }

    #[test]
    fn a_closed_pane_hands_the_keyboard_to_its_reading_order_neighbour() {
        let tree = split(
            SplitDir::Horizontal,
            0.5,
            leaf(1),
            split(SplitDir::Vertical, 0.5, leaf(2), leaf(3)),
        );
        assert_eq!(
            neighbour(&tree, &ResourceId::local(3)),
            Some(ResourceId::local(2))
        );
        assert_eq!(
            neighbour(&tree, &ResourceId::local(1)),
            Some(ResourceId::local(2))
        );
        assert_eq!(neighbour(&leaf(1), &ResourceId::local(1)), None);
    }

    #[test]
    fn unlisted_leaves_are_pruned_but_the_focused_one_stays() {
        let tree = split(
            SplitDir::Horizontal,
            0.5,
            leaf(1),
            split(SplitDir::Vertical, 0.5, leaf(2), leaf(3)),
        );
        let listed = [ResourceId::local(1), ResourceId::local(3)];
        let pruned = prune(tree.clone(), &listed, &ResourceId::local(1)).expect("leaves remain");
        assert_eq!(
            leaves(&pruned),
            vec![ResourceId::local(1), ResourceId::local(3)]
        );
        let kept = prune(tree, &[], &ResourceId::local(2)).expect("focus stays");
        assert_eq!(leaves(&kept), vec![ResourceId::local(2)]);
    }
}
