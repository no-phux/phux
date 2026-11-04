//! Opening, moving through, and running the command palette.

use phux_client_runtime::Client;
use phux_protocol::ids::ResourceId;
use rune::{App, Key, KeyEvent, MenuItem, Tx};

use super::{Desk, NoticeKind};
use crate::keys::{self, Shortcut};
use crate::palette::{self, Hit, Mode, Target};

/// A path search on the focused pane's host.
#[derive(Debug, Clone, PartialEq)]
pub struct PathSearch {
    /// The pane a picked path goes to.
    pub terminal: ResourceId,
    /// Its folder: the search's root, and what paths read relative to.
    pub origin: String,
    /// The request whose answer counts; later ones replace it.
    pending: u32,
    /// Absolute paths in the server's order, and whether each is a folder.
    pub rows: Vec<(String, bool)>,
    /// Why there is nothing to show, or what the answer left out.
    pub note: Option<String>,
}

impl PathSearch {
    pub const fn searching(&self) -> bool {
        self.pending != 0
    }
}

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
        item!(FILE, SwitchSession),
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
        item!(VIEW, EssentialShortcuts),
        item!(TERMINAL, SplitRight),
        item!(TERMINAL, SplitDown),
        item!(TERMINAL, Find),
        item!(TERMINAL, FindNext),
        item!(TERMINAL, FindPrevious),
        item!(TERMINAL, FindSelection),
        item!(TERMINAL, InsertPath),
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
        self.open_palette(Mode::Shortcuts);
    }

    /// The palette listing the keys worth learning first.
    pub(super) fn show_essentials(&mut self) {
        self.open_palette(Mode::Essentials);
    }

    /// The palette over everything else, listing `mode`.
    pub(super) fn open_palette(&mut self, mode: Mode) {
        self.close_settings();
        self.close_inbox();
        self.close_palette();
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

    /// Paths under the focused pane's folder, searched on its host.
    pub(super) fn open_paths(&mut self) {
        let Some(terminal) = self.shown.as_ref().map(|s| s.terminal.clone()) else {
            return;
        };
        let Some(origin) = self.focused_cwd() else {
            return self.say(
                NoticeKind::Error,
                "This terminal hasn't said which folder it is in.",
            );
        };
        self.open_palette(Mode::Paths);
        self.paths = Some(PathSearch {
            terminal,
            origin,
            pending: 0,
            rows: Vec::new(),
            note: None,
        });
        self.search_paths();
    }

    /// Ask the host for the paths the query names; an empty query lists the
    /// folder itself.
    fn search_paths(&mut self) {
        let (Some(client), Some(search)) = (&self.client, self.paths.as_mut()) else {
            return;
        };
        let query = self.palette_query.text.trim().to_owned();
        let host = search.terminal.host().cloned();
        let recursive = !query.is_empty();
        search.pending = client.path_query(search.origin.clone(), query, recursive, host);
        search.rows.clear();
        search.note =
            (search.pending == 0).then(|| "Path search is unavailable on this server.".to_owned());
    }

    /// Answers to the path search. True when the list changed.
    pub(super) fn take_paths(&mut self, client: &Client) -> bool {
        let answers = client.take_path_answers();
        let Some(search) = self.paths.as_mut() else {
            return false;
        };
        let Some(answer) = answers
            .into_iter()
            .rfind(|a| a.request_id == search.pending)
        else {
            return false;
        };
        search.pending = 0;
        search.rows = answer
            .rows
            .iter()
            .map(|row| {
                let folder = row.kind == phux_client_runtime::control::PathMatchKind::Directory;
                (row.path.clone(), folder)
            })
            .collect();
        search.note = match (answer.failure, answer.status) {
            (Some(_), _) if !answer.message.is_empty() => Some(answer.message),
            (Some(_), _) => Some("The host couldn't search here.".to_owned()),
            (None, Some(phux_client_runtime::control::PathSearchStatus::Truncated)) => {
                Some("More match; keep typing to narrow them.".to_owned())
            }
            (None, Some(phux_client_runtime::control::PathSearchStatus::Warming)) => {
                Some("The host is still indexing; try again shortly.".to_owned())
            }
            _ => None,
        };
        self.palette_picked = 0;
        true
    }

    /// The query changed: back to the first row, and a new search or preview.
    pub fn palette_edited(&mut self) {
        self.palette_picked = 0;
        match self.palette_mode {
            Mode::Paths => self.search_paths(),
            Mode::Themes => self.preview_picked(),
            _ => {}
        }
    }

    /// Show the highlighted theme without keeping it.
    fn preview_picked(&mut self) {
        if self.palette_mode != Mode::Themes {
            return;
        }
        let hits = self.palette_hits();
        if let Some(Target::Theme(id)) = hits.get(self.palette_picked).map(|h| &h.item.target) {
            self.settings.theme = id.and_then(crate::theme::find);
            self.apply_theme();
        }
    }

    pub fn close_palette(&mut self) {
        self.palette_open = false;
        self.palette_mode = Mode::All;
        self.palette_query.clear();
        self.palette_query.focused = false;
        self.paths = None;
        if let Some(before) = self.theme_before.take() {
            self.settings.theme = before;
            self.apply_theme();
        }
    }

    /// The palette's rows for the current query, best first.
    pub fn palette_hits(&self) -> Vec<Hit> {
        let topology = self.topology.as_ref();
        let query = &self.palette_query.text;
        match self.palette_mode {
            Mode::All => palette::rank(palette::items(topology), query),
            Mode::Shortcuts => palette::rank(palette::shortcut_items(), query),
            Mode::Essentials => palette::rank(palette::essential_items(), query),
            Mode::Themes => palette::rank(palette::theme_items(), query),
            Mode::Sessions => {
                let mut hits = palette::rank(palette::session_items(topology), query);
                let new = palette::new_session_item(topology, query);
                hits.extend(new.map(|item| Hit {
                    item,
                    score: 0,
                    matched: Vec::new(),
                }));
                hits
            }
            // The host already matched them, best first.
            Mode::Paths => {
                let Some(search) = &self.paths else {
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
        let shown = self.shown.as_ref()?;
        let frame = self.client.as_ref()?.acquire_view(shown.view)?;
        let [x, y] = *self.origins.get(&shown.terminal)?;
        let m = self.metrics();
        let (col, row) = (f32::from(frame.cursor.col), f32::from(frame.cursor.row));
        Some([x + col * m.cell_w, y + (row + 1.0) * m.cell_h])
    }

    /// Put a picked path at its pane's prompt as one shell word.
    fn insert_path(&mut self, path: &str, search: PathSearch) {
        let still_there = self
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
        if matches!(hit.item.target, Target::Theme(_)) {
            // Chosen, so closing doesn't put the old one back.
            self.theme_before = None;
        }
        let search = self.paths.take();
        self.close_palette();
        tx.blur();
        match hit.item.target {
            Target::Terminal { id, session } => self.show(id, &session),
            Target::Session(name) => self.show_session(&name),
            Target::Action(action) => self.shortcut(action, tx),
            Target::Theme(id) => self.choose_theme(id.and_then(crate::theme::find)),
            Target::NewSession(name) => self.create_session(name),
            Target::Path(path) => {
                if let Some(search) = search {
                    self.insert_path(&path, search);
                }
            }
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
