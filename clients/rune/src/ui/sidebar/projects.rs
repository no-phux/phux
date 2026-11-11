//! Project groups in the session list (ADR-0168): a project's name over its
//! sessions, indented, and a linked worktree's name after a `└`. Grouping
//! itself is `project_groups`' job; this paints what it was handed.

use phux_client_runtime::control::SessionDescriptor;
use phux_client_runtime::project_groups::Block;
use rune::{El, col, el, memo, row, text};

use super::super::{CAPTION, Ink, LABEL};
use super::waiting_badge;
use crate::palette::project_blocks;

/// The `└` a linked worktree's name follows.
pub(super) fn branch_mark(ink: &Ink) -> El {
    text("└").font(CAPTION).color(ink.dim)
}

/// A project's name over its sessions, and what needs you across them.
fn project_header(name: &str, waiting: usize, ink: &Ink) -> El {
    row()
        .w_full()
        .pt(12.0)
        .pb(2.0)
        .px(8.0)
        .gap(6.0)
        .items_center()
        .child(
            text(name.to_owned())
                .font(LABEL)
                .color(ink.text.alpha(0.82))
                .truncate(),
        )
        .child(el().grow(1.0))
        .children((waiting > 0).then(|| waiting_badge(waiting, ink)))
}

/// `sessions`, already in the stored order, onto `list` in project order:
/// each project's header, then its sessions indented; every other session
/// after. `rows` draws one session's header and rows, told whether it is a
/// linked worktree.
pub(super) fn grouped(
    mut list: El,
    sessions: &[&SessionDescriptor],
    waiting: impl Fn(u32) -> usize,
    theme: Option<&str>,
    ink: &Ink,
    rows: impl Fn(&SessionDescriptor, bool) -> Vec<El>,
) -> El {
    for block in project_blocks(sessions, waiting) {
        list = match block {
            Block::Single(index) => list.children(rows(sessions[index], false)),
            Block::Project(project) => list
                .child(memo(
                    format!("project-{}", project.key),
                    (&project.name, project.needs_you, theme),
                    || project_header(&project.name, project.needs_you, ink),
                ))
                .child(
                    col().w_full().gap(1.0).pl(10.0).children(
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
