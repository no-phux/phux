//! Opening, moving through, and running the command palette.

use rune::{App, Key, KeyEvent, MenuItem, Tx};

use super::{Desk, NoticeKind};
use crate::keys::{self, Shortcut};
use crate::palette::{self, Hit, Mode, Target};

mod paths;
pub use paths::PathSearch;

/// What right-clicking a tab offers, in order.
pub const TAB_MENU: [Shortcut; 3] = [Shortcut::LockTab, Shortcut::RenameTab, Shortcut::CloseTab];

/// What right-clicking a pane offers, in order.
pub const CONTEXT_MENU: [Shortcut; 8] = [
    Shortcut::Copy,
    Shortcut::Paste,
    Shortcut::Screenshot,
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

impl Desk {
    pub fn close_menu(&mut self) {
        self.pointer.menu = None;
        self.pointer.tab_menu = false;
    }

    /// The right-click menu of the tab at `index`, which it selects first so
    /// the rows act on the active tab.
    pub fn open_tab_menu(&mut self, index: usize, at: rune::PointerPress) {
        self.select_tab(index);
        self.pointer.menu = Some(at);
        self.pointer.tab_menu = true;
    }

    /// A row of the context menu.
    pub fn context_pick(&mut self, action: Shortcut, tx: &mut Tx) {
        self.close_menu();
        // Over a popup, only what its keys may do.
        match self.modal() {
            Some(modal @ super::input::Modal::Popup) => self.modal_command(modal, action, tx),
            _ => self.shortcut(action, tx),
        }
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
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if self.phone.is_none() {
            self.refresh_plugin_actions();
        }
        self.palette_picked = 0;
        self.palette_query.clear();
        // Ask once; the runtime keeps the field's focus from here.
        self.palette_query.focused = true;
    }

    /// The palette listing every chord; picking one runs it.
    pub(super) fn show_shortcuts(&mut self) {
        self.open_palette(Mode::Shortcuts);
    }

    /// The palette listing the keys worth learning first.
    pub(super) fn show_essentials(&mut self) {
        self.open_palette(Mode::Essentials);
    }

    /// The palette over everything else, listing `mode`.
    pub(super) fn open_palette(&mut self, mode: Mode) {
        self.close_inbox();
        self.close_palette();
        self.close_settings();
        self.toggle_palette();
        self.palette_mode = mode;
    }

    /// The themes, the current one highlighted; moving the highlight shows
    /// each one until Return keeps it or Escape puts the old one back.
    pub(super) fn open_themes(&mut self) {
        self.open_palette(Mode::Themes);
        let current = self.settings.theme;
        self.theme_before = Some(current);
        let at = crate::theme::all().position(|t| Some(t) == current);
        self.palette_picked = at.map_or(0, |i| i + 1);
    }

    /// The query changed: back to the first row, and a new search or preview.
    pub fn palette_edited(&mut self) {
        self.palette_picked = 0;
        match self.palette_mode {
            Mode::Paths => self.search_paths(),
            Mode::Themes | Mode::All => self.preview_picked(),
            _ => {}
        }
    }

    /// Show the highlighted theme without keeping it. The general palette
    /// shows the theme it opened with again once the highlight leaves the
    /// themes.
    fn preview_picked(&mut self) {
        let general = self.palette_mode == Mode::All;
        if !general && self.palette_mode != Mode::Themes {
            return;
        }
        let hits = self.palette_hits();
        let theme = match hits.get(self.palette_picked).map(|h| &h.item.target) {
            Some(Target::Theme(id)) => {
                self.theme_before.get_or_insert(self.settings.theme);
                id.and_then(crate::theme::find)
            }
            _ => match self.theme_before {
                Some(before) if general => before,
                _ => return,
            },
        };
        if theme != self.settings.theme {
            self.settings.theme = theme;
            self.apply_theme();
        }
    }

    pub fn close_palette(&mut self) {
        self.palette_open = false;
        self.palette_mode = Mode::All;
        self.palette_query.clear();
        self.palette_query.focused = false;
        self.connection.paths = None;
        if let Some(before) = self.theme_before.take() {
            self.settings.theme = before;
            self.apply_theme();
        }
        if std::mem::take(&mut self.settings_ui.return_from_themes) {
            self.settings_open = true;
        }
    }

    /// The tabs of every session that has two or more, for the palette's
    /// headings; a session whose arrangement hasn't arrived has none.
    fn palette_tabs(&self, listed: &[phux_protocol::ids::ResourceId]) -> Vec<palette::TabGroup> {
        let (Some(client), Some(topology)) = (&self.connection.client, &self.connection.topology)
        else {
            return Vec::new();
        };
        let org = client.organization();
        let mut out = Vec::new();
        for session in &topology.sessions {
            let doc = session.key.and_then(|key| org.arrangement_document(&key));
            let tabs = doc.map_or_else(Vec::new, |(_, _, doc, _)| super::tabs::list(&doc, listed));
            if tabs.len() < 2 {
                continue;
            }
            out.extend(tabs.iter().map(|tab| palette::TabGroup {
                title: self.tab_label(tab),
                session: session.name.clone(),
                panes: tab.resources.clone(),
            }));
        }
        out
    }

    /// The palette's rows for the current query, best first.
    pub fn palette_hits(&self) -> Vec<Hit> {
        let topology = self.connection.topology.as_ref();
        let query = &self.palette_query.text;
        match self.palette_mode {
            Mode::All if self.phone.is_some() => {
                let native = self.native.is_some();
                let items = palette::phone::items(
                    topology,
                    self.connection.reorder.order(),
                    native,
                    self.resource_items(),
                );
                palette::rank(items, query)
            }
            Mode::All => {
                // The quick terminal's own action leads; then terminals,
                // sessions and commands; the mirrors, which few reach for,
                // wait at the end until a query names them.
                let mut items: Vec<palette::Item> = self
                    .quick
                    .is_some()
                    .then(|| palette::Item {
                        title: "Move to a Tab".to_owned(),
                        detail: "into the main window".to_owned(),
                        kind: "action",
                        target: Target::MoveToTab,
                    })
                    .into_iter()
                    .collect();
                let shown = self.addressable();
                items.extend(palette::items(
                    topology,
                    &shown,
                    self.connection.reorder.order(),
                ));
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                items.extend(self.plugin_actions.items());
                items.extend(self.resource_items());
                let listed: Vec<_> = shown.into_iter().map(|r| r.id).collect();
                palette::rank_grouped(items, &self.palette_tabs(&listed), query)
            }
            Mode::Mirror(source) => self.mirror_items(source, query),
            Mode::Screens => self.screen_hits(query),
            Mode::Shortcuts => palette::rank(palette::shortcut_items(), query),
            Mode::Essentials => palette::rank(palette::essential_items(), query),
            Mode::Playground => palette::rank(palette::action_items(keys::PLAYGROUND), query),
            Mode::Themes => palette::rank(palette::theme_items(), query),
            Mode::Sessions => {
                let mut hits = palette::rank(
                    palette::session_items(topology, self.connection.reorder.order()),
                    query,
                );
                let new = palette::new_session_item(topology, query);
                let new = new.into_iter().chain(palette::new_worktree_item(query));
                hits.extend(new.map(|item| Hit {
                    item,
                    score: 0,
                    matched: Vec::new(),
                }));
                hits
            }
            Mode::Popup => {
                let item = super::popup::item(query);
                let hit = item.map(|item| Hit {
                    item,
                    score: 0,
                    matched: Vec::new(),
                });
                hit.into_iter().collect()
            }
            // The host already matched them, best first.
            Mode::Paths => {
                let Some(search) = &self.connection.paths else {
                    return Vec::new();
                };
                let rows = search.rows.iter();
                let items =
                    rows.map(|(path, folder)| palette::path_item(path, *folder, &search.origin));
                palette::rank(items.collect(), "")
            }
        }
    }

    /// The point under the focused pane's cursor, in window points, where
    /// the path picker opens.
    pub fn cursor_point(&self) -> Option<[f32; 2]> {
        let shown = self.connection.shown.as_ref()?;
        let frame = self.connection.client.as_ref()?.acquire_view(shown.view?)?;
        let [x, y] = *self.connection.origins.get(&shown.resource)?;
        let m = self.metrics();
        let (col, row) = (f32::from(frame.cursor.col), f32::from(frame.cursor.row));
        Some([x + col * m.cell_w, y + (row + 1.0) * m.cell_h])
    }

    /// Put a picked path at its pane's prompt as one shell word.
    fn insert_path(&mut self, path: &str, search: PathSearch) {
        let still_there = self
            .connection
            .topology
            .as_ref()
            .is_some_and(|t| t.pane(&search.terminal).is_some());
        let word = crate::paths::shell_word(path, &search.origin);
        match word.filter(|_| still_there) {
            Some(word) => self.paste(&search.terminal, &word),
            None => self.say(
                NoticeKind::Error,
                "That pane is gone; nothing was inserted.",
            ),
        }
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
        self.preview_picked();
        true
    }

    /// Run the highlighted row, or the row at `index`.
    pub fn palette_run(&mut self, index: Option<usize>, tx: &mut Tx) {
        let hits = self.palette_hits();
        let picked = self.palette_picked.min(hits.len().saturating_sub(1));
        let Some(hit) = hits.get(index.unwrap_or(picked)).cloned() else {
            return;
        };
        self.commit_palette(hit.item.target, tx);
    }

    /// Pointer events name the row drawn, never the row at a now-stale index.
    pub fn palette_run_target(&mut self, target: Target, tx: &mut Tx) {
        if self.palette_open
            && self
                .palette_hits()
                .iter()
                .any(|hit| hit.item.target == target)
        {
            self.commit_palette(target, tx);
        }
    }

    fn commit_palette(&mut self, target: Target, tx: &mut Tx) {
        if matches!(target, Target::Theme(_)) {
            // Chosen, so closing doesn't put the old one back.
            self.theme_before = None;
        }
        let search = self.connection.paths.take();
        self.close_palette();
        tx.blur();
        self.run_palette_target(target, search, tx);
    }

    fn run_palette_target(&mut self, target: Target, search: Option<PathSearch>, tx: &mut Tx) {
        match target {
            Target::Mirror(source) => self.open_palette(Mode::Mirror(source)),
            Target::StartMirror(source, target) => self.ask_mirror(source, target),
            Target::Screens => self.open_screens(),
            Target::Screen(host, target) => self.start_screen(host, target),
            Target::Terminal { id, session } => self.show(id, &session),
            Target::Session(name) => self.show_session(&name),
            Target::Action(action) => self.shortcut(action, tx),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Target::Extension(action) => self.run_plugin_action(action),
            Target::MoveToTab => self.move_to_tab(),
            Target::Theme(id) => self.choose_theme(id.and_then(crate::theme::find)),
            Target::NewSession(name) => self.create_session(name),
            Target::NewWorktree(branch) => self.new_worktree(&branch),
            Target::Popup(line) => self.open_popup(&line),
            Target::Path(path) => {
                if let Some(search) = search {
                    self.insert_path(&path, search);
                }
            }
        }
    }

    /// Select a session and show its focused terminal.
    pub fn show_session(&mut self, name: &str) {
        let Some(topology) = &self.connection.topology else {
            return;
        };
        let Some(session) = topology.sessions.iter().find(|s| s.name == name) else {
            return;
        };
        let panes = || topology.panes.iter().filter(|p| p.session_id == session.id);
        let pane = panes()
            .find(|p| p.terminal_id == topology.focused_pane)
            .or_else(|| panes().next());
        let id = pane
            .map(|p| p.terminal_id.clone())
            .or_else(|| self.default_document(session.id));
        let Some(id) = id else {
            if let Some(client) = &self.connection.client {
                client.attach_session(phux_protocol::wire::frame::AttachTarget::ByName(
                    name.to_owned(),
                ));
                self.connection.pending_session = Some(name.to_owned());
                self.connection.wanted = None;
            }
            return;
        };
        self.show(id, name);
    }
}
