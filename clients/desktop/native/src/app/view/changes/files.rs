//! The file column: conflicts, staged files, then working-tree changes, each
//! as a tree of folders or a flat list. A file row reveals its diff; its
//! counts, viewed mark and stage control sit at its end.

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, Hsla, IntoElement, SharedString, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::{Icon, Sizable};

use crate::git::{Area, Entry, Op, Source, Status};

use super::super::super::changes::tree::{self, Row};
use super::super::super::color::from_hex;
use super::super::super::desk::Desk;
use super::super::super::skin::Look;
use super::super::super::ui::list::{Emphasis, row_label};
use super::super::super::ui::{ghost_button, ghost_icon, muted, row, section_label};

/// Rows listed per section; a vast untracked tree is counted, not painted.
const MAX_ROWS: usize = 500;
/// Indent per folder level.
const INDENT: f32 = 12.;

struct Section {
    title: &'static str,
    scope: &'static str,
    areas: &'static [Area],
    source: Source,
    bulk: Option<(&'static str, Op)>,
}

pub(super) fn list(
    desk: &Desk,
    status: &Status,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let sections = [
        Section {
            title: "Conflicts",
            scope: "conflicts",
            areas: &[Area::Conflicted],
            source: Source::Changes,
            bulk: None,
        },
        Section {
            title: "Staged",
            scope: "staged",
            areas: &[Area::Staged],
            source: Source::Staged,
            bulk: Some(("Unstage All", Op::UnstageAll)),
        },
        Section {
            title: "Changes",
            scope: "changes",
            areas: &[Area::Unstaged, Area::Untracked],
            source: Source::Changes,
            bulk: Some(("Stage All", Op::StageAll)),
        },
    ];
    let mut list = div().flex().flex_col().px(px(6.)).py(px(6.)).gap(px(1.));
    for section in sections {
        let entries: Vec<&Entry> = section
            .areas
            .iter()
            .flat_map(|area| status.in_area(*area))
            .collect();
        if entries.is_empty() {
            continue;
        }
        list = list.child(heading(
            &section,
            entries.len(),
            desk.changes.busy,
            look,
            cx,
        ));
        let hidden = entries.len().saturating_sub(MAX_ROWS);
        let shown = &entries[..entries.len().min(MAX_ROWS)];
        let rows: Vec<Row<'_>> = if desk.changes.tree {
            tree::rows(shown, section.scope, &desk.changes.collapsed)
        } else {
            shown
                .iter()
                .map(|entry| Row::File { entry, depth: 0 })
                .collect()
        };
        for row in rows {
            list = list.child(match row {
                Row::Folder {
                    key,
                    label,
                    depth,
                    open,
                    files,
                } => folder_row(key, label, depth, open, files, look, cx),
                Row::File { entry, depth } => {
                    file_row(desk, entry, depth, section.source, look, cx)
                }
            });
        }
        if hidden > 0 {
            list = list.child(muted(look, format!("{hidden} more not shown")).px(px(8.)));
        }
    }
    div()
        .id("changes-files")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .child(list)
        .into_any_element()
}

fn heading(
    section: &Section,
    count: usize,
    busy: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let title = section.title;
    div()
        .flex()
        .items_center()
        .pt(px(6.))
        .pr(px(2.))
        .child(
            div()
                .flex_1()
                .child(section_label(&format!("{title} · {count}"), look)),
        )
        .children(section.bulk.clone().map(|(label, op)| {
            ghost_button(SharedString::from(format!("changes-bulk-{title}")), label)
                .xsmall()
                .disabled(busy)
                .on_click(cx.listener(move |desk, _, _, cx| desk.change_index(op.clone(), cx)))
        }))
}

fn folder_row(
    key: String,
    label: String,
    depth: usize,
    open: bool,
    files: usize,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let id = SharedString::from(format!("changes-folder-{key}"));
    let faint = from_hex(&look.chrome.faint);
    row(id.clone(), look)
        .debug_selector(move || id.to_string())
        .pl(px(4. + depth as f32 * INDENT))
        .on_click(cx.listener(move |desk, _, _, cx| desk.toggle_folder(&key, cx)))
        .child(
            Icon::new(if open {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            })
            .xsmall()
            .text_color(faint),
        )
        .child(
            Icon::new(if open {
                IconName::FolderOpen
            } else {
                IconName::Folder
            })
            .small()
            .text_color(from_hex(&look.chrome.muted)),
        )
        .child(
            row_label(label, Emphasis::Normal, look)
                .flex_1()
                .min_w_0()
                .truncate(),
        )
        .when(!open, |row| row.child(muted(look, files.to_string())))
        .into_any_element()
}

/// A file: status letter, name (and folder, in a flat list), its line
/// counts, a viewed mark, and a stage or unstage button on hover.
fn file_row(
    desk: &Desk,
    entry: &Entry,
    depth: usize,
    source: Source,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let (name, folder) = entry.split_name();
    let id = SharedString::from(format!("change-{:?}-{}", entry.area, entry.path));
    let group = SharedString::from(format!("change-group-{:?}-{}", entry.area, entry.path));
    let (icon, label) = match entry.area {
        Area::Staged => (IconName::Minus, "Unstage"),
        Area::Conflicted => (IconName::Check, "Mark Resolved"),
        Area::Unstaged | Area::Untracked => (IconName::Plus, "Stage"),
    };
    let tooltip = match &entry.from {
        Some(from) => format!("{from} → {}", entry.path),
        None => entry.path.clone(),
    };
    let focused =
        desk.changes.source == source && desk.changes.focus.as_deref() == Some(entry.path.as_str());
    let viewed = desk.changes.source == source && desk.changes.is_viewed(&entry.path);
    let stats = desk.changes.loaded(source).stats(&entry.path);
    let theme = &look.chrome.theme;
    let path = entry.path.clone();
    let toggle = entry.path.clone();
    row(id.clone(), look)
        .debug_selector({
            let id = id.clone();
            move || id.to_string()
        })
        .group(group.clone())
        .pl(px(4.
            + depth as f32 * INDENT
            + if desk.changes.tree { 14. } else { 0. }))
        .when(focused, |row| row.bg(look.active))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .on_click(cx.listener(move |desk, _, _, cx| desk.reveal_change(source, &path, cx)))
        .child(
            div()
                .w(px(12.))
                .flex_shrink_0()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(code_color(entry, look))
                .child(code_letter(entry).to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_baseline()
                .gap(px(6.))
                .child(
                    row_label(name.to_owned(), Emphasis::Normal, look)
                        .flex_none()
                        .max_w_full()
                        .when(viewed, |label| {
                            label.text_color(from_hex(&look.chrome.muted))
                        }),
                )
                .when(!desk.changes.tree && !folder.is_empty(), |line| {
                    line.child(muted(look, folder.to_owned()).flex_1().min_w_0().truncate())
                }),
        )
        .children(stats.map(|(added, removed)| {
            div()
                .flex()
                .flex_shrink_0()
                .gap(px(4.))
                .text_xs()
                .group_hover(group.clone(), |style| style.invisible())
                .when(added > 0, |counts| {
                    counts.child(
                        div()
                            .text_color(from_hex(&theme.success))
                            .child(format!("+{added}")),
                    )
                })
                .when(removed > 0, |counts| {
                    counts.child(
                        div()
                            .text_color(from_hex(&theme.danger))
                            .child(format!("−{removed}")),
                    )
                })
        }))
        .when(viewed, |row| {
            row.child(
                Icon::new(IconName::Eye)
                    .xsmall()
                    .text_color(from_hex(&look.chrome.faint)),
            )
        })
        .child(
            div()
                .absolute()
                .right(px(4.))
                .invisible()
                .group_hover(group, |style| style.visible())
                .child(
                    ghost_icon(SharedString::from(format!("{id}-stage")), icon, label)
                        .disabled(desk.changes.busy)
                        .on_click(cx.listener(move |desk, _, _, cx| {
                            cx.stop_propagation();
                            desk.stage_path(source, &toggle, cx);
                        })),
                ),
        )
        .into_any_element()
}

/// The letter a file row leads with. Untracked files read as added.
fn code_letter(entry: &Entry) -> char {
    match entry.code {
        '?' => 'A',
        code => code,
    }
}

fn code_color(entry: &Entry, look: &Look) -> Hsla {
    let theme = &look.chrome.theme;
    from_hex(match code_letter(entry) {
        'A' | 'C' => &theme.success,
        'D' | 'U' => &theme.danger,
        'R' | 'T' => &theme.accent,
        _ => &theme.warning,
    })
}
