//! The workspace: the active tab's split tree of pane cards, its dividers,
//! each pane's floating controls, and the empty state.
//!
//! A pane is a rounded card on the glass with no header. Its controls float
//! in when the pointer reaches the top edge, and a file dragged over it shows
//! where the drop will paste.

use crate::settings::TabPosition;
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

use card::pane_view;
use split::split_tree;

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
        .child(workspace_body(desk, look, window, cx))
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
        .pt(px(gutter))
        .pr(px(gutter))
        .pb(px(foot(desk, look)))
        .child(if let Some(id) = zoomed {
            desk.placement(&id).cloned().map_or_else(
                || div().into_any_element(),
                |placement| pane_view(desk, &placement, window, cx),
            )
        } else if let Some(root) = root {
            split_tree(desk, &root, look, window, cx).into_any_element()
        } else {
            empty_state(desk, look, cx).into_any_element()
        })
        .children(super::status::peek_card(desk, look, window, cx))
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
                "Reconnect",
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
