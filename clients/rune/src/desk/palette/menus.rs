//! The native menu bar, built from the command table.

use rune::{App, MenuItem, Tx};

use super::super::Desk;
use crate::keys::{self, Shortcut};

/// A menu item running `action`. Menu items take a plain `fn`, so each row
/// names its action twice over here rather than capturing it.
macro_rules! item {
    ($menu:expr, $action:ident) => {
        item($menu, Shortcut::$action, |desk, tx| {
            desk.menu_command(Shortcut::$action, tx);
        })
    };
}

/// One native menu row: its words and chord from the command table. Keys go
/// to the window first, so a chord behaves the same with the menu bar there.
fn item(
    menu: Option<&str>,
    action: Shortcut,
    run: fn(&mut Desk, &mut Tx),
) -> Option<MenuItem<Desk>> {
    let command = keys::command(action)?;
    let item = MenuItem::new(command.title, run)
        .id(format!("{action:?}"))
        .keys_first();
    let item = match menu {
        Some(menu) => item.menu(menu),
        None => item,
    };
    let chord = command.mac.first().map(|c| c.accelerator().parse());
    Some(match chord {
        Some(Ok(chord)) => item.key_equivalent(chord),
        _ => item,
    })
}

/// The menu bar, on top of the Edit and Window menus Rune adds. Other
/// platforms have no native menu bar here; the palette reaches everything.
pub fn with_menus(app: App<Desk>) -> App<Desk> {
    const FILE: Option<&str> = Some("File");
    const VIEW: Option<&str> = Some("View");
    const TERMINAL: Option<&str> = Some("Terminal");
    if !cfg!(target_os = "macos") {
        return app;
    }
    [
        item!(None, Settings),
        item!(None, CheckForUpdates),
        item!(FILE, NewTerminal),
        item!(FILE, NewSession),
        item!(FILE, SwitchSession),
        item!(FILE, NextSession),
        item!(FILE, PreviousSession),
        item!(FILE, CloseTerminal),
        item!(FILE, CloseTab),
        item!(FILE, LockTab),
        item!(FILE, CloseSession),
        item!(VIEW, ToggleSidebar),
        item!(VIEW, ToggleFullScreen),
        item!(VIEW, ZoomPane),
        item!(VIEW, ZoomIn),
        item!(VIEW, ZoomOut),
        item!(VIEW, ZoomReset),
        item!(VIEW, InterfaceBigger),
        item!(VIEW, InterfaceSmaller),
        item!(VIEW, InterfaceReset),
        item!(VIEW, CopyConfig),
        item!(VIEW, RestoreSessions),
        item!(VIEW, ReviewRawRelaunch),
        item!(VIEW, Inbox),
        item!(VIEW, Playground),
        item!(VIEW, Palette),
        item!(VIEW, KeyboardShortcuts),
        item!(VIEW, EssentialShortcuts),
        item!(Some("Help"), GetStarted),
        item!(TERMINAL, SplitRight),
        item!(TERMINAL, SplitDown),
        item!(TERMINAL, Find),
        item!(TERMINAL, FindNext),
        item!(TERMINAL, FindPrevious),
        item!(TERMINAL, FindSelection),
        item!(TERMINAL, CopyMode),
        item!(TERMINAL, InsertPath),
        item!(TERMINAL, Screenshot),
        item!(TERMINAL, QuickLook),
        item!(TERMINAL, NextTab),
        item!(TERMINAL, PreviousTab),
        item!(TERMINAL, NextTerminal),
        item!(TERMINAL, PreviousTerminal),
        item!(TERMINAL, NextSlotTab),
        item!(TERMINAL, PreviousSlotTab),
        item!(TERMINAL, NextTerminalInSession),
        item!(TERMINAL, PreviousTerminalInSession),
        item!(TERMINAL, PromptUp),
        item!(TERMINAL, PromptDown),
        item!(TERMINAL, ScrollToBottom),
        item!(TERMINAL, JumpToAttention),
    ]
    .into_iter()
    .flatten()
    .fold(app, App::menu_item)
}
