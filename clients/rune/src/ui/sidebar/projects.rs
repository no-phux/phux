//! Project groups in the session list (ADR-0168): a project's name over its
//! sessions, without a duplicate matching header, and a linked worktree's
//! name after a branch glyph. Grouping
//! itself is `project_groups`' job; this paints what it was handed.

use phux_client_runtime::control::SessionDescriptor;
use phux_client_runtime::project_groups::Block;
use rune::{El, col, el, memo, row, text};

use super::super::{Paint, Tokens, icons};
use super::{Counts, histogram};
use crate::palette::project_blocks;

/// A linked worktree's branch, drawn in the same icon language as its controls.
pub(super) fn branch_mark(t: &Tokens) -> El {
    rune::icon(&icons::BRANCH, t.icon.size.sm)
        .color(t.roles.text.dim.paint())
        .a11y_hidden()
}

/// A project's name over its sessions, and what needs you across them.
fn project_header(name: &str, waiting: usize, t: &Tokens) -> El {
    let s = &t.space;
    row()
        .w_full()
        .pt(s.s3)
        .pb(s.s0_5)
        .px(s.s2)
        .gap(s.icon_gap)
        .items_center()
        .role("heading")
        .child(
            text(name.to_owned())
                .font(t.ty.label)
                .color(t.roles.text.dim.paint())
                .truncate(),
        )
        .child(el().grow(1.0))
        .child(histogram(
            Counts {
                needs_you: waiting,
                ..Counts::default()
            },
            t,
        ))
}

/// `sessions`, already in the stored order, onto `list` in project order:
/// each project's non-duplicate header, then its sessions; every other session
/// after. `rows` draws one session's header and rows, told whether it is a
/// linked worktree.
pub(super) fn grouped(
    mut list: El,
    sessions: &[&SessionDescriptor],
    waiting: impl Fn(u32) -> usize,
    version: u64,
    t: &Tokens,
    rows: impl Fn(&SessionDescriptor, bool) -> Vec<El>,
) -> El {
    for block in project_blocks(sessions, waiting) {
        list = match block {
            Block::Single(index) => list.children(rows(sessions[index], false)),
            Block::Project(project) => list
                .children(
                    (!project
                        .members
                        .iter()
                        .any(|m| sessions[m.index].name == project.name))
                    .then(|| {
                        memo(
                            format!("project-{}", project.key),
                            (&project.name, project.needs_you, version),
                            || project_header(&project.name, project.needs_you, t),
                        )
                    }),
                )
                .child(
                    col().w_full().gap(t.space.s0_5).children(
                        project
                            .members
                            .iter()
                            .flat_map(|m| rows(sessions[m.index], m.linked)),
                    ),
                ),
        };
    }
    list
}
