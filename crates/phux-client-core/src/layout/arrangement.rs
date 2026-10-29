//! [`ArrangementDoc`]: a session's groups and its active index.

use super::{
    Cow, Group, LayoutError, LayoutState, ResourceId, kill_pane, leaves, repair_focus,
    shifted_index,
};

/// A session's arrangement as a client edits it: the groups the TUI
/// presents (one per tab), plus the (per-client) active index.
///
/// Invariant: when `groups` is non-empty, `active < groups.len()`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArrangementDoc {
    /// The groups, in display order. May be empty before the first
    /// pane is seeded (the single-pane fallback renders nothing).
    pub groups: Vec<Group>,
    /// Index of the active group into [`Self::groups`].
    pub active: usize,
}

impl ArrangementDoc {
    /// An empty arrangement — no groups, matching [`LayoutState::default`]'s
    /// "no panes yet" sentinel.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An arrangement with a single group named `"1"` holding one pane.
    #[must_use]
    pub fn single(pane: ResourceId) -> Self {
        Self {
            groups: vec![Group::new("1".to_owned(), LayoutState::single(pane))],
            active: 0,
        }
    }

    /// The active group, or `None` when the arrangement is empty.
    #[must_use]
    pub fn active_group(&self) -> Option<&LayoutState> {
        self.groups.get(self.active).map(|g| &g.state)
    }

    /// Mutable access to the active group's layout, or `None` when the
    /// arrangement is empty.
    pub fn active_group_mut(&mut self) -> Option<&mut LayoutState> {
        self.groups.get_mut(self.active).map(|g| &mut g.state)
    }

    /// The layout to render and reflow against: a single-leaf layout when
    /// `zoomed` is a live leaf of the active group, else the active group
    /// (so a stale zoom heals itself). Mutation and input routing use
    /// [`Self::active_group`].
    #[must_use]
    pub fn render_group(&self, zoomed: Option<&ResourceId>) -> Option<Cow<'_, LayoutState>> {
        let active = self.active_group()?;
        if let Some(id) = zoomed
            && active.tree.as_ref().is_some_and(|t| leaves(t).contains(id))
        {
            return Some(Cow::Owned(LayoutState::single(id.clone())));
        }
        Some(Cow::Borrowed(active))
    }

    /// Append a new group named `name` holding a single `seed` pane and
    /// make it active.
    pub fn add_group(&mut self, name: String, seed: ResourceId) {
        let id = super::identity::unused_id(&self.groups);
        self.groups
            .push(Group::with_id(id, name, LayoutState::single(seed)));
        self.active = self.groups.len() - 1;
    }

    /// Drop groups whose tree became empty, keeping `active` on the same
    /// group or its successor. Returns `true` if anything was removed.
    pub fn prune_empty_groups(&mut self) -> bool {
        if self.groups.iter().all(|g| g.state.tree.is_some()) {
            return false;
        }
        let active_survives = self
            .groups
            .get(self.active)
            .is_some_and(|g| g.state.tree.is_some());
        // The active group's new index is the count of survivors that
        // precede it; if it died, that index is where the next survivor
        // lands.
        let survivors_before = self.groups[..self.active.min(self.groups.len())]
            .iter()
            .filter(|g| g.state.tree.is_some())
            .count();
        self.groups.retain(|g| g.state.tree.is_some());
        self.active = if self.groups.is_empty() {
            0
        } else if active_survives {
            survivors_before
        } else {
            survivors_before.min(self.groups.len() - 1)
        };
        true
    }

    /// Remove `target`, refusing to close the arrangement's final pane.
    ///
    /// A one-pane group is pruned after the leaf is removed. Focus stays
    /// on a surviving leaf of the affected group when it is still live,
    /// otherwise it moves to the first remaining leaf. `active` follows
    /// that group, or its post-prune successor.
    ///
    /// # Errors
    /// * [`LayoutError::LastPane`] if `target` is the only remaining pane.
    /// * [`LayoutError::PaneNotInLayout`] if `target` is not in any group.
    pub fn close_pane(&mut self, target: &ResourceId) -> Result<(), LayoutError> {
        if pane_count(self) == 1 {
            return Err(LayoutError::LastPane);
        }
        let index = self
            .groups
            .iter()
            .position(|group| {
                group
                    .state
                    .tree
                    .as_ref()
                    .is_some_and(|tree| leaves(tree).contains(target))
            })
            .ok_or_else(|| LayoutError::PaneNotInLayout(target.clone()))?;
        let tree = self.groups[index]
            .state
            .tree
            .as_ref()
            .ok_or_else(|| LayoutError::PaneNotInLayout(target.clone()))?;
        self.groups[index].state.tree = kill_pane(tree, target)?;
        repair_focus(&mut self.groups[index].state);
        self.active = index;
        self.prune_empty_groups();
        Ok(())
    }

    /// Switch focus to the next group (wraps).
    pub const fn next(&mut self) {
        if !self.groups.is_empty() {
            self.active = (self.active + 1) % self.groups.len();
        }
    }

    /// Switch focus to the previous group (wraps).
    pub const fn prev(&mut self) {
        if !self.groups.is_empty() {
            self.active = (self.active + self.groups.len() - 1) % self.groups.len();
        }
    }

    /// Select the group at `idx`. Returns `false` (no-op) if out of range.
    pub const fn select(&mut self, idx: usize) -> bool {
        if idx < self.groups.len() {
            self.active = idx;
            true
        } else {
            false
        }
    }

    /// Move the group at `from` to position `to`; `active` follows its
    /// group. Returns `false` when out of range or `from == to`.
    pub fn move_group(&mut self, from: usize, to: usize) -> bool {
        let len = self.groups.len();
        if from >= len || to >= len || from == to {
            return false;
        }
        let group = self.groups.remove(from);
        self.groups.insert(to, group);
        self.active = shifted_index(self.active, from, to);
        true
    }

    /// Rename the active group. No-op when the arrangement is empty.
    pub fn rename_active(&mut self, name: String) {
        if let Some(g) = self.groups.get_mut(self.active) {
            g.name = name;
        }
    }

    /// The lowest unused positive-integer name (`"1"`, `"2"`, …), used
    /// when a new group is created without an explicit name.
    #[must_use]
    pub fn default_group_name(&self) -> String {
        let used: std::collections::HashSet<u32> = self
            .groups
            .iter()
            .filter_map(|g| g.name.parse::<u32>().ok())
            .collect();
        (1u32..=u32::MAX)
            .find(|n| !used.contains(n))
            .unwrap_or(1)
            .to_string()
    }
}

fn pane_count(arrangement: &ArrangementDoc) -> usize {
    arrangement
        .groups
        .iter()
        .filter_map(|group| group.state.tree.as_ref())
        .map(|tree| leaves(tree).len())
        .sum()
}
