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

use super::Desk;

/// The steps from the root to a split: `false` left/top, `true` right/bottom.
pub type SplitPath = Vec<bool>;

/// The smallest share a divider drag leaves either side.
const MIN_RATIO: f32 = 0.1;

impl Desk {
    /// The selected session's key, and its arrangement when the replica
    /// holds it (fetching it once when it does not).
    fn arrangement(&mut self, client: &Client) -> Option<(SessionKey, ArrangementDoc)> {
        let selected = client.selected_session()?;
        let session = self
            .topology
            .as_ref()?
            .sessions
            .iter()
            .find(|s| s.id == selected)?;
        let key = session.key?;
        if let Some((_, _, doc, _)) = client.organization().arrangement_document(&key) {
            return Some((key, doc));
        }
        if let Some(arrangement) = session.arrangement
            && self.fetched.insert(arrangement)
        {
            let _ = client.fetch_arrangement(arrangement);
        }
        None
    }

    /// Recompute what the terminal area shows: the arrangement's active
    /// group when it holds the focused pane, else the focused pane alone.
    pub(super) fn refresh_layout(&mut self, client: &Client) -> bool {
        let Some(shown) = self.shown.as_ref().map(|s| s.terminal.clone()) else {
            return self.layout.take().is_some();
        };
        let tree = self
            .arrangement(client)
            .and_then(|(_, doc)| doc.active_group().and_then(|g| g.tree.clone()))
            .filter(|tree| leaves(tree).contains(&shown));
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
            .insert(request, (shown.terminal.clone(), dir));
        self.zoomed = None;
    }

    /// Place a spawned terminal beside its split's target and write the
    /// tree back. Without an arrangement it still shows, unplaced.
    pub(super) fn place_split(
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
                if let Err(error) = client.replace_arrangement(session, &doc) {
                    self.notice = Some(format!("Split not saved: {error}"));
                }
            }
        }
        self.want(new.clone());
    }

    /// Drop a closed pane from the arrangement, as the TUI does.
    pub(super) fn unplace(&mut self, client: &Client, id: &ResourceId) {
        let Some((session, mut doc)) = self.arrangement(client) else {
            return;
        };
        let Some(group) = doc.active_group_mut() else {
            return;
        };
        let Some(tree) = group.tree.clone() else {
            return;
        };
        if let Ok(rest) = kill_pane(&tree, id) {
            group.tree = rest;
            let _ = client.replace_arrangement(session, &doc);
        }
    }

    /// A split's container moved or resized.
    pub fn split_laid_out(&mut self, path: SplitPath, size: [f32; 2]) {
        self.split_sizes.insert(path, size);
    }

    /// The length a divider at `path` travels along.
    pub fn split_span(&self, path: &SplitPath, horizontal: bool) -> f32 {
        self.split_sizes
            .get(path)
            .map_or(0.0, |size| if horizontal { size[0] } else { size[1] })
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
            let _ = client.replace_arrangement(session, &doc);
        }
        self.refresh_layout(&client);
    }
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
}
