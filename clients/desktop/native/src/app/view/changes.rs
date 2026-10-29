//! The Changes panel: the focused pane's checkout as a review surface beside
//! the panes. The branch heads it; below, the changed files as a tree with the
//! commit field under them, and beside them every file's diff in one scroll.
//! Its left edge drags to resize it.
//!
//! The panel is a cached chrome slot: terminal frames do not rebuild it, only
//! desk notifications (a new status or patch, a keystroke in a field) and the
//! diff view's own state.
//!
//! | Module | Owns |
//! |---|---|
//! | [`files`] | the file tree or list, by section, with stage controls |
//! | [`diff`] | the toolbar, the diff, file header controls, comments |

mod diff;
mod files;

use gpui::prelude::*;
use gpui::{AnyElement, Context, FontWeight, IntoElement, MouseButton, Window, div, px};
use gpui_kit::assets::IconName;
use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::{Selectable, Sizable};

use crate::git::{Area, Status};
use crate::workspace::layout::{Axis, LayoutNode};

use super::super::changes::{Layout, View};
use super::super::color::from_hex;
use super::super::desk::Desk;
use super::super::skin::{Look, Surface, Surfaced};
use super::super::ui::{ghost_icon, muted};

/// The narrowest the panel goes to keep the panes their room: a file column
/// and a readable diff. Below it the panel holds and the panes take the rest.
const PANEL_FLOOR: f32 = 360.;
/// Room each column of side-by-side panes keeps beside the panel.
const COLUMN_MIN: f32 = 280.;
/// The file column's width, and the share of a narrow panel it takes at most.
const FILES_WIDTH: f32 = 252.;
const FILES_SHARE: f32 = 0.45;

/// The panel's width this frame: as dragged and at least its minimum where
/// the window has room, yielding to keep the panes their room (`PANES_MIN`,
/// or [`COLUMN_MIN`] for each column set side by side) down to its own
/// [`PANEL_FLOOR`].
pub(crate) fn width(desk: &Desk, viewport: f32) -> f32 {
    use super::super::changes::{MIN_WIDTH, PANES_MIN};
    let sidebar = if super::navigator::wanted(desk) {
        desk.prefs.sidebar_width as f32 * desk.look().skin.chrome.sidebar.clamp(0., 1.)
    } else {
        0.
    };
    let available = (viewport - sidebar).max(0.);
    desk.changes
        .width
        .max(MIN_WIDTH.min(viewport * 0.5))
        .min(available - PANES_MIN.max(columns(desk) as f32 * COLUMN_MIN))
        .max(PANEL_FLOOR.min(available))
}

/// How many panes the active tab sets side by side.
fn columns(desk: &Desk) -> usize {
    fn across(node: &LayoutNode) -> usize {
        match node {
            LayoutNode::Leaf { .. } => 1,
            LayoutNode::Split {
                axis: Axis::Row,
                first,
                second,
                ..
            } => across(first) + across(second),
            LayoutNode::Split { first, second, .. } => across(first).max(across(second)),
        }
    }
    desk.active_tab().map_or(1, |tab| {
        if tab.zoomed_id.is_some() {
            1
        } else {
            across(&tab.root)
        }
    })
}

pub(super) fn build(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    desk.prepare_changes_input(window, cx);
    let look = desk.look();
    let gutter = look.skin.shape.gutter;
    let panel = width(desk, window.viewport_size().width.as_f32());
    let body = match desk.changes.view.clone() {
        View::Repo(status) if !status.entries.is_empty() => {
            let files_width = FILES_WIDTH.min(panel * FILES_SHARE);
            let diff_width = panel - files_width - gutter;
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(
                    div()
                        .w(px(files_width))
                        .flex_shrink_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .border_r_1()
                        .border_color(look.divider)
                        .child(files::list(desk, &status, &look, cx))
                        .child(commit_box(desk, &look, cx)),
                )
                .child(diff::column(desk, diff_width, &look, cx))
                .into_any_element()
        }
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
        .relative()
        .pt(px(super::panes::top_inset(desk, &look)))
        .pr(px(gutter))
        // Its foot is the window's, whatever sits under the panes.
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
                // Floating on the sheet it is framed like a card; flush
                // against the window it is only split from the panes.
                .when(gutter > 0., |panel| panel.border_color(look.border))
                .when(gutter <= 0., |panel| {
                    panel.border_0().border_l_1().border_color(look.divider)
                })
                .child(header(desk, &look, cx))
                .child(body),
        )
        .child(resize(desk, gutter, &look, cx))
        .into_any_element()
}

/// The left edge, dragged to resize the panel, down to the chrome under it.
fn resize(desk: &Desk, bottom: f32, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let active = desk.dragging_changes();
    let accent = from_hex(&look.chrome.theme.accent);
    div()
        .id("changes-resize")
        .debug_selector(|| "changes-resize".into())
        .absolute()
        .left(px(-3.))
        .top(px(0.))
        .bottom(px(bottom))
        .w(px(6.))
        .cursor_col_resize()
        .when(active, |handle| handle.bg(accent))
        .hover(move |style| style.bg(accent.opacity(0.5)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|desk, _, _, cx| desk.begin_changes_drag(cx)),
        )
}

/// The branch, or where HEAD is detached.
fn branch_title(status: Option<&Status>) -> String {
    let Some(branch) = status.map(|status| &status.branch) else {
        return "Changes".into();
    };
    branch.name.clone().unwrap_or_else(|| {
        let commit = branch.commit.as_deref().unwrap_or("no commit");
        format!("detached at {commit}")
    })
}

/// The repository, then how the branch stands against its upstream.
fn branch_detail(status: &Status) -> String {
    let branch = &status.branch;
    let drift = [
        (branch.ahead > 0).then(|| format!("{} to push", branch.ahead)),
        (branch.behind > 0).then(|| format!("{} to pull", branch.behind)),
        (branch.upstream.is_none() && branch.name.is_some()).then(|| "no upstream".into()),
    ];
    std::iter::once(status.name())
        .chain(drift.into_iter().flatten())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The layout button's icon and tooltip: what it shows, and what a click does.
pub(super) fn layout_control(layout: Layout) -> (IconName, &'static str) {
    match layout {
        Layout::Auto => (
            IconName::Columns2,
            "Layout: Automatic (click for Side by Side)",
        ),
        Layout::Split => (
            IconName::Columns2,
            "Layout: Side by Side (click for Unified)",
        ),
        Layout::Unified => (IconName::Rows2, "Layout: Unified (click for Automatic)"),
    }
}

/// Branch, upstream drift and the repository, with view toggles, refresh and close.
fn header(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let status = desk.changes.status();
    let (layout_icon, layout_label) = layout_control(desk.changes.layout);
    let (tree_icon, tree_label) = if desk.changes.tree {
        (IconName::List, "Show as List")
    } else {
        (IconName::ListTree, "Show as Tree")
    };
    let repo = status.is_some();
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(6.))
        // The trailing ghost buttons pad their icons by 4pt, and a close
        // glyph draws narrower than the branch glyph: these insets put both
        // edge glyphs the same distance from the rim.
        .pl(px(14.))
        .pr(px(8.))
        .py(px(8.))
        .border_b_1()
        .border_color(look.divider)
        .child(
            gpui_kit::component::Icon::new(IconName::GitBranch)
                .xsmall()
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
                        .child(branch_title(status)),
                )
                .children(status.map(|status| muted(look, branch_detail(status)).truncate())),
        )
        .when(repo, |header| {
            header
                .child(
                    ghost_icon("changes-tree", tree_icon, tree_label).on_click(cx.listener(
                        |desk, _, _, cx| {
                            desk.changes.tree = !desk.changes.tree;
                            cx.notify();
                        },
                    )),
                )
                .child(
                    ghost_icon("changes-layout", layout_icon, layout_label)
                        .selected(desk.changes.layout != Layout::Auto)
                        .on_click(cx.listener(|desk, _, _, cx| {
                            desk.changes.layout = desk.changes.layout.next();
                            cx.notify();
                        })),
                )
        })
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
                .w_full()
                .text_center()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(from_hex(&look.chrome.subtext))
                .child(title.to_owned()),
        )
        .when(!detail.is_empty(), |column| {
            column.child(muted(look, detail.to_owned()).w_full().text_center())
        })
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
