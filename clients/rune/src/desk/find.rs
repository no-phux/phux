//! Find in a terminal's history: a field over the focused pane, every match
//! the engine registers, and the current one selected and scrolled to.
//!
//! Search starts from the newest output, so ↩ walks back in time and ⇧↩
//! forward, as in iTerm and Ghostty.

use phux_client_runtime::ViewId;
use phux_protocol::ids::ResourceId;
use rune::{Key, KeyEvent, TextEdit, Tx};

use super::Desk;
use super::input::selection::Selected;

pub struct Find {
    pub terminal: ResourceId,
    view: ViewId,
    /// Matches in document order, as start and end anchors.
    matches: Vec<(u64, u64)>,
    /// The current match; `None` with no matches.
    pub index: Option<usize>,
    pub case_sensitive: bool,
    /// A bound (the match cap or the anchor budget) hid more matches.
    truncated: bool,
}

impl Find {
    /// "3 of 12", "no matches", or nothing before a query.
    pub fn status(&self, query: &str) -> String {
        if query.is_empty() {
            return String::new();
        }
        let Some(index) = self.index else {
            return "no matches".to_owned();
        };
        // Counted from the newest match, the way the user walks them.
        let total = self.matches.len();
        let more = if self.truncated { "+" } else { "" };
        format!("{} of {total}{more}", total - index)
    }
}

impl Desk {
    /// Open find on the focused pane, or focus its field again.
    pub fn open_find(&mut self) {
        let Some((terminal, view)) = self.shown.as_ref().map(|s| (s.terminal.clone(), s.view))
        else {
            return;
        };
        // Reopening keeps the query, selected so typing replaces it.
        self.find_query.focused = true;
        self.find_query.set_selection(0, self.find_query.text.len());
        if self.find.as_ref().is_some_and(|f| f.terminal == terminal) {
            return;
        }
        self.close_find();
        self.find = Some(Find {
            terminal,
            view,
            matches: Vec::new(),
            index: None,
            case_sensitive: false,
            truncated: false,
        });
    }

    /// Search for the selected text, as macOS's ⌘E does.
    pub fn find_selection(&mut self) {
        let selected = match self.selected() {
            Selected::Text(text) => text,
            Selected::Nothing | Selected::TooLarge => String::new(),
        };
        let Some(line) = selected.lines().find(|l| !l.trim().is_empty()) else {
            return self.open_find();
        };
        let line: String = line.chars().take(512).collect();
        self.open_find();
        self.find_query = TextEdit::new(line);
        self.find_query.focused = true;
        self.run_search();
    }

    /// Search again for the field's text, landing on the newest match.
    pub fn run_search(&mut self) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(find) = &mut self.find else { return };
        let query = self.find_query.text.clone();
        let Some(engine) = client.engine() else {
            return;
        };
        // An empty query frees the last search's anchors, which history
        // scrolling draws on too.
        let found = if query.is_empty() {
            let _ = engine.clear_view_search(find.view);
            None
        } else {
            engine
                .search_view_bounded(find.view, query, find.case_sensitive)
                .ok()
        };
        find.truncated = found.as_ref().is_some_and(|f| f.truncated);
        find.matches = found
            .map(|f| f.matches.into_iter().map(|m| (m.start, m.end)).collect())
            .unwrap_or_default();
        find.index = find.matches.len().checked_sub(1);
        self.show_match();
    }

    /// Older (`-1`) or newer (`1`) match, wrapping. With none, searches again.
    pub fn step_find(&mut self, delta: isize) {
        let Some(find) = &mut self.find else {
            return self.open_find();
        };
        let Some(index) = find.index else {
            return self.run_search();
        };
        let len = find.matches.len() as isize;
        find.index = Some((index as isize + delta).rem_euclid(len) as usize);
        self.show_match();
    }

    pub fn toggle_find_case(&mut self) {
        if let Some(find) = &mut self.find {
            find.case_sensitive = !find.case_sensitive;
        }
        self.run_search();
    }

    fn show_match(&mut self) {
        let (Some(client), Some(find)) = (&self.client, &self.find) else {
            return;
        };
        let Some(engine) = client.engine() else {
            return;
        };
        match find.index.and_then(|i| find.matches.get(i)) {
            Some(&(start, end)) => {
                let _ = engine.set_view_selection(find.view, start, end, false);
                let _ = client.pin_viewport_view(find.view, start);
            }
            None => {
                let _ = engine.clear_view_selection(find.view);
            }
        }
        self.generation += 1;
    }

    /// Close find, leaving the history where the last match put it.
    pub fn close_find(&mut self) {
        let Some(find) = self.find.take() else { return };
        self.find_query.focused = false;
        if let Some(engine) = self.client.as_ref().and_then(|c| c.engine()) {
            let _ = engine.clear_view_selection(find.view);
            let _ = engine.clear_view_search(find.view);
        }
        self.generation += 1;
    }

    /// Keys inside the find field.
    pub fn find_key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        match event.key {
            Key::Enter if event.mods.shift => self.step_find(1),
            Key::Enter => self.step_find(-1),
            Key::Up => self.step_find(-1),
            Key::Down => self.step_find(1),
            Key::Escape => {
                self.close_find();
                tx.blur();
            }
            _ => return false,
        }
        true
    }

    /// Find belongs to one view; it closes when that pane loses its view.
    pub(super) fn keep_find(&mut self) {
        let alive = self.find.as_ref().is_none_or(|find| {
            self.view_of(&find.terminal) == Some(find.view)
                && self
                    .shown
                    .as_ref()
                    .is_some_and(|s| s.terminal == find.terminal)
        });
        if !alive {
            self.close_find();
        }
    }
}
