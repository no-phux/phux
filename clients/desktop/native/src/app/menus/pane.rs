//! A pane's action menus: the pane controls' menu, and the terminal's
//! right-click menu with the clipboard ahead of the same actions.

use gpui::{Context, WeakEntity};
use gpui_kit::assets::IconName;

use super::super::actions::title;
use super::super::desk::Desk;
use super::super::ui::keystroke;
use super::super::ui::menu::{MenuItem, MenuPage, MenuSpec};
use super::{on_desk, spec};

/// The pane actions menu, by catalog id; `close` starts its own group.
pub(super) const PANE_MENU: [&str; 4] = ["split-right", "split-down", "view", "close"];

const PANE_ICONS: [IconName; 4] = [
    IconName::PanelRight,
    IconName::PanelBottom,
    IconName::Copy,
    IconName::Close,
];

/// The terminal's clipboard commands, ahead of the pane's actions.
const CLIPBOARD: [(&str, IconName); 3] = [
    ("copy", IconName::Copy),
    ("paste", IconName::ClipboardPaste),
    ("select-all", IconName::ScanText),
];

/// One pane's actions, run on that pane even when another has focus.
pub(super) fn pane_menu(desk: &Desk, placement: &str, cx: &mut Context<Desk>) -> MenuSpec {
    let handle = cx.weak_entity();
    let items = PANE_MENU
        .into_iter()
        .zip(PANE_ICONS)
        .map(|(id, icon)| {
            let item = placement_item(desk, &handle, placement, id, icon);
            if id == "close" { item.gap() } else { item }
        })
        .collect();
    spec("Pane actions", MenuPage::new(items), 240., desk)
}

/// A right click in a terminal that does not take the mouse itself: the
/// clipboard first, then the pane's own actions.
pub(super) fn terminal_menu(desk: &Desk, placement: &str, cx: &mut Context<Desk>) -> MenuSpec {
    let handle = cx.weak_entity();
    let items = CLIPBOARD
        .into_iter()
        .chain(PANE_MENU.into_iter().zip(PANE_ICONS))
        .map(|(id, icon)| {
            let item = placement_item(desk, &handle, placement, id, icon);
            if id == PANE_MENU[0] || id == "close" {
                item.gap()
            } else {
                item
            }
        })
        .collect();
    spec("Terminal", MenuPage::new(items), 240., desk)
}

/// Catalog command `id`, run on `placement` after focusing it.
fn placement_item(
    desk: &Desk,
    handle: &WeakEntity<Desk>,
    placement: &str,
    id: &'static str,
    icon: IconName,
) -> MenuItem {
    let placement = placement.to_owned();
    MenuItem::action(
        title(id),
        on_desk(handle, move |desk, window, cx| {
            if let Some(tab) = desk.active_tab_id() {
                desk.focus_placement(&tab, &placement, window, cx);
            }
            desk.execute(id, window, cx);
        }),
    )
    .icon(icon)
    .chord(desk.bound_chord(id).and_then(keystroke))
    .selector(id)
}
