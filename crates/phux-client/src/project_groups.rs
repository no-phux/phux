//! Grouping session rows by project identity (ADR-0168): one pure rule for
//! the TUI sidebar, the session picker, and `phux ls`.
//!
//! An explicit project label (`phux project link`) outranks the repository
//! identity, and a label equal to an identity's display name joins that
//! identity's group, so a linked non-git session sits beside the git
//! sessions of the same project, on any host.
//!
//! Grouping is membership only. Blocks keep the first-appearance order of
//! their input, so a caller that orders rows stably (the sidebar) stays
//! stable and one that orders by what needs you (the picker) keeps that.

use std::collections::HashMap;

use phux_core::project_identity::SessionProject;

/// One input row as grouping sees it.
#[derive(Debug, Clone, Copy, Default)]
pub struct GroupRow<'a> {
    /// The row's structured identity, when it has one.
    pub project: Option<&'a SessionProject>,
    /// An explicit project label: the session's `project` attribute, else a
    /// legacy `phux.session.project/v1` tag. It outranks [`Self::project`].
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
    /// The grouping key: the identity's `id` (also for a label naming that
    /// identity), or `label:<text>`.
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

fn label<'a>(row: &GroupRow<'a>) -> Option<&'a str> {
    row.label.filter(|label| !label.trim().is_empty())
}

/// Each row's grouping key. A label outranks the row's own identity; a label
/// equal to the display name of an unlabeled row's identity takes that
/// identity's key (the first in input order, if two share a name).
fn group_keys(rows: &[GroupRow<'_>]) -> Vec<Option<String>> {
    let mut identity_by_name: HashMap<&str, &str> = HashMap::new();
    for row in rows.iter().filter(|row| label(row).is_none()) {
        if let Some(project) = row.project {
            identity_by_name
                .entry(project.name.as_str())
                .or_insert(project.id.as_str());
        }
    }
    let key_of = |row: &GroupRow<'_>| {
        let Some(label) = label(row) else {
            return row.project.map(|project| project.id.clone());
        };
        Some(
            identity_by_name
                .get(label)
                .map_or_else(|| format!("label:{label}"), |id| (*id).to_owned()),
        )
    };
    rows.iter().map(key_of).collect()
}

/// Group `rows` by project. A project with fewer than `min_members` rows
/// stays as [`Block::Single`] rows in place.
#[must_use]
pub fn group_rows(rows: &[GroupRow<'_>], min_members: usize) -> Vec<Block> {
    let keys = group_keys(rows);
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
    // A row's identity places it only in its own identity's group.
    let identity = |j: usize| rows[j].project.filter(|p| p.id == key);
    let linked = |j: usize| identity(j).is_some_and(|p| p.linked_worktree);
    let host = |j: usize| identity(j).and_then(|p| p.host.as_deref());
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
        identity(j).map_or_else(
            || label(&rows[j]).unwrap_or_default().to_owned(),
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

    fn labelled<'a>(project: Option<&'a SessionProject>, label: &'a str) -> GroupRow<'a> {
        GroupRow {
            project,
            label: Some(label),
            needs_you: 0,
        }
    }

    #[test]
    fn a_label_naming_an_identity_joins_its_group_across_hosts() {
        let mini = project("gh/a/phux", "phux", false, "mini");
        let laptop_wt = project("gh/a/phux", "phux", true, "laptop");
        let rows = [
            labelled(None, "phux"),
            row(Some(&mini), 0),
            row(None, 0),
            row(Some(&laptop_wt), 0),
        ];
        let blocks = group_rows(&rows, 2);
        let Block::Project(phux) = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(phux.key, "gh/a/phux", "the identity's key, not label:phux");
        assert_eq!(phux.name, "phux");
        let order: Vec<(usize, bool)> = phux.members.iter().map(|m| (m.index, m.linked)).collect();
        assert_eq!(order, [(0, false), (1, false), (3, true)]);
        assert_eq!(blocks[1], Block::Single(2));
    }

    #[test]
    fn an_explicit_label_outranks_the_repository_identity() {
        let dotfiles = project("gh/a/dotfiles", "dotfiles", true, "mini");
        let phux = project("gh/a/phux", "phux", false, "mini");
        let rows = [
            labelled(Some(&dotfiles), "phux"),
            row(Some(&dotfiles), 0),
            row(Some(&phux), 0),
            labelled(Some(&dotfiles), "infra"),
            labelled(None, "infra"),
        ];
        let blocks = group_rows(&rows, 2);
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        let Block::Project(linked) = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(linked.key, "gh/a/phux");
        let order: Vec<(usize, bool)> =
            linked.members.iter().map(|m| (m.index, m.linked)).collect();
        assert_eq!(
            order,
            [(0, false), (2, false)],
            "a relabelled worktree is no longer drawn as a linked one"
        );
        assert_eq!(blocks[1], Block::Single(1));
        let Block::Project(infra) = &blocks[2] else {
            panic!("{blocks:?}");
        };
        assert_eq!(
            (infra.key.as_str(), infra.name.as_str()),
            ("label:infra", "infra")
        );
        assert_eq!(infra.members.len(), 2);
    }

    #[test]
    fn a_blank_label_falls_back_to_the_identity() {
        let phux = project("gh/a/phux", "phux", false, "mini");
        let rows = [labelled(Some(&phux), " "), row(Some(&phux), 0)];
        let Block::Project(block) = &group_rows(&rows, 2)[0] else {
            panic!("grouped");
        };
        assert_eq!(block.key, "gh/a/phux");
        assert_eq!(block.members.len(), 2);
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
