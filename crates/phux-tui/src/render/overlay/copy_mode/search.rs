//! Copy-mode search: the query being typed, the request handed to the
//! dispatcher, and the hits it brings back.

use phux_protocol::input::key::{KeyEvent, PhysicalKey};

use super::super::selection::SearchMatch;
use super::super::{OverlayCommand, SelectionMode};
use super::CopyModeOverlay;

/// A search the overlay asks the dispatcher to run: find `needle` after
/// (or, `backward`, before) the pane-local cursor, wrapping around.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopySearchRequest {
    /// The text to find (the engine's case policy applies).
    pub needle: String,
    /// Search toward older output (`?`, `N` after `/`).
    pub backward: bool,
    /// Pane-local cursor row the search starts from.
    pub cursor_row: u16,
    /// Pane-local cursor column the search starts from.
    pub cursor_col: u16,
    /// The pane's visible rows: a hit outside them scrolls into view.
    pub pane_rows: u16,
}

/// What a [`CopySearchRequest`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopySearchResult {
    /// Every loaded hit in document order, the one jumped to, and the
    /// document row now at the top of the (possibly scrolled) viewport.
    Found {
        /// All hits, oldest first.
        matches: Vec<SearchMatch>,
        /// Index of the hit the cursor moved to.
        current: usize,
        /// Document row of viewport row 0 after the jump.
        top: u32,
    },
    /// Nothing loaded matches.
    NotFound,
}

/// The last search and what it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SearchState {
    pub(super) needle: String,
    backward: bool,
    matches: Vec<SearchMatch>,
    current: Option<usize>,
}

/// What the painter shows for copy-mode search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopySearchView<'a> {
    /// Every hit, oldest first, in document rows.
    pub matches: &'a [SearchMatch],
    /// The hit the selection covers, if any.
    pub current: Option<usize>,
}

impl CopyModeOverlay {
    /// Adopt a search result: jump the cursor to the current hit and select
    /// it, so Enter copies it and the painter inverts it.
    pub fn apply_search_result(&mut self, result: CopySearchResult) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let CopySearchResult::Found {
            matches,
            current,
            top,
        } = result
        else {
            search.matches.clear();
            search.current = None;
            return;
        };
        let Some(hit) = matches.get(current).copied() else {
            return;
        };
        search.matches = matches;
        search.current = Some(current);
        let local = |row: u32| u16::try_from(row.saturating_sub(top)).unwrap_or(u16::MAX);
        let max_row = self.pane_rows.saturating_sub(1);
        let max_col = self.pane_cols.saturating_sub(1);
        self.cursor_row = local(hit.start_row).min(max_row);
        self.cursor_col = hit.start_col.min(max_col);
        self.anchor_row = local(hit.end_row).min(max_row);
        self.anchor_col = hit.end_col.min(max_col);
        self.mode = SelectionMode::Char;
        self.selecting = false;
        self.mouse_anchor_screen = None;
        self.mouse_anchor_viewport_row = None;
    }

    /// The search hits for the painter, or `None` before any search.
    #[must_use]
    pub fn search_view(&self) -> Option<CopySearchView<'_>> {
        self.search.as_ref().map(|search| CopySearchView {
            matches: &search.matches,
            current: search.current,
        })
    }

    /// The search part of the status strip: the line being typed, or the
    /// last search with its position (`/foo 2/7`, `/foo: no match`).
    #[must_use]
    pub fn search_status(&self) -> Option<String> {
        if let Some((edit, backward)) = &self.search_input {
            let prompt = if *backward { '?' } else { '/' };
            return Some(format!("{prompt}{}_", edit.as_str()));
        }
        let search = self.search.as_ref()?;
        let prompt = if search.backward { '?' } else { '/' };
        Some(search.current.map_or_else(
            || format!("{prompt}{}: no match", search.needle),
            |index| {
                format!(
                    "{prompt}{} {}/{}",
                    search.needle,
                    index + 1,
                    search.matches.len()
                )
            },
        ))
    }

    /// A search request for `needle` from the cursor, remembered for `n`/`N`.
    pub(super) fn start_search(&mut self, needle: String, backward: bool) -> OverlayCommand {
        self.search = Some(SearchState {
            needle: needle.clone(),
            backward,
            matches: Vec::new(),
            current: None,
        });
        self.search_request(needle, backward)
    }

    pub(super) const fn search_request(&self, needle: String, backward: bool) -> OverlayCommand {
        OverlayCommand::Search(CopySearchRequest {
            needle,
            backward,
            cursor_row: self.cursor_row,
            cursor_col: self.cursor_col,
            pane_rows: self.pane_rows,
        })
    }

    /// `n` (`reverse` false) or `N`: the last search again, in its own
    /// direction or the opposite one. Nothing searched yet is a no-op.
    pub(super) fn repeat_search(&self, reverse: bool) -> OverlayCommand {
        self.search.as_ref().map_or(OverlayCommand::Stay, |search| {
            self.search_request(search.needle.clone(), search.backward != reverse)
        })
    }

    /// A key while the search line is open: edits, Enter runs (an empty
    /// line repeats the last search), Esc closes the line but not copy-mode.
    pub(super) fn handle_search_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        let Some((edit, backward)) = self.search_input.as_mut() else {
            return OverlayCommand::Stay;
        };
        match key.key {
            PhysicalKey::Escape => {
                self.search_input = None;
                OverlayCommand::Stay
            }
            PhysicalKey::Enter | PhysicalKey::NumpadEnter => {
                let needle = edit.as_str().to_owned();
                let backward = *backward;
                self.search_input = None;
                if needle.is_empty() {
                    return self.repeat_search(false);
                }
                self.start_search(needle, backward)
            }
            _ => {
                edit.handle_key(key);
                OverlayCommand::Stay
            }
        }
    }
}
