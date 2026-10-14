//! Grouping session rows by project identity (ADR-0167): one pure rule for
//! the TUI sidebar, the session picker, and `phux ls`.
//!
//! Grouping is membership only. Blocks keep the first-appearance order of
//! their input, so a caller that orders rows stably (the sidebar) stays
//! stable and one that orders by what needs you (the picker) keeps that.

use phux_core::project_identity::SessionProject;

/// One input row as grouping sees it.
#[derive(Debug, Clone, Copy, Default)]
pub struct GroupRow<'a> {
    /// The row's structured identity, when it has one.
    pub project: Option<&'a SessionProject>,
    /// A free-text `phux.session.project/v1` label, used only when the row
    /// has no identity.
    pub label: Option<&'a str>,
    /// Panes in the row that need a person (blocked or asking).
    pub needs_you: usize,
}

/// A grouped row's place in its block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Member {
    /// Index into the input rows.
    pub index: usize,
    /// A linked worktree, drawn indented under the primary checkout.
    pub linked: bool,
}

/// Rows that share one project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectBlock {
    /// The grouping key: the identity's `id`, or `label:<text>`.
    pub key: String,
    /// Display name: the first primary checkout's name, else the first
    /// member's.
    pub name: String,
    /// Members: each primary checkout followed by the linked worktrees read
    /// on its host, then any remaining linked worktrees, each in input order.
    pub members: Vec<Member>,
    /// Sum of the members' [`GroupRow::needs_you`].
    pub needs_you: usize,
}

/// One step of the grouped output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A row in no group of at least `min_members`.
    Single(usize),
    /// A project group.
    Project(ProjectBlock),
}

fn group_key(row: &GroupRow<'_>) -> Option<String> {
    if let Some(project) = row.project {
        return Some(project.id.clone());
    }
    row.label
        .filter(|label| !label.is_empty())
        .map(|label| format!("label:{label}"))
}

/// Group `rows` by project. A project with fewer than `min_members` rows
/// stays as [`Block::Single`] rows in place.
#[must_use]
pub fn group_rows(rows: &[GroupRow<'_>], min_members: usize) -> Vec<Block> {
    let keys: Vec<Option<String>> = rows.iter().map(group_key).collect();
    let mut blocks = Vec::new();
    let mut emitted = vec![false; rows.len()];
    for index in 0..rows.len() {
        if emitted[index] {
            continue;
        }
        let Some(key) = &keys[index] else {
            emitted[index] = true;
            blocks.push(Block::Single(index));
            continue;
        };
        let indices: Vec<usize> = (index..rows.len())
            .filter(|&j| keys[j].as_ref() == Some(key))
            .collect();
        for &j in &indices {
            emitted[j] = true;
        }
        if indices.len() < min_members.max(1) {
            blocks.extend(indices.into_iter().map(Block::Single));
            continue;
        }
        blocks.push(Block::Project(project_block(rows, key, &indices)));
    }
    blocks
}

fn project_block(rows: &[GroupRow<'_>], key: &str, indices: &[usize]) -> ProjectBlock {
    let linked = |j: usize| rows[j].project.is_some_and(|p| p.linked_worktree);
    let host = |j: usize| rows[j].project.and_then(|p| p.host.as_deref());
    let mut members = Vec::with_capacity(indices.len());
    let mut placed = vec![false; indices.len()];
    for (slot, &primary) in indices.iter().enumerate() {
        if linked(primary) {
            continue;
        }
        placed[slot] = true;
        members.push(Member {
            index: primary,
            linked: false,
        });
        for (other, &child) in indices.iter().enumerate() {
            if !placed[other] && linked(child) && host(child) == host(primary) {
                placed[other] = true;
                members.push(Member {
                    index: child,
                    linked: true,
                });
            }
        }
    }
    for (slot, &child) in indices.iter().enumerate() {
        if !placed[slot] {
            members.push(Member {
                index: child,
                linked: true,
            });
        }
    }
    let name_of = |j: usize| {
        rows[j].project.map_or_else(
            || rows[j].label.unwrap_or_default().to_owned(),
            |p| p.name.clone(),
        )
    };
    let name = indices
        .iter()
        .copied()
        .find(|&j| !linked(j))
        .map_or_else(|| name_of(indices[0]), name_of);
    ProjectBlock {
        key: key.to_owned(),
        name,
        needs_you: indices.iter().map(|&j| rows[j].needs_you).sum(),
        members,
    }
}

impl ProjectBlock {
    /// The members a collapsed block still shows: those `keep` names
    /// (the current session, one that needs you). An expanded block shows
    /// every member.
    #[must_use]
    pub fn visible(&self, collapsed: bool, keep: impl Fn(usize) -> bool) -> Vec<Member> {
        self.members
            .iter()
            .copied()
            .filter(|member| !collapsed || keep(member.index))
            .collect()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests")]
mod tests {
    use super::*;

    fn project(id: &str, name: &str, linked: bool, host: &str) -> SessionProject {
        SessionProject {
            id: id.to_owned(),
            name: name.to_owned(),
            checkout: format!("/w/{name}"),
            branch: None,
            linked_worktree: linked,
            host: Some(host.to_owned()),
        }
    }

    fn row(project: Option<&SessionProject>, needs_you: usize) -> GroupRow<'_> {
        GroupRow {
            project,
            label: None,
            needs_you,
        }
    }

    #[test]
    fn primaries_lead_and_linked_worktrees_pack_under_their_host() {
        let feat = project("gh/a/phux", "phux", true, "mini");
        let scratch = project("gh/a/other", "other", false, "mini");
        let main = project("gh/a/phux", "phux", false, "mini");
        let laptop = project("gh/a/phux", "phux", false, "laptop");
        let laptop_wt = project("gh/a/phux", "phux", true, "laptop");
        let rows = [
            row(Some(&feat), 1),
            row(Some(&scratch), 0),
            row(None, 0),
            row(Some(&laptop_wt), 2),
            row(Some(&main), 0),
            row(Some(&laptop), 0),
        ];
        let blocks = group_rows(&rows, 2);
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        let Block::Project(phux) = &blocks[0] else {
            panic!("first block is the phux group: {blocks:?}");
        };
        assert_eq!(phux.key, "gh/a/phux");
        assert_eq!(phux.name, "phux");
        assert_eq!(phux.needs_you, 3);
        let order: Vec<(usize, bool)> = phux.members.iter().map(|m| (m.index, m.linked)).collect();
        assert_eq!(order, [(4, false), (0, true), (5, false), (3, true)]);
        assert_eq!(blocks[1], Block::Single(1));
        assert_eq!(blocks[2], Block::Single(2));
    }

    #[test]
    fn a_lone_project_stays_a_single_row_below_the_minimum() {
        let p = project("gh/a/b", "b", false, "h");
        let rows = [row(Some(&p), 0), row(None, 0)];
        assert_eq!(group_rows(&rows, 2), [Block::Single(0), Block::Single(1)]);
        let Block::Project(block) = &group_rows(&rows, 1)[0] else {
            panic!("a minimum of one groups it");
        };
        assert_eq!(block.members.len(), 1);
    }

    #[test]
    fn legacy_labels_group_rows_without_an_identity() {
        let rows = [
            GroupRow {
                label: Some("api"),
                ..GroupRow::default()
            },
            GroupRow {
                label: Some(""),
                ..GroupRow::default()
            },
            GroupRow {
                label: Some("api"),
                ..GroupRow::default()
            },
        ];
        let blocks = group_rows(&rows, 1);
        let Block::Project(api) = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(api.key, "label:api");
        assert_eq!(api.name, "api");
        assert_eq!(api.members.len(), 2);
        assert_eq!(blocks[1], Block::Single(1));
    }

    #[test]
    fn only_linked_worktrees_still_group_and_name_from_the_first() {
        let a = project("gh/x", "x", true, "h1");
        let b = project("gh/x", "x", true, "h2");
        let blocks = group_rows(&[row(Some(&a), 0), row(Some(&b), 0)], 2);
        let Block::Project(block) = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(block.name, "x");
        assert!(block.members.iter().all(|m| m.linked));
    }

    #[test]
    fn a_collapsed_block_keeps_only_the_rows_asked_for() {
        let p = project("gh/x", "x", false, "h");
        let w = project("gh/x", "x", true, "h");
        let rows = [row(Some(&p), 0), row(Some(&w), 1), row(Some(&w), 0)];
        let Block::Project(block) = &group_rows(&rows, 2)[0] else {
            panic!("grouped");
        };
        assert_eq!(block.visible(false, |_| false).len(), 3);
        let kept: Vec<usize> = block
            .visible(true, |index| rows[index].needs_you > 0)
            .iter()
            .map(|m| m.index)
            .collect();
        assert_eq!(kept, [1]);
    }
}
