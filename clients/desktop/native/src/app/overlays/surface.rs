//! Where transient surfaces sit: the modal layer, its backdrop and popover,
//! and the card every palette, panel and dialog is drawn on.

use gpui::prelude::*;
use gpui::{AnyElement, Context, IntoElement, MouseButton, Window, div, px};

use super::dialogs::{
    add_host_dialog, confirm, paste_question, project_dialog, rename_dialog, session_dialog,
};
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
        Modal::AgentFleet => crate::app::fleet::dashboard::panel(desk, &look, window, cx),
        Modal::Commands
        | Modal::Goto
        | Modal::WindowSession
        | Modal::Windows
        | Modal::MoveResources => launcher::palette(desk, colors, window, cx),
        Modal::Path => {
            let cursor = desk.cursor_anchor(cx);
            let panel = path_panel(desk, colors, &look, cursor.is_some(), cx);
            return match cursor {
                Some(cursor) => at_cursor(cursor, window, cx, panel),
                None => backdrop(colors, window, cx, panel),
            };
        }
        Modal::Themes => super::theme_panel::theme_panel(desk, &look, window, cx),
        Modal::Notifications => {
            let panel = notifications_panel(desk, colors, &look, window, cx);
            return match desk.anchors.get("notifications") {
                Some(bell) => popover(bell, window, cx, panel),
                None => backdrop(colors, window, cx, panel),
            };
        }
        Modal::NativeLaunch => crate::app::native_launch::panel(desk, &look, window, cx),
        Modal::Rename { .. } => rename_dialog(desk, colors, &look, cx),
        Modal::Session { create, .. } => session_dialog(desk, create, colors, &look, cx),
        Modal::Project { name, current, .. } => {
            project_dialog(desk, &name, current.is_some(), colors, &look, cx)
        }
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
            "Its program is not expecting a paste, so each line break arrives as Enter and runs the line before it.",
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

/// A modal's card: at most `width` wide, never wider than the window's
/// gutters allow, and a click inside it never reaches the backdrop.
pub(crate) fn sheet(look: &Look, width: f32) -> gpui::Div {
    card(look, width)
        .debug_selector(|| "modal-sheet".into())
        .max_w_full()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

/// A dimmed layer over the window that blocks the terminal, places the modal
/// at the window's center, and closes it on a click outside the card.
pub(super) fn backdrop(
    colors: &Palette,
    _window: &Window,
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
        .justify_center()
        .p(px(SHEET_GUTTER))
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

/// The width and tallest height of a panel opened at the terminal cursor.
pub(super) const AT_CURSOR: (f32, f32) = (460., 300.);

/// A panel opened at the terminal cursor, as a completion list opens: below
/// the cursor's line, or above it when the window has no room below; kept
/// inside the window, no dimming, and a click outside closes it.
pub(super) fn at_cursor(
    cursor: gpui::Bounds<gpui::Pixels>,
    window: &Window,
    cx: &mut Context<Desk>,
    card: impl IntoElement,
) -> AnyElement {
    let spot = cursor_spot(cursor, window.viewport_size());
    let placed = div().absolute().left(spot.left);
    let placed = match spot.edge {
        Edge::Top(top) => placed.top(top),
        Edge::Bottom(bottom) => placed.bottom(bottom),
    };
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
            placed
                .debug_selector(|| "path-at-cursor".into())
                .child(card),
        )
        .into_any_element()
}

/// Where a panel at the cursor sits: its left edge, and the edge it hangs
/// from (a top below the cursor's line, or a bottom above it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Spot {
    pub(super) left: gpui::Pixels,
    pub(super) edge: Edge,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Edge {
    Top(gpui::Pixels),
    Bottom(gpui::Pixels),
}

/// Below the cursor's line when the tallest panel fits there, else above it;
/// shifted left as far as it must to stay inside the window.
pub(super) fn cursor_spot(
    cursor: gpui::Bounds<gpui::Pixels>,
    viewport: gpui::Size<gpui::Pixels>,
) -> Spot {
    let gutter = px(SHEET_GUTTER / 2.);
    let (width, height) = (px(AT_CURSOR.0), px(AT_CURSOR.1));
    let left = cursor
        .left()
        .min(viewport.width - width - gutter)
        .max(gutter);
    let edge = if viewport.height - cursor.bottom() - gutter >= height {
        Edge::Top(cursor.bottom() + px(4.))
    } else {
        Edge::Bottom(viewport.height - cursor.top() + px(4.))
    };
    Spot { left, edge }
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

/// A box `inset` points inside a [`card`]'s outer edge: its corner is
/// concentric with the card's, down to half a kit corner.
pub(super) fn inset_radius(look: &Look, inset: f32) -> gpui::Pixels {
    let shape = &look.skin.shape;
    px((shape.card_radius - inset).max(shape.kit_radius / 2.))
}

/// The line box GPUI gives `size` text at the default line height.
pub(crate) fn line_height(window: &Window, size: gpui::Pixels) -> gpui::Pixels {
    gpui::TextStyle {
        font_size: size.into(),
        ..Default::default()
    }
    .line_height_in_pixels(window.rem_size())
}

/// How far below the top of a `line`-high line box GPUI paints the baseline
/// of `size` text in the UI font: half the leading, then the ascent.
pub(super) fn baseline(cx: &gpui::App, size: gpui::Pixels, line: gpui::Pixels) -> gpui::Pixels {
    use gpui_kit::component::ActiveTheme;
    let text = cx.text_system();
    let font = text.resolve_font(&gpui::font(cx.theme().font_family.clone()));
    let ascent = text.ascent(font, size);
    let descent = text.descent(font, size).abs();
    (line - ascent - descent) / 2. + ascent
}

#[cfg(test)]
mod cursor_spot_tests {
    use super::{Edge, Spot, cursor_spot};
    use gpui::{Bounds, point, px, size};

    fn cell(x: f32, y: f32) -> Bounds<gpui::Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(8.), px(16.)))
    }

    #[test]
    fn the_list_hangs_below_the_cursor_and_flips_above_near_the_bottom() {
        let window = size(px(1280.), px(800.));
        assert_eq!(
            cursor_spot(cell(300., 200.), window),
            Spot {
                left: px(300.),
                edge: Edge::Top(px(220.))
            }
        );
        assert_eq!(
            cursor_spot(cell(300., 700.), window).edge,
            Edge::Bottom(px(104.))
        );
        // Near the right edge it shifts left to stay whole.
        assert_eq!(cursor_spot(cell(1200., 200.), window).left, px(812.));
    }
}
