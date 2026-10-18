//! The Changes panel: the focused pane's checkout as a card beside the panes.
//! Its branch heads the card, changed files follow by area, a file's diff
//! opens under its row, and the commit field closes the card.
//!
//! The panel is a cached chrome slot: terminal frames do not rebuild it, only
//! desk notifications (a new status, a diff, a keystroke in the field).

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, Hsla, IntoElement, SharedString, Window, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Textarea;

use crate::git::{Area, Diff, DiffLine, Entry, LineKind, Op, Status};

use super::super::changes::{Selected, View};
use super::super::color::from_hex;
use super::super::desk::Desk;
use super::super::skin::{Look, Surface, Surfaced};
use super::super::ui::list::{Emphasis, row_label};
use super::super::ui::{ghost_button, ghost_icon, muted, row, row_icon, section_label};

/// The card's width beside the panes.
pub(super) const WIDTH: f32 = 380.;
/// Rows listed per section; a vast untracked tree is counted, not painted.
const MAX_ROWS: usize = 500;

pub(super) fn build(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    desk.prepare_changes_input(window, cx);
    let look = desk.look();
    let gutter = look.skin.shape.gutter;
    let mono = SharedString::from(desk.prefs.font_family.clone());
    let body = match desk.changes.view.clone() {
        View::Repo(status) if !status.entries.is_empty() => files(desk, &status, &mono, &look, cx),
        View::Repo(_) => placeholder("Nothing to commit", "The working tree is clean.", &look),
        View::Loading => placeholder("Reading changes…", "", &look),
        View::NotRepo(folder) => {
            placeholder("Not a Git checkout", &folder.display().to_string(), &look)
        }
        View::Unavailable(reason) => placeholder("No changes to show", &reason, &look),
        View::Failed(error) => placeholder("Could not read changes", &error, &look),
    };
    div()
        .size_full()
        .pt(px(gutter))
        .pr(px(gutter))
        .pb(px(gutter))
        .child(
            div()
                .id("changes")
                .debug_selector(|| "changes-panel".into())
                .size_full()
                .flex()
                .flex_col()
                .overflow_hidden()
                .surface(&look, Surface::Pane)
                .border_1()
                .border_color(look.border)
                .child(header(desk, &look, cx))
                .child(body)
                .children(
                    desk.changes
                        .status()
                        .is_some()
                        .then(|| commit_box(desk, &look, cx)),
                ),
        )
        .into_any_element()
}

/// Branch, upstream drift and the repository, with refresh and close.
fn header(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let status = desk.changes.status();
    let branch = status.map_or_else(
        || "Changes".to_owned(),
        |status| {
            status.branch.name.clone().unwrap_or_else(|| {
                let commit = status.branch.commit.as_deref().unwrap_or("no commit");
                format!("detached at {commit}")
            })
        },
    );
    let mut detail = Vec::new();
    if let Some(status) = status {
        detail.push(status.name());
        let b = &status.branch;
        if b.ahead > 0 {
            detail.push(format!("{} to push", b.ahead));
        }
        if b.behind > 0 {
            detail.push(format!("{} to pull", b.behind));
        }
        if b.upstream.is_none() && b.name.is_some() {
            detail.push("no upstream".into());
        }
    }
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(8.))
        .px(px(12.))
        .py(px(10.))
        .border_b_1()
        .border_color(look.divider)
        .child(
            gpui_kit::component::Icon::new(IconName::GitBranch)
                .small()
                .text_color(from_hex(&look.chrome.muted)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(branch),
                )
                .when(!detail.is_empty(), |column| {
                    column.child(muted(look, detail.join(" · ")).truncate())
                }),
        )
        .child(
            ghost_icon("changes-refresh", IconName::RefreshCw, "Refresh").on_click(cx.listener(
                |desk, _, _, cx| {
                    desk.refresh_changes(true, cx);
                },
            )),
        )
        .child(
            ghost_icon("changes-close", IconName::Close, "Close Changes")
                .on_click(cx.listener(|desk, _, window, cx| desk.execute("changes", window, cx))),
        )
}

fn placeholder(title: &str, detail: &str, look: &Look) -> AnyElement {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(4.))
        .px(px(24.))
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(from_hex(&look.chrome.subtext))
                .child(title.to_owned()),
        )
        .when(!detail.is_empty(), |column| {
            column.child(muted(look, detail.to_owned()).text_center())
        })
        .into_any_element()
}

/// The scrolling file list: conflicts, staged, then working-tree changes.
fn files(
    desk: &Desk,
    status: &Status,
    mono: &SharedString,
    look: &Look,
    cx: &mut Context<Desk>,
) -> AnyElement {
    let sections = [
        ("Conflicts", vec![Area::Conflicted], None),
        (
            "Staged",
            vec![Area::Staged],
            Some(("Unstage All", Op::UnstageAll)),
        ),
        (
            "Changes",
            vec![Area::Unstaged, Area::Untracked],
            Some(("Stage All", Op::StageAll)),
        ),
    ];
    let mut list = div().flex().flex_col().px(px(6.)).py(px(6.)).gap(px(2.));
    for (title, areas, bulk) in sections {
        let entries: Vec<&Entry> = areas
            .iter()
            .flat_map(|area| status.in_area(*area))
            .collect();
        if entries.is_empty() {
            continue;
        }
        list = list.child(section(
            title,
            entries.len(),
            bulk,
            desk.changes.busy,
            look,
            cx,
        ));
        let hidden = entries.len().saturating_sub(MAX_ROWS);
        for entry in entries.into_iter().take(MAX_ROWS) {
            let selected = desk
                .changes
                .selected
                .as_ref()
                .filter(|selected| &selected.entry == entry);
            list = list.child(file_row(
                entry,
                selected.is_some(),
                desk.changes.busy,
                look,
                cx,
            ));
            if let Some(selected) = selected {
                list = list.child(diff_view(selected, mono, look));
            }
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
        .track_scroll(&desk.changes.scroll)
        .child(list)
        .into_any_element()
}

fn section(
    title: &str,
    count: usize,
    bulk: Option<(&'static str, Op)>,
    busy: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
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
        .children(bulk.map(|(label, op)| {
            ghost_button(SharedString::from(format!("changes-bulk-{title}")), label)
                .xsmall()
                .disabled(busy)
                .on_click(cx.listener(move |desk, _, _, cx| desk.change_index(op.clone(), cx)))
        }))
}

/// A file: its status letter, name and folder, and a stage or unstage button
/// that shows on hover.
fn file_row(
    entry: &Entry,
    selected: bool,
    busy: bool,
    look: &Look,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
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
    let open = entry.clone();
    let toggle = entry.clone();
    row(id.clone(), look)
        .debug_selector({
            let id = id.clone();
            move || id.to_string()
        })
        .group(group.clone())
        .when(selected, |row| row.bg(look.active))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .on_click(cx.listener(move |desk, _, _, cx| desk.select_change(open.clone(), cx)))
        .child(row_icon(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(code_color(entry, look))
                .child(code_letter(entry).to_string()),
        ))
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
                        .max_w_full(),
                )
                .when(!folder.is_empty(), |line| {
                    line.child(muted(look, folder.to_owned()).flex_1().min_w_0().truncate())
                }),
        )
        .child(
            div()
                .invisible()
                .group_hover(group, |style| style.visible())
                .child(
                    ghost_icon(SharedString::from(format!("{id}-stage")), icon, label)
                        .disabled(busy)
                        .on_click(cx.listener(move |desk, _, _, cx| {
                            cx.stop_propagation();
                            desk.stage_change(&toggle, cx);
                        })),
                ),
        )
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

/// The open file's hunks with line numbers, added and removed lines washed.
/// File headers are left out: the row above already names the file.
fn diff_view(selected: &Selected, mono: &SharedString, look: &Look) -> AnyElement {
    let container = div()
        .mx(px(4.))
        .mb(px(4.))
        .py(px(4.))
        .rounded(px(look.skin.shape.kit_radius))
        .bg(look.hover)
        .font_family(mono.clone())
        .text_xs();
    let diff = match &selected.diff {
        None => {
            return container
                .px(px(8.))
                .child(muted(look, "Loading diff…"))
                .into_any_element();
        }
        Some(Err(error)) => {
            return container
                .px(px(8.))
                .child(muted(look, error.clone()))
                .into_any_element();
        }
        Some(Ok(diff)) => diff,
    };
    container
        .id(SharedString::from(format!("diff-{}", selected.entry.path)))
        .overflow_x_scroll()
        .child(
            div()
                .flex()
                .flex_col()
                .min_w_full()
                .children(summary(diff, look))
                .children(
                    diff.lines
                        .iter()
                        .filter(|line| line.kind != LineKind::Meta)
                        .map(|line| diff_line(line, look)),
                ),
        )
        .into_any_element()
}

fn summary(diff: &Diff, look: &Look) -> Option<AnyElement> {
    if diff.added + diff.removed == 0 {
        return None;
    }
    let theme = &look.chrome.theme;
    Some(
        div()
            .flex()
            .gap(px(8.))
            .px(px(8.))
            .pb(px(2.))
            .child(
                div()
                    .text_color(from_hex(&theme.success))
                    .child(format!("+{}", diff.added)),
            )
            .child(
                div()
                    .text_color(from_hex(&theme.danger))
                    .child(format!("−{}", diff.removed)),
            )
            .into_any_element(),
    )
}

fn diff_line(line: &DiffLine, look: &Look) -> AnyElement {
    let theme = &look.chrome.theme;
    let (wash, ink) = match line.kind {
        LineKind::Added => (Some(from_hex(&theme.success).opacity(0.16)), look.ink),
        LineKind::Removed => (Some(from_hex(&theme.danger).opacity(0.16)), look.ink),
        LineKind::Hunk => (None, from_hex(&theme.accent)),
        LineKind::Note | LineKind::Meta => (None, from_hex(&look.chrome.muted)),
        LineKind::Context => (None, from_hex(&look.chrome.subtext)),
    };
    let number = line.number.map(|n| n.to_string()).unwrap_or_default();
    div()
        .flex()
        .whitespace_nowrap()
        .when_some(wash, |line, wash| line.bg(wash))
        .child(
            div()
                .w(px(36.))
                .flex_shrink_0()
                .pr(px(6.))
                .text_right()
                .text_color(from_hex(&look.chrome.faint))
                .child(number),
        )
        .child(div().pr(px(8.)).text_color(ink).child(line.text.clone()))
        .into_any_element()
}

/// The message field and the commit button, with what will be committed.
fn commit_box(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> AnyElement {
    let staged = desk
        .changes
        .status()
        .map_or(0, |status| status.count(Area::Staged));
    let conflicted = desk
        .changes
        .status()
        .is_some_and(|status| status.count(Area::Conflicted) > 0);
    let note = match (conflicted, staged) {
        (true, _) => "Resolve conflicts first".to_owned(),
        (false, 0) => "Stage files to commit".to_owned(),
        (false, 1) => "1 file staged".to_owned(),
        (false, n) => format!("{n} files staged"),
    };
    let ready = desk.changes.can_commit(cx);
    let busy = desk.changes.busy;
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .gap(px(8.))
        .p(px(10.))
        .border_t_1()
        .border_color(look.divider)
        .children(
            desk.changes
                .message
                .as_ref()
                .map(|(input, _)| Textarea::new(input).aria_label("Commit message").w_full()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(muted(look, note).flex_1())
                .child(
                    Button::new("changes-commit")
                        .label(if busy { "Working…" } else { "Commit" })
                        .primary()
                        .small()
                        .disabled(!ready)
                        .on_click(cx.listener(|desk, _, _, cx| desk.commit_changes(cx))),
                ),
        )
        .into_any_element()
}
