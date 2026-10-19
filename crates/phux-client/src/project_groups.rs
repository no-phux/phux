//! Grouping session rows by project identity (ADR-0168): one pure rule for
//! the TUI sidebar, the session picker, and `phux ls`.
//!
//! An explicit project link (`phux project link`) outranks the repository
//! identity, and a link equal to an identity's display name joins that
//! identity's group, so a linked non-git session sits beside the git
//! sessions of the same project, on any host. The free-text label is only
//! a fallback for a row with neither.
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
    /// The session's explicit project link (`project.link`,
    /// `phux project link`). It outranks [`Self::project`].
    pub link: Option<&'a str>,
    /// A free-text `phux.session.project/v1` label, used only when the row
    /// has neither a link nor an identity.
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
    /// The grouping key: the identity's `id` (also for a link naming that
    /// identity), or `label:<text>` for a link or label naming none.
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

fn nonblank(text: Option<&str>) -> Option<&str> {
    text.filter(|text| !text.trim().is_empty())
}

fn link<'a>(row: &GroupRow<'a>) -> Option<&'a str> {
    nonblank(row.link)
}

/// The identity a row groups by: its own, unless a link names another
/// project. A link equal to the identity's own name keeps it.
fn kept_identity<'a>(row: &GroupRow<'a>) -> Option<&'a SessionProject> {
    row.project
        .filter(|project| link(row).is_none_or(|link| link == project.name))
}

/// Each row's grouping key. A kept identity keys by its `id`. A link that
/// overrides it takes the `id` of a kept identity with that display name
/// (the first in input order, if two share a name), else `label:<link>`.
/// A row with neither keys by its label, as before links existed.
fn group_keys(rows: &[GroupRow<'_>]) -> Vec<Option<String>> {
    let mut identity_by_name: HashMap<&str, &str> = HashMap::new();
    for project in rows.iter().filter_map(kept_identity) {
        identity_by_name
            .entry(project.name.as_str())
            .or_insert(project.id.as_str());
    }
    let key_of = |row: &GroupRow<'_>| {
        if let Some(project) = kept_identity(row) {
            return Some(project.id.clone());
        }
        if let Some(link) = link(row) {
            return Some(
                identity_by_name
                    .get(link)
                    .map_or_else(|| format!("label:{link}"), |id| (*id).to_owned()),
            );
        }
        nonblank(row.label).map(|label| format!("label:{label}"))
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
            || {
                link(&rows[j])
                    .or_else(|| nonblank(rows[j].label))
                    .unwrap_or_default()
                    .to_owned()
            },
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
            needs_you,
            ..GroupRow::default()
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

    fn linked<'a>(project: Option<&'a SessionProject>, link: &'a str) -> GroupRow<'a> {
        GroupRow {
            project,
            link: Some(link),
            ..GroupRow::default()
        }
    }

    fn members(block: &Block) -> Vec<(usize, bool)> {
        let Block::Project(block) = block else {
            panic!("not a project block: {block:?}");
        };
        block.members.iter().map(|m| (m.index, m.linked)).collect()
    }

    /// With no link anywhere, a free-text label on a git row changes
    /// nothing: the identity groups, worktree markers and keys included.
    #[test]
    fn unlinked_rows_group_exactly_by_identity() {
        let main = project("gh/a/phux", "phux", false, "mini");
        let wt = project("gh/a/phux", "phux", true, "mini");
        let plain = [row(Some(&wt), 0), row(Some(&main), 0)];
        let tagged = [
            GroupRow {
                label: Some("phux-checkout"),
                ..row(Some(&wt), 0)
            },
            GroupRow {
                label: Some("other-dir"),
                ..row(Some(&main), 0)
            },
        ];
        let expected = group_rows(&plain, 2);
        assert_eq!(group_rows(&tagged, 2), expected);
        let Block::Project(block) = &expected[0] else {
            panic!("{expected:?}");
        };
        assert_eq!(block.key, "gh/a/phux");
        assert_eq!(members(&expected[0]), [(1, false), (0, true)]);
    }

    #[test]
    fn a_link_naming_an_identity_joins_its_group_across_hosts() {
        let mini = project("gh/a/phux", "phux", false, "mini");
        let laptop_wt = project("gh/a/phux", "phux", true, "laptop");
        let rows = [
            linked(None, "phux"),
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
        assert_eq!(members(&blocks[0]), [(0, false), (1, false), (3, true)]);
        assert_eq!(blocks[1], Block::Single(2));
    }

    /// A row linked to its own repository's name keeps its identity: same
    /// key, same worktree marker, even when it is that repository's only
    /// row.
    #[test]
    fn a_link_to_its_own_repository_keeps_the_identity() {
        let main = project("gh/a/phux", "phux", false, "mini");
        let wt = project("gh/a/phux", "phux", true, "mini");
        let rows = [linked(Some(&wt), "phux"), row(Some(&main), 0)];
        let blocks = group_rows(&rows, 2);
        assert_eq!(
            blocks,
            group_rows(&[row(Some(&wt), 0), row(Some(&main), 0)], 2)
        );
        assert_eq!(members(&blocks[0]), [(1, false), (0, true)]);
        let alone = group_rows(&[linked(Some(&wt), "phux")], 1);
        let Block::Project(block) = &alone[0] else {
            panic!("{alone:?}");
        };
        assert_eq!(block.key, "gh/a/phux");
        assert_eq!(members(&alone[0]), [(0, true)]);
    }

    #[test]
    fn a_link_to_another_name_outranks_the_identity() {
        let dotfiles = project("gh/a/dotfiles", "dotfiles", true, "mini");
        let phux = project("gh/a/phux", "phux", false, "mini");
        let rows = [
            linked(Some(&dotfiles), "phux"),
            row(Some(&dotfiles), 0),
            row(Some(&phux), 0),
            linked(Some(&dotfiles), "infra"),
            linked(None, "infra"),
        ];
        let blocks = group_rows(&rows, 2);
        assert_eq!(blocks.len(), 3, "{blocks:?}");
        let Block::Project(joined) = &blocks[0] else {
            panic!("{blocks:?}");
        };
        assert_eq!(joined.key, "gh/a/phux");
        assert_eq!(
            members(&blocks[0]),
            [(0, false), (2, false)],
            "a worktree linked elsewhere is not drawn as phux's worktree"
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

    /// The free-text label never overrides an identity, even when it names
    /// another repository; a link does.
    #[test]
    fn a_label_differing_from_the_identity_does_not_override_it() {
        let dotfiles = project("gh/a/dotfiles", "dotfiles", false, "mini");
        let phux = project("gh/a/phux", "phux", false, "mini");
        let rows = [
            GroupRow {
                label: Some("phux"),
                ..row(Some(&dotfiles), 0)
            },
            row(Some(&phux), 0),
        ];
        assert_eq!(group_rows(&rows, 2), [Block::Single(0), Block::Single(1)]);
    }

    #[test]
    fn a_blank_link_falls_back_to_the_identity() {
        let phux = project("gh/a/phux", "phux", false, "mini");
        let rows = [linked(Some(&phux), " "), row(Some(&phux), 0)];
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
