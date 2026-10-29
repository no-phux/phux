//! The workspace: the active tab's split tree of pane cards, its dividers,
//! each pane's floating controls, and the empty state.
//!
//! A pane is a rounded card on the glass with no header. Its controls float
//! in when the pointer reaches the top edge, and a file dragged over it shows
//! where the drop will paste.

use crate::settings::TabPosition;
use crate::workspace::layout;
use gpui::prelude::*;
use gpui::{
    AnimationExt, Context, IntoElement, ParentElement, StyleRefinement, Styled, Window, div, px,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::kbd::Kbd;
use phux_client_runtime::control::Status;

use super::super::color::from_hex;
use super::super::desk::Desk;
use super::super::skin::Look;
use super::super::ui::{keystroke, muted};

mod card;
mod controls;
mod depth;
mod split;
mod stack;

use split::split_tree;
use stack::zoom_stack;

pub(super) fn main_column(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    div()
        .debug_selector(|| "main-column".into())
        .flex_grow(1.)
        .min_w(px(0.))
        .min_h(px(0.))
        .flex()
        .flex_col()
        .child(workspace_viewport(desk, look, window, cx))
        .when(
            super::tabs_at(desk, &look.skin, TabPosition::Bottom),
            |column| {
                column.child(super::chrome_slot(
                    &mut desk.chrome.bottom_tabs,
                    super::tabs::build_bottom,
                    StyleRefinement::default()
                        .w_full()
                        .h(px(look.skin.chrome.titlebar)),
                    cx,
                ))
            },
        )
}

fn workspace_viewport(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let open = desk.tab_peek.open;
    let available = window.viewport_size().height.as_f32() - super::content_top(desk, &look.skin);
    let height = super::tab_peek::height(available);
    let shelf = open.then(|| super::tab_peek::shelf(desk, look, height, cx));
    let moves = super::chrome_moves(desk, cx);
    // A relative offset changes paint/hit bounds, not layout allocation. The
    // terminal keeps its entire height while the viewport clips its bottom.
    div()
        .debug_selector(|| "workspace-viewport".into())
        .flex_1()
        .min_h_0()
        .relative()
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(
            div()
                .size_full()
                .flex_shrink_0()
                .relative()
                .flex()
                .flex_col()
                .child(workspace_body(desk, look, window, cx))
                .when(open, |body| body.child(super::tab_peek::dismiss(cx)))
                .with_spring(
                    "tab-peek-offset",
                    super::chrome_spring(moves, if open { height } else { 0. }),
                    |body, offset| body.top(px(offset)),
                ),
        )
        .children(shelf)
}

/// The space the workspace keeps under its cards: none over the bottom tab
/// strip, which spaces itself, else one gutter.
pub(super) fn foot(desk: &Desk, look: &Look) -> f32 {
    if super::tabs_at(desk, &look.skin, TabPosition::Bottom) {
        0.
    } else {
        look.skin.shape.gutter
    }
}

/// The cards' inset from the main column's leading edge: one gutter, less
/// the sidebar's own row inset while it shows, so the space between the
/// sidebar's rows and the first card reads as one gutter too.
pub(super) fn lead_inset(desk: &Desk, look: &Look) -> f32 {
    let gutter = look.skin.shape.gutter;
    let sidebar = super::navigator::wanted(desk) && look.skin.chrome.sidebar > 0.;
    if sidebar {
        (gutter - super::navigator::LIST_INSET * look.skin.chrome.sidebar.clamp(0., 1.)).max(0.)
    } else {
        gutter
    }
}

/// A top tab strip supplies separation; a compact unified header already
/// meets the cards, so it must not keep the former strip's extra gutter.
pub(super) fn top_inset(desk: &Desk, look: &Look) -> f32 {
    if super::integrated_titlebar(desk, &look.skin)
        || desk.prefs.tab_position == TabPosition::Top
        || super::quick(desk)
    {
        look.skin.shape.gutter
    } else {
        0.
    }
}

fn workspace_body(
    desk: &mut Desk,
    look: &Look,
    window: &mut Window,
    cx: &mut Context<Desk>,
) -> impl IntoElement {
    let gutter = look.skin.shape.gutter;
    let lead = lead_inset(desk, look);
    let moves = super::chrome_moves(desk, cx);
    let zoomed = desk.active_tab().and_then(|tab| tab.zoomed_id.clone());
    let root = desk.active_tab().map(|tab| tab.root.clone());
    div()
        .debug_selector(|| "workspace-body".into())
        .flex_grow(1.)
        .min_h(px(0.))
        .relative()
        .pt(px(top_inset(desk, look)))
        .pr(px(gutter))
        .pb(px(foot(desk, look)))
        .child(if let Some(id) = zoomed {
            let behind = root
                .as_ref()
                .map_or(0, |root| layout::placements(root).len().saturating_sub(1));
            desk.placement(&id).cloned().map_or_else(
                || div().into_any_element(),
                |placement| zoom_stack(desk, &placement, behind, look, window, cx),
            )
        } else if let Some(root) = root {
            split_tree(desk, &root, look, window, cx).into_any_element()
        } else {
            empty_state(desk, look, cx).into_any_element()
        })
        .children(super::status::peek_card(desk, look, window, cx))
        .when(super::integrated_titlebar(desk, &look.skin), |body| {
            body.child(div().id("workspace-drag").debug_selector(|| "workspace-drag".into())
                .absolute().top_0().left_0().right_0().h(px(gutter))
                .on_mouse_down(gpui::MouseButton::Left, crate::chrome::drag_window))
        })
        // Springs with the sidebar, so the cards keep their gap as it moves.
        .with_spring(
            "workspace-lead",
            super::chrome_spring(moves, lead),
            |body, inset| body.pl(px(inset)),
        )
}

fn empty_state(desk: &Desk, look: &Look, cx: &mut Context<Desk>) -> impl IntoElement {
    let status = desk.link.as_ref().map(|link| link.connection.status);
    let error = desk.connect_error.clone().or_else(|| {
        desk.link
            .as_ref()
            .and_then(|link| link.connection.error.clone())
    });
    let pending = desk
        .link
        .as_ref()
        .is_some_and(|link| link.workspace.pending() > 0);
    let new = if pending {
        "Starting terminal…"
    } else {
        "New terminal"
    };
    let body = match status {
        Some(Status::Attached) => div().flex().flex_col().items_center().gap(px(8.)).children(
            [
                ("empty-new", new, "new", true),
                ("empty-folder", "Open folder…", "folder", false),
                ("empty-palette", "Command palette", "palette", false),
            ]
            .map(|(id, label, command, primary)| {
                empty_action(desk, id, label, command, primary, cx)
            }),
        ),
        Some(Status::Idle | Status::Connecting | Status::Negotiated) => muted(
            look,
            match desk.remote.as_deref() {
                Some(host) => format!(
                    "Connecting to {}…",
                    super::super::machine_name::display(host)
                ),
                None => format!("Connecting to {}…", desk.socket.display()),
            },
        )
        .text_sm(),
        _ => div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .max_w(px(520.))
                    .text_center()
                    .text_sm()
                    .text_color(from_hex(&look.chrome.theme.danger))
                    .child(error.unwrap_or_else(|| "The connection closed.".into())),
            )
            .child(empty_action(
                desk,
                "empty-reconnect",
                "Retry connection",
                "reconnect",
                true,
                cx,
            )),
    };
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(18.))
        .child(
            div()
                .text_size(px(34.))
                .font_weight(gpui::FontWeight::EXTRA_BOLD)
                .child("phux"),
        )
        .child(body)
}

fn empty_action(
    desk: &Desk,
    id: &'static str,
    label: &str,
    command: &'static str,
    primary: bool,
    cx: &mut Context<Desk>,
) -> gpui::Div {
    let kbd = command_kbd(desk, command);
    let button = Button::new(id)
        .label(label.to_owned())
        .w(px(240.))
        .on_click(cx.listener(move |desk, _, window, cx| desk.execute(command, window, cx)));
    let button = if primary { button.primary() } else { button };
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(button)
        .child(div().w(px(64.)).children(kbd))
}

fn command_kbd(desk: &Desk, id: &str) -> Option<Kbd> {
    keystroke(desk.bound_chord(id)?).map(Kbd::new)
}
