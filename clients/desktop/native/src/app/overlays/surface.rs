//! Where transient surfaces sit: the modal layer, its backdrop and popover,
//! and the card every palette, panel and dialog is drawn on.

use gpui::prelude::*;
use gpui::{AnyElement, Context, IntoElement, MouseButton, Window, div, px};

use super::dialogs::{add_host_dialog, confirm, paste_question, rename_dialog, session_dialog};
use super::launcher;
use super::notifications::notifications_panel;
use super::path_panel::path_panel;
use crate::app::color::from_hex;
use crate::app::desk::{Desk, Modal};
use crate::app::skin::{Look, Surface, Surfaced};
use crate::settings::theme::Palette;

pub fn layer(desk: &mut Desk, window: &mut Window, cx: &mut Context<Desk>) -> AnyElement {
    let look = desk.look();
    let colors = &look.chrome;
    let card = match desk.modal.clone() {
        Modal::None => return div().into_any_element(),
        Modal::Capture => crate::app::capture::panel(desk, &look, window, cx),
        Modal::Commands
        | Modal::Goto
        | Modal::WindowSession
        | Modal::Windows
        | Modal::MoveResources => launcher::palette(desk, colors, window, cx),
        Modal::Path => path_panel(desk, colors, &look, cx),
        Modal::Settings if desk.settings_page.is_some() => {
            crate::app::settings_panel::panel(desk, window, cx)
        }
        Modal::Settings => launcher::palette(desk, colors, window, cx),
        Modal::Notifications => {
            let panel = notifications_panel(desk, colors, &look, cx);
            return match desk.anchors.get("notifications") {
                Some(bell) => popover(bell, window, cx, panel),
                None => backdrop(colors, window, cx, panel),
            };
        }
        Modal::NativeLaunch => crate::app::native_launch::panel(desk, &look, cx),
        Modal::Rename { .. } => rename_dialog(desk, colors, &look, cx),
        Modal::Session { create, .. } => session_dialog(desk, create, colors, &look, cx),
        Modal::KillSession { name, .. } => confirm(
            format!("Kill {name}?"),
            "This ends every process in the session, for every client, and deletes it. Closing a pane or tab ends its processes too; closing a window only detaches.",
            "Kill Session",
            colors,
            &look,
            cx,
            |desk, window, cx| desk.confirm_kill_session(window, cx),
        ),
        Modal::AddHost => add_host_dialog(desk, colors, &look, cx),
        Modal::RemoveHost { name } => confirm(
            format!("Remove {name}?"),
            "This forgets the machine here. Its server and terminals keep running; `phux host add` registers it again.",
            "Remove Host",
            colors,
            &look,
            cx,
            |desk, window, cx| desk.confirm_remove_host(window, cx),
        ),
        Modal::Terminate { title, .. } => confirm(
            format!("Terminate {title}?"),
            "This ends the process for every view and every client attached to it. Close Pane does the same without this confirmation.",
            "Terminate",
            colors,
            &look,
            cx,
            |desk, window, cx| desk.confirm_terminate(window, cx),
        ),
        Modal::Paste { title, paste, .. } => confirm(
            paste_question(&title, paste.lines()),
            "It is running a command, so each line goes to that program as input, not to a shell prompt.",
            "Paste",
            colors,
            &look,
            cx,
            |desk, window, cx| desk.confirm_paste(window, cx),
        ),
    };
    backdrop(colors, window, cx, card)
}

/// The gap between a transient surface and the window's edges.
pub(crate) const SHEET_GUTTER: f32 = 16.;

/// How far below the window top every palette, panel and dialog sits. One
/// placement for every transient surface, scaled to the window height.
pub(crate) fn sheet_top(viewport: gpui::Size<gpui::Pixels>) -> f32 {
    (viewport.height.as_f32() * 0.13).clamp(SHEET_GUTTER, 84.)
}

/// A modal's card: at most `width` wide, never wider than the window's
/// gutters allow, and a click inside it never reaches the backdrop.
pub(crate) fn sheet(look: &Look, width: f32) -> gpui::Div {
    card(look, width)
        .debug_selector(|| "modal-sheet".into())
        .max_w_full()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A dimmed layer over the window that blocks the terminal, places the modal
/// at [`sheet_top`], and closes it on a click outside the card.
pub(super) fn backdrop(
    colors: &Palette,
    window: &Window,
    cx: &mut Context<Desk>,
    card: impl IntoElement,
) -> AnyElement {
    div()
        .id("modal-backdrop")
        .absolute()
        .inset_0()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(sheet_top(window.viewport_size())))
        .px(px(SHEET_GUTTER))
        .pb(px(SHEET_GUTTER))
        .bg(from_hex(&colors.backdrop))
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|desk, _, window, cx| desk.close_modal(window, cx)),
        )
        .child(card)
        .into_any_element()
}

/// A panel dropped from its trigger, as a menu drops: no dimming, the
/// workspace stays in view, and a click anywhere outside closes it.
pub(super) fn popover(
    trigger: gpui::Bounds<gpui::Pixels>,
    window: &Window,
    cx: &mut Context<Desk>,
    card: impl IntoElement,
) -> AnyElement {
    let viewport = window.viewport_size();
    let right = (viewport.width - trigger.right()).max(px(SHEET_GUTTER / 2.));
    div()
        .id("modal-backdrop")
        .absolute()
        .inset_0()
        .size_full()
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|desk, _, window, cx| desk.close_modal(window, cx)),
        )
        .child(
            div()
                .absolute()
                .top(trigger.bottom() + px(6.))
                .right(right)
                .max_h(viewport.height - trigger.bottom() - px(SHEET_GUTTER))
                .child(card),
        )
        .into_any_element()
}

/// A dialog or palette card. The skin decides how much it lifts; the backing
/// stays near-opaque so text reads over terminal content.
pub(crate) fn card(look: &Look, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .text_color(look.ink)
        .surface(look, Surface::Popover)
        .occlude()
}

/// `glass` points of corner on an inner element, scaled to the skin's kit
/// radius so the glass skin keeps its shipped corners.
pub(crate) fn corner(look: &Look, glass: f32) -> gpui::Pixels {
    let kit = crate::settings::skin::GLASS.shape.kit_radius;
    px(glass * look.skin.shape.kit_radius / kit)
}

/// An inset row inside a [`card`]: its corner follows the card's.
pub(super) fn inset_radius(look: &Look) -> gpui::Pixels {
    px((look.skin.shape.card_radius - 4.).max(0.))
}
