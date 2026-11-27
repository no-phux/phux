//! The right-click menu over a pane or a tab: the kit's `context_menu`,
//! sized to its longest row.

use rune::{El, PointerPress, el};
use rune_kit::{MenuItem, context_menu as kit_menu};

use super::super::kit_overlay;
use super::super::tokens::Tokens;
use crate::desk::{CONTEXT_MENU, Desk, TAB_MENU};
use crate::keys::{self, Shortcut};

/// Which actions belong together in the menu: a separator falls between
/// groups.
const fn group(action: Shortcut) -> u8 {
    match action {
        // A screenshot goes in as Paste does, so it sits with the edits
        // rather than splitting them with rules of its own.
        Shortcut::Copy | Shortcut::Paste | Shortcut::Screenshot | Shortcut::SelectAll => 0,
        Shortcut::Find => 1,
        Shortcut::SplitRight | Shortcut::SplitDown => 2,
        Shortcut::LockTab | Shortcut::RenameTab => 3,
        _ => 4,
    }
}

/// The right-click menu over a pane or a tab (kit `context_menu`): it opens
/// at the press, flips to stay inside the window, takes the keyboard
/// (arrows, Home and End, Return, Escape) and gives focus back on close.
/// The menu mounts in the kit layer; what this returns marks its place.
pub fn context_menu(desk: &Desk, at: PointerPress, p: &Tokens) -> El {
    let t = p;
    let actions: &[Shortcut] = if desk.pointer.tab_menu {
        &TAB_MENU
    } else {
        &CONTEXT_MENU
    };
    let locked = desk
        .connection
        .tabs
        .list
        .iter()
        .any(|tab| tab.active && tab.locked);
    let mut menu = kit_menu("context", el(), Some(at)).on_close(|d: &mut Desk| d.close_menu());
    let mut last = None;
    let mut widest: f32 = 0.0;
    for &action in actions {
        let Some((title, hint)) = context_words(action, locked) else {
            continue;
        };
        if last.is_some_and(|g| g != group(action)) {
            menu = menu.separator();
        }
        last = Some(group(action));
        widest = widest.max(context_row_width(title, &hint, t));
        menu = menu.item(context_item(action, title, hint, locked));
    }
    // The kit holds a menu at a fixed minimum and truncates labels to fit,
    // so "Screenshot to Terminal" read "Screenshot to…". Size it to its
    // longest row instead, as a native menu fits its items.
    let menu = menu.min_width(widest);
    let mut layer = Vec::new();
    let anchor = menu.build(&t.kit, &mut layer);
    for overlay in layer {
        kit_overlay(overlay);
    }
    anchor
}

/// What a context row says: its title and its chord.
fn context_words(action: Shortcut, locked: bool) -> Option<(&'static str, String)> {
    let command = keys::command(action)?;
    // The lock row says which way it goes for the tab under it.
    let title = match action {
        Shortcut::LockTab if locked => "Unlock Tab",
        Shortcut::LockTab => "Lock Tab",
        _ => command.title,
    };
    Some((title, command.hint()))
}

/// The width a kit menu row needs to show `title` and `hint` whole: the
/// list's inset, the row's padding, the label, a clear gap and the chord,
/// each in the face the kit sets them in.
fn context_row_width(title: &str, hint: &str, t: &Tokens) -> f32 {
    let ty = &t.kit.ty;
    let (label, _) = rune::text::measure(title, &[], ty.ui_compact, None);
    let chord = if hint.is_empty() {
        0.0
    } else {
        rune::text::measure(hint, &[], ty.small_medium, None).0 + t.space.s6
    };
    // List inset (2 a side), row padding (12 leading, 6 trailing).
    let chrome = 2.0 * 2.0 + 12.0 + 6.0;
    (label + chord + chrome).ceil()
}

fn context_item(action: Shortcut, title: &'static str, hint: String, locked: bool) -> MenuItem {
    let item = MenuItem::new(title)
        .danger(matches!(action, Shortcut::CloseTerminal | Shortcut::CloseTab))
        // A locked tab refuses to close: say so before the press.
        .disabled(action == Shortcut::CloseTab && locked)
        .on_select_tx(move |d: &mut Desk, tx| d.context_pick(action, tx));
    if hint.is_empty() {
        item
    } else {
        item.shortcut(hint)
    }
}
