//! Opening, moving through, and running the command palette.

use rune::{App, Key, KeyEvent, MenuItem, Tx};

use super::Desk;
use crate::keys::{self, Shortcut};
use crate::palette::{self, Hit, Target};

/// What right-clicking a pane offers, in order.
pub const CONTEXT_MENU: [Shortcut; 7] = [
    Shortcut::Copy,
    Shortcut::Paste,
    Shortcut::SelectAll,
    Shortcut::Find,
    Shortcut::SplitRight,
    Shortcut::SplitDown,
    Shortcut::CloseTerminal,
];

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
        item!(FILE, CloseTerminal),
        item!(FILE, CloseTab),
        item!(FILE, CloseSession),
        item!(VIEW, ToggleSidebar),
        item!(VIEW, ToggleFullScreen),
        item!(VIEW, ZoomPane),
        item!(VIEW, ZoomIn),
        item!(VIEW, ZoomOut),
        item!(VIEW, ZoomReset),
        item!(VIEW, Inbox),
        item!(VIEW, Palette),
        item!(VIEW, KeyboardShortcuts),
        item!(TERMINAL, SplitRight),
        item!(TERMINAL, SplitDown),
        item!(TERMINAL, Find),
        item!(TERMINAL, FindNext),
        item!(TERMINAL, FindPrevious),
        item!(TERMINAL, FindSelection),
        item!(TERMINAL, NextTab),
        item!(TERMINAL, PreviousTab),
        item!(TERMINAL, NextTerminal),
        item!(TERMINAL, PreviousTerminal),
        item!(TERMINAL, PromptUp),
        item!(TERMINAL, PromptDown),
        item!(TERMINAL, ScrollBottom),
        item!(TERMINAL, JumpToAttention),
    ]
    .into_iter()
    .flatten()
    .fold(app, App::menu_item)
}

impl Desk {
    pub fn close_menu(&mut self) {
        self.pointer.menu = None;
    }

    /// A row of the context menu.
    pub fn context_pick(&mut self, action: Shortcut, tx: &mut Tx) {
        self.close_menu();
        self.shortcut(action, tx);
    }

    /// A menu pick: the same rule as the chord.
    fn menu_command(&mut self, action: Shortcut, tx: &mut Tx) {
        match self.modal() {
            Some(modal) => self.modal_command(modal, action, tx),
            None => self.shortcut(action, tx),
        }
    }

    pub fn toggle_palette(&mut self) {
        if self.palette_open {
            self.close_palette();
            return;
        }
        self.palette_open = true;
        self.palette_picked = 0;
        self.palette_query.clear();
        // Ask once; the runtime keeps the field's focus from here.
        self.palette_query.focused = true;
    }

    /// The palette listing every chord; picking one runs it.
    pub(super) fn show_shortcuts(&mut self) {
        self.close_palette();
        self.toggle_palette();
        self.palette_shortcuts = true;
    }

    pub fn close_palette(&mut self) {
        self.palette_open = false;
        self.palette_shortcuts = false;
        self.palette_query.clear();
        self.palette_query.focused = false;
    }

    /// The palette's rows for the current query, best first.
    pub fn palette_hits(&self) -> Vec<Hit> {
        let items = if self.palette_shortcuts {
            palette::shortcut_items()
        } else {
            palette::items(self.topology.as_ref())
        };
        palette::rank(items, &self.palette_query.text)
    }

    /// Arrow keys and Escape inside the palette's field.
    pub fn palette_key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        // Only the rendered rows can be highlighted.
        let count = self.palette_hits().len().min(palette::ROWS);
        match event.key {
            Key::Down | Key::Tab if count > 0 => {
                self.palette_picked = (self.palette_picked + 1) % count
            }
            Key::Up if count > 0 => self.palette_picked = (self.palette_picked + count - 1) % count,
            Key::Escape => {
                self.close_palette();
                tx.blur();
            }
            _ => return false,
        }
        tx.scroll_into_view(format!("palette-row-{}", self.palette_picked));
        true
    }

    /// Run the highlighted row, or the row at `index`.
    pub fn palette_run(&mut self, index: Option<usize>, tx: &mut Tx) {
        let hits = self.palette_hits();
        let picked = self.palette_picked.min(hits.len().saturating_sub(1));
        let Some(hit) = hits.get(index.unwrap_or(picked)).cloned() else {
            return;
        };
        self.close_palette();
        tx.blur();
        match hit.item.target {
            Target::Terminal { id, session } => self.show(id, &session),
            Target::Session(name) => self.show_session(&name),
            Target::Action(action) => self.shortcut(action, tx),
        }
    }

    /// Select a session and show its focused terminal.
    pub fn show_session(&mut self, name: &str) {
        let Some(topology) = &self.topology else {
            return;
        };
        let Some(session) = topology.sessions.iter().find(|s| s.name == name) else {
            return;
        };
        let panes = || topology.panes.iter().filter(|p| p.session_id == session.id);
        let Some(pane) = panes()
            .find(|p| p.terminal_id == topology.focused_pane)
            .or_else(|| panes().next())
        else {
            return;
        };
        let (id, session) = (pane.terminal_id.clone(), pane.session_name.clone());
        self.show(id, &session);
    }
}
