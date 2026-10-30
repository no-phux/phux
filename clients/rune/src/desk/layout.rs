//! Splits: the selected session's arrangement, the server-stored split tree
//! the terminal UI uses too, and the views of every pane it shows.
//!
//! Focus and zoom stay in this window, as the arrangement contract says.
//! A pane the arrangement does not place (a spawn nobody arranged) shows on
//! its own, like a zoom. Splitting spawns a terminal and writes the new
//! tree back; another client sees it on its next read.

use phux_client_core::layout::{
    ArrangementDoc, Direction, LayoutNode, SplitDir, focus_direction, kill_pane, leaves, split_at,
};
use phux_client_runtime::Client;
use phux_client_runtime::control::SpawnRequest;
use phux_protocol::ids::{ResourceId, SessionKey};

use super::{Desk, Placement};

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
            .topology
            .as_ref()?
            .sessions
            .iter()
            .find(|s| s.id == selected)?;
        let key = session.key?;
        if let Some((_, _, mut doc, _)) = client.organization().arrangement_document(&key) {
            let shown = self.shown.as_ref().map(|s| &s.terminal);
            super::tabs::focus_group(&mut doc, shown);
            return Some((key, doc));
        }
        if let Some(arrangement) = session.arrangement
            && self.fetched.insert(arrangement)
        {
            let _ = client.fetch_arrangement(arrangement);
        }
        None
    }

    /// The keyboard moved to a terminal the layout doesn't hold (another tab).
    pub(super) fn layout_is_stale(&self) -> bool {
        match (&self.layout, &self.shown) {
            (Some(tree), Some(shown)) => !leaves(tree).contains(&shown.terminal),
            _ => false,
        }
    }

    /// Recompute what the terminal area shows: the arrangement's active
    /// group when it holds the focused pane, else the focused pane alone.
    pub(super) fn refresh_layout(&mut self, client: &Client) -> bool {
        let Some(shown) = self.shown.as_ref().map(|s| s.terminal.clone()) else {
            return self.layout.take().is_some();
        };
        self.place_pending_splits(client);
        let listed: Vec<ResourceId> = self
            .topology
            .iter()
            .flat_map(|t| t.panes.iter().map(|p| p.terminal_id.clone()))
            .collect();
        let tree = self
            .arrangement(client)
            .and_then(|(_, doc)| doc.active_group().and_then(|g| g.tree.clone()))
            // A leaf whose terminal is gone draws nothing; the server owns
            // the stored tree, so it is pruned here, never written.
            .and_then(|tree| prune(tree, &listed, &shown))
            .filter(|tree| leaves(tree).contains(&shown));
        self.sync_tabs(client, &listed);
        let tree = match (&self.zoomed, tree) {
            (Some(zoomed), Some(tree)) if leaves(&tree).contains(zoomed) => {
                LayoutNode::Leaf(zoomed.clone())
            }
            (_, Some(tree)) => tree,
            (_, None) => LayoutNode::Leaf(shown),
        };
        let tree = match &self.dragging {
            Some((path, ratio)) => with_ratio(&tree, path, *ratio).unwrap_or(tree),
            None => tree,
        };
        let changed = self.layout.as_ref() != Some(&tree);
        // Everything on screen counts as seen, not only the keyboard's pane.
        for id in leaves(&tree) {
            self.activities.seen(&id);
        }
        self.layout = Some(tree);
        changed | self.sync_views(client)
    }

    /// Open a view for every pane on screen and close the ones that left.
    fn sync_views(&mut self, client: &Client) -> bool {
        let wanted: Vec<ResourceId> = self.layout.as_ref().map(leaves).unwrap_or_default();
        let shown = self.shown.as_ref().map(|s| s.terminal.clone());
        let mut changed = false;
        self.others.retain(|id, view| {
            let keep = wanted.contains(id) && Some(id) != shown.as_ref();
            if !keep {
                let _ = client.destroy_view(*view);
                changed = true;
            }
            keep
        });
        for id in wanted {
            if Some(&id) == shown.as_ref() || self.others.contains_key(&id) {
                continue;
            }
            // Not streaming yet: the next drain tries again.
            if let Ok(view) = client.create_view(&id) {
                self.others.insert(id, view);
                changed = true;
            }
        }
        changed
    }

    /// Move the keyboard to a pane already on screen, keeping its view.
    pub fn focus_pane(&mut self, id: &ResourceId) {
        if self.shown.as_ref().is_some_and(|s| &s.terminal == id) {
            return;
        }
        let Some(view) = self.others.remove(id) else {
            return;
        };
        if let Some(old) = self.shown.take() {
            self.others.insert(old.terminal, old.view);
        }
        self.activities.seen(id);
        self.shown = Some(super::Shown {
            terminal: id.clone(),
            view,
        });
        self.painted.replace(None);
        self.preedit = None;
        self.generation += 1;
    }

    /// Focus the pane beside the focused one.
    pub fn focus_toward(&mut self, direction: Direction) {
        let (Some(tree), Some(shown)) = (&self.layout, &self.shown) else {
            return;
        };
        if let Some(next) = focus_direction(tree, &shown.terminal, direction) {
            self.focus_pane(&next);
        }
    }

    /// Show the focused pane alone, or the whole group again.
    pub fn toggle_zoom(&mut self) {
        self.zoomed = match (&self.zoomed, &self.shown) {
            (Some(_), _) | (None, None) => None,
            (None, Some(shown)) => Some(shown.terminal.clone()),
        };
        if let Some(client) = self.client.clone() {
            self.refresh_layout(&client);
        }
    }

    /// Split the focused pane: spawn a terminal beside it, placed when the
    /// server answers.
    pub fn split(&mut self, dir: SplitDir) {
        let (Some(client), Some(shown)) = (&self.client, &self.shown) else {
            return;
        };
        let request = client.spawn_terminal(SpawnRequest {
            command: None,
            cwd: None,
            env: None,
            session_id: client.selected_session(),
            initial_size: None,
        });
        self.pending_splits
            .insert(request, Some((shown.terminal.clone(), dir)));
        self.zoomed = None;
    }

    /// Queue a spawned terminal for placement.
    pub(super) fn place_split(&mut self, client: &Client, new: &ResourceId, place: Placement) {
        self.unplaced_splits
            .push((new.clone(), place, std::time::Instant::now()));
        self.place_pending_splits(client);
    }

    /// Place every queued split whose session arrangement the replica now
    /// holds; the keyboard stays where it is until then. One that waits too
    /// long shows on its own with a notice.
    fn place_pending_splits(&mut self, client: &Client) {
        const PATIENCE: std::time::Duration = std::time::Duration::from_secs(3);
        let pending = std::mem::take(&mut self.unplaced_splits);
        for (new, place, since) in pending {
            if self.arrangement(client).is_some() {
                match &place {
                    Some((target, dir)) => self.write_split(client, target, &new, *dir),
                    None => self.write_tab(client, &new),
                }
            } else if since.elapsed() < PATIENCE {
                self.unplaced_splits.push((new, place, since));
            } else {
                self.notice =
                    Some("The layout could not be saved; it shows on its own.".to_owned());
                self.want(new);
            }
        }
    }

    fn write_split(
        &mut self,
        client: &Client,
        target: &ResourceId,
        new: &ResourceId,
        dir: SplitDir,
    ) {
        if let Some((session, mut doc)) = self.arrangement(client)
            && let Some(group) = doc.active_group_mut()
        {
            let tree = group
                .tree
                .clone()
                .unwrap_or_else(|| LayoutNode::Leaf(target.clone()));
            let base = if leaves(&tree).contains(target) {
                tree
            } else {
                LayoutNode::Leaf(target.clone())
            };
            if let Ok(split) = split_at(&base, target, new, dir, 0.5) {
                group.tree = Some(split);
                group.focus = Some(new.clone());
                self.write_arrangement(client, session, &doc, "Split not saved");
            }
        }
        self.want(new.clone());
    }

    /// A split's container moved or resized.
    pub fn split_laid_out(&mut self, path: SplitPath, size: [f32; 2]) {
        self.split_sizes.insert(path, size);
    }

    /// The length a divider at `path` travels along.
    pub fn split_span(&self, path: &SplitPath, horizontal: bool) -> f32 {
        self.split_sizes
            .get(path)
            // The divider's own width is not either side's.
            .map_or(0.0, |size| (if horizontal { size[0] } else { size[1] }) - crate::ui::DIVIDER)
    }

    /// A divider drag: preview the ratio locally, and write it on release.
    pub fn drag_divider(&mut self, path: SplitPath, ratio: f32, done: bool) {
        let ratio = ratio.clamp(MIN_RATIO, 1.0 - MIN_RATIO);
        let Some(client) = self.client.clone() else {
            return;
        };
        if !done {
            self.dragging = Some((path, ratio));
            self.refresh_layout(&client);
            return;
        }
        self.dragging = None;
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

/// `tree` with the split at `path` given `ratio`; `None` when `path` does
/// not end at a split.
pub fn with_ratio(tree: &LayoutNode, path: &[bool], ratio: f32) -> Option<LayoutNode> {
    let LayoutNode::Split {
        dir,
        ratio: old,
        left,
        right,
    } = tree
    else {
        return None;
    };
    Some(match path.split_first() {
        None => LayoutNode::Split {
            dir: *dir,
            ratio,
            left: left.clone(),
            right: right.clone(),
        },
        Some((false, rest)) => LayoutNode::Split {
            dir: *dir,
            ratio: *old,
            left: Box::new(with_ratio(left, rest, ratio)?),
            right: right.clone(),
        },
        Some((true, rest)) => LayoutNode::Split {
            dir: *dir,
            ratio: *old,
            left: left.clone(),
            right: Box::new(with_ratio(right, rest, ratio)?),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
