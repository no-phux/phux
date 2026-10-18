//! Shared transient-surface placement, overlay dispatch, and the find bar.

use super::*;

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
            "This ends every process in the session, for every client, and deletes it. Closing a pane, tab, or window never does this.",
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
            "This ends the process for every view and every client attached to it. Closing a pane only detaches.",
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
fn backdrop(
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
fn popover(
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

pub fn find_bar(desk: &Desk, case_sensitive: bool, cx: &mut Context<Desk>) -> AnyElement {
    let Some((input, _)) = desk.find_input.as_ref() else {
        return div().into_any_element();
    };
    let look = desk.look();
    card(&look, 420.)
        .absolute()
        .top(px(12.))
        .right(px(12.))
        .p(px(8.))
        .flex()
        .items_center()
        .gap(px(6.))
        .child(
            div().flex_grow(1.).child(
                Input::new(input)
                    .appearance(false)
                    .prefix(Icon::new(IconName::Search).small()),
            ),
        )
        .child(
            muted(&look, desk.find_status())
                .px(px(6.))
                .py(px(3.))
                .rounded(corner(&look, 5.))
                .bg(look.ink.opacity(0.045)),
        )
        .child(
            ghost_icon("find-case", IconName::CaseSensitive, "Match case")
                .small()
                .toggled(case_sensitive)
                .on_click(cx.listener(|desk, _, window, cx| {
                    let case = !desk
                        .find
                        .as_ref()
                        .map(|find| find.case_sensitive)
                        .unwrap_or(false);
                    let query = desk
                        .find
                        .as_ref()
                        .map(|find| find.query.clone())
                        .unwrap_or_default();
                    desk.run_search(query, case, cx);
                    desk.after_command(window, cx);
                })),
        )
        .children(
            [
                ("find-prev", IconName::ChevronUp, "Previous match"),
                ("find-next", IconName::ChevronDown, "Next match"),
                ("find-close", IconName::Close, "Close"),
            ]
            .map(|(id, icon, tooltip)| {
                ghost_icon(id, icon, tooltip)
                    .small()
                    .on_click(cx.listener(move |desk, _, window, cx| desk.execute(id, window, cx)))
            }),
        )
        .into_any_element()
}
