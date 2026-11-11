//! Keyboard copy mode: a cursor over the focused pane, moved, extended and
//! copied with the TUI's vi keys (`h j k l`, `w b e`, `0 ^ $`, `g G`,
//! `H M L`, `C-u C-d C-b C-f`, `v V C-v`, `y`).
//!
//! The cursor is a one-cell selection in the engine, so it paints as any
//! selection does. A selection's fixed end is three engine anchors on the
//! cell it started from (the cell and both ends of its row), which stay on
//! their text while the viewport scrolls.

use phux_client_core::motion::{RowMotion, row_motion_target, word_span};
use phux_client_runtime::ViewId;
use phux_client_runtime::engine::{EngineDocumentPoint, Scroll};
use phux_client_runtime::publication::GridFrame;
use phux_protocol::ids::ResourceId;
use rune::{Key, KeyEvent, Tx};

use super::{Desk, NoticeKind};

/// What the user is told on the way in.
const HINT: &str = "Copy mode: h j k l move, v selects, y copies, Esc leaves.";

/// `EngineDocumentPoint::space` for the visible viewport.
const VIEWPORT: u32 = 1;

/// How a selection grows from its fixed end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Char,
    Line,
    Block,
}

/// A selection's fixed end: where `v` was pressed.
#[derive(Debug, Clone, Copy)]
struct Pin {
    shape: Shape,
    /// The row in the scrollable area, to tell which end comes first.
    row: u64,
    /// Engine anchors: the cell, its row's first cell, its row's last cell.
    anchors: [u64; 3],
}

pub struct CopyMode {
    terminal: ResourceId,
    view: ViewId,
    col: u16,
    row: u16,
    pin: Option<Pin>,
    /// The engine anchors the shown selection runs between.
    ends: Vec<u64>,
}

/// The codepoint in each cell of `row` (`0` for an empty cell).
fn row_cells(frame: &GridFrame, row: u16) -> Vec<u32> {
    let start = usize::from(row) * usize::from(frame.cols);
    (start..start + usize::from(frame.cols))
        .map(|index| {
            std::str::from_utf8(frame.buffer.cell_text(index))
                .ok()
                .and_then(|text| text.chars().next())
                .map_or(0, u32::from)
        })
        .collect()
}

/// Where the viewport's first row lands after `scroll` from `frame`.
fn top_after(frame: &GridFrame, scroll: Scroll) -> u64 {
    let bar = frame.scrollbar;
    let tail = bar.total.saturating_sub(bar.len);
    match scroll {
        Scroll::Top => 0,
        Scroll::Delta(rows) => bar.offset.saturating_add_signed(rows).min(tail),
        _ => tail,
    }
}

/// The key as a lowercase character and whether it was shifted.
fn letter(event: &KeyEvent) -> Option<(char, bool)> {
    let c = match &event.key {
        Key::Text(text) => text.chars().next()?,
        Key::Command(c) => *c,
        _ => return None,
    };
    Some((c.to_ascii_lowercase(), c.is_uppercase() || event.mods.shift))
}

impl Desk {
    /// Enter copy mode on the focused pane at its cursor, or leave it.
    pub fn toggle_copy_mode(&mut self) {
        if self.copy_mode.is_some() {
            return self.leave_copy_mode(true);
        }
        let Some((terminal, view)) = self
            .shown
            .as_ref()
            .and_then(|s| Some((s.resource.clone(), s.view?)))
        else {
            return;
        };
        let Some(frame) = self.client.as_ref().and_then(|c| c.acquire_view(view)) else {
            return;
        };
        let (col, row) = if frame.cursor.visible {
            (frame.cursor.col, frame.cursor.row)
        } else {
            (0, frame.rows.saturating_sub(1))
        };
        self.copy_mode = Some(CopyMode {
            terminal,
            view,
            col,
            row,
            pin: None,
            ends: Vec::new(),
        });
        self.show_copy_cursor(frame.scrollbar.offset);
        self.say(NoticeKind::Info, HINT);
    }

    /// Leave copy mode, releasing its anchors; `clear` takes the highlight
    /// down too.
    pub(super) fn leave_copy_mode(&mut self, clear: bool) {
        let Some(mode) = self.copy_mode.take() else {
            return;
        };
        if let Some(engine) = self.client.as_ref().and_then(|c| c.engine()) {
            if clear {
                let _ = engine.clear_view_selection(mode.view);
            }
            let pinned = mode.pin.iter().flat_map(|pin| pin.anchors);
            for anchor in mode.ends.into_iter().chain(pinned) {
                let _ = engine.release_view_anchor(mode.view, anchor);
            }
        }
        if self.notice.as_ref().is_some_and(|n| n.text == HINT) {
            self.notice = None;
        }
        self.generation += 1;
    }

    /// Copy mode belongs to one view; it ends when that pane loses it.
    pub(super) fn keep_copy_mode(&mut self) {
        let gone = self.copy_mode.as_ref().is_some_and(|mode| {
            self.shown
                .as_ref()
                .is_none_or(|s| s.resource != mode.terminal || s.view != Some(mode.view))
        });
        if gone {
            self.leave_copy_mode(true);
        }
    }

    /// An engine anchor on a viewport cell.
    fn pin_cell(&self, view: ViewId, column: u16, row: u16) -> Option<u64> {
        let point = EngineDocumentPoint {
            space: VIEWPORT,
            column,
            row: u32::from(row),
        };
        self.client
            .as_ref()?
            .engine()?
            .track_view_anchor(view, point)
            .ok()
    }

    /// Select from the pin (or just the cursor's cell) to the cursor, with
    /// the viewport's first row at `top` of the scrollable area.
    fn show_copy_cursor(&mut self, top: u64) {
        let Some(mode) = &self.copy_mode else { return };
        let (view, col, row, pin) = (mode.view, mode.col, mode.row, mode.pin);
        let Some(last) = self
            .client
            .as_ref()
            .and_then(|c| c.acquire_view(view))
            .map(|frame| frame.cols.saturating_sub(1))
        else {
            return;
        };
        let cursor_first = pin.is_some_and(|pin| top + u64::from(row) < pin.row);
        // A line selection runs from one row's first cell to the other's last.
        let (pinned, cursor_col) = match pin {
            Some(pin) if pin.shape == Shape::Line && cursor_first => (Some(pin.anchors[2]), 0),
            Some(pin) if pin.shape == Shape::Line => (Some(pin.anchors[1]), last),
            Some(pin) => (Some(pin.anchors[0]), col),
            None => (None, col),
        };
        let Some(cursor) = self.pin_cell(view, cursor_col, row) else {
            return;
        };
        let block = pin.is_some_and(|pin| pin.shape == Shape::Block);
        if let Some(engine) = self.client.as_ref().and_then(|c| c.engine()) {
            let _ = engine.set_view_selection(view, pinned.unwrap_or(cursor), cursor, block);
            if let Some(mode) = &mut self.copy_mode {
                for old in std::mem::replace(&mut mode.ends, vec![cursor]) {
                    let _ = engine.release_view_anchor(view, old);
                }
            }
        }
        self.generation += 1;
    }

    /// `v`, `V`, `C-v`: start selecting from the cursor, change an open
    /// selection's shape, or (the same key again) drop it.
    fn toggle_copy_selection(&mut self, shape: Shape, frame: &GridFrame) {
        let Some(mode) = &self.copy_mode else { return };
        let (view, col, row) = (mode.view, mode.col, mode.row);
        let pin = match mode.pin {
            Some(pin) if pin.shape == shape => {
                if let Some(engine) = self.client.as_ref().and_then(|c| c.engine()) {
                    for anchor in pin.anchors {
                        let _ = engine.release_view_anchor(view, anchor);
                    }
                }
                None
            }
            Some(pin) => Some(Pin { shape, ..pin }),
            None => {
                let last = frame.cols.saturating_sub(1);
                let cells = [col, 0, last].map(|column| self.pin_cell(view, column, row));
                match cells {
                    [Some(cell), Some(first), Some(end)] => Some(Pin {
                        shape,
                        row: frame.scrollbar.offset + u64::from(row),
                        anchors: [cell, first, end],
                    }),
                    _ => None,
                }
            }
        };
        if let Some(mode) = &mut self.copy_mode {
            mode.pin = pin;
        }
    }

    /// Copy what is selected and leave. With nothing selected, `word` takes
    /// the word under the cursor and otherwise the cursor's whole row.
    fn copy_and_leave(&mut self, word: bool, frame: &GridFrame, tx: &mut Tx) {
        let Some(mode) = &self.copy_mode else { return };
        let (view, row) = (mode.view, mode.row);
        if mode.pin.is_none() {
            let (start, end) = if word {
                word_span(&row_cells(frame, row), mode.col)
            } else {
                (0, frame.cols.saturating_sub(1))
            };
            let ends = (
                self.pin_cell(view, start, row),
                self.pin_cell(view, end, row),
            );
            if let (Some(start), Some(end)) = ends
                && let Some(engine) = self.client.as_ref().and_then(|c| c.engine())
            {
                let _ = engine.set_view_selection(view, start, end, false);
                if let Some(mode) = &mut self.copy_mode {
                    mode.ends.extend([start, end]);
                }
            }
        }
        self.copy(tx);
        self.leave_copy_mode(true);
    }

    /// A key while copy mode is on. False leaves it to the app's commands
    /// (a ⌘ chord); everything else is copy mode's, never terminal input.
    pub(super) fn copy_mode_key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        let Some(mode) = &self.copy_mode else {
            return false;
        };
        let (view, col, row) = (mode.view, mode.col, mode.row);
        let Some(frame) = self.client.as_ref().and_then(|c| c.acquire_view(view)) else {
            self.leave_copy_mode(true);
            return true;
        };
        // Off macOS Ctrl is the command modifier too: a bare Ctrl chord is
        // copy mode's, one with Shift or Alt is the app's.
        let ctrl = event.mods.ctrl && !event.mods.shift && !event.mods.alt;
        if event.mods.cmd && !ctrl {
            return false;
        }
        let bottom = frame.rows.saturating_sub(1);
        let page = i64::from(bottom.max(1));
        let cells = || row_cells(&frame, row);
        let along = |motion| (row_motion_target(&cells(), col, motion), row, None);
        let scrolled = |scroll| (col, row, Some(scroll));
        let (to_col, to_row, scroll) = match (&event.key, letter(event)) {
            (Key::Escape, _) | (_, Some(('q', _))) if !ctrl => {
                self.leave_copy_mode(true);
                return true;
            }
            (Key::Enter, _) | (_, Some(('y', false))) if !ctrl => {
                self.copy_and_leave(true, &frame, tx);
                return true;
            }
            (_, Some(('y', true))) if !ctrl => {
                self.copy_and_leave(false, &frame, tx);
                return true;
            }
            (_, Some(('v', shift))) => {
                let shape = match (ctrl, shift) {
                    (true, _) => Shape::Block,
                    (false, true) => Shape::Line,
                    (false, false) => Shape::Char,
                };
                self.toggle_copy_selection(shape, &frame);
                (col, row, None)
            }
            (_, Some(('u', _))) if ctrl => scrolled(Scroll::Delta(-(page / 2).max(1))),
            (_, Some(('d', _))) if ctrl => scrolled(Scroll::Delta((page / 2).max(1))),
            (Key::PageUp, _) => scrolled(Scroll::Delta(-page)),
            (Key::PageDown, _) => scrolled(Scroll::Delta(page)),
            (_, Some(('b', _))) if ctrl => scrolled(Scroll::Delta(-page)),
            (_, Some(('f', _))) if ctrl => scrolled(Scroll::Delta(page)),
            // A control chord is never its plain letter.
            _ if ctrl => return true,
            (Key::Left, _) | (_, Some(('h', false))) => (col.saturating_sub(1), row, None),
            (Key::Right, _) | (_, Some(('l', false))) => (col + 1, row, None),
            (Key::Up, _) | (_, Some(('k', _))) if row == 0 => scrolled(Scroll::Delta(-1)),
            (Key::Up, _) | (_, Some(('k', _))) => (col, row - 1, None),
            (Key::Down, _) | (_, Some(('j', _))) if row == bottom => scrolled(Scroll::Delta(1)),
            (Key::Down, _) | (_, Some(('j', _))) => (col, row + 1, None),
            (_, Some(('h', true))) => (col, 0, None),
            (_, Some(('m', true))) => (col, bottom / 2, None),
            (_, Some(('l', true))) => (col, bottom, None),
            (_, Some(('g', false))) => (0, 0, Some(Scroll::Top)),
            (_, Some(('g', true))) => (0, bottom, Some(Scroll::Bottom)),
            (Key::Home, _) | (_, Some(('0', _))) => (0, row, None),
            (Key::End, _) | (_, Some(('$', _))) => along(RowMotion::LineEnd),
            (_, Some(('^', _))) => along(RowMotion::FirstNonBlank),
            (_, Some(('w', _))) => along(RowMotion::WordForward),
            (_, Some(('b', _))) => along(RowMotion::WordBack),
            (_, Some(('e', _))) => along(RowMotion::WordEnd),
            _ => return true,
        };
        let top = scroll.map_or(frame.scrollbar.offset, |scroll| {
            if let Some(client) = &self.client {
                let _ = client.with_control(|control| control.scroll_view(view, scroll));
            }
            top_after(&frame, scroll)
        });
        if let Some(mode) = &mut self.copy_mode {
            mode.col = to_col.min(frame.cols.saturating_sub(1));
            mode.row = to_row.min(bottom);
        }
        self.show_copy_cursor(top);
        true
    }
}

#[cfg(test)]
mod tests {
    use rune::Mods;

    use super::*;
    use crate::desk::input::selection::Selected;

    fn press(rt: &mut rune::Runtime<Desk>, keys: &str) {
        for c in keys.chars() {
            rt.key(KeyEvent {
                key: Key::Text(c.to_string()),
                mods: Mods::default(),
            });
            rt.frame(0.0);
        }
    }

    fn selected(rt: &rune::Runtime<Desk>) -> Option<String> {
        match rt.state.selected() {
            Selected::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Three rows, the cursor left at the end of the third.
    fn fixture() -> (
        std::sync::Arc<phux_client_runtime::Client>,
        rune::Runtime<Desk>,
    ) {
        let (client, mut rt) =
            crate::grid::budget::input_fixture_showing("\r\nalpha beta\r\ngamma.delta\r\nlast");
        rt.frame(0.0);
        rt.update(|desk, _| desk.toggle_copy_mode());
        (client, rt)
    }

    #[test]
    fn keys_move_select_and_copy_without_reaching_the_terminal() {
        let (client, mut rt) = fixture();
        let board = rune::clipboard::memory();
        rt.use_clipboard(&board);
        // Let the pane's fit to the window go out first.
        rt.frame(0.0);
        rt.frame(0.0);
        let _ = client.take_outbound();
        assert!(rt.state.copy_mode.is_some());

        // Up two rows, to the line's start, select two words, copy.
        press(&mut rt, "kk0vee");
        assert_eq!(selected(&rt).as_deref(), Some("alpha beta"));
        press(&mut rt, "y");
        rt.frame(0.0);
        assert_eq!(
            board.text().as_ref().map(|t| t.as_str()),
            Some("alpha beta")
        );
        assert!(rt.state.copy_mode.is_none(), "y leaves copy mode");
        assert_eq!(selected(&rt), None, "and takes the highlight down");
        let out = client.take_outbound();
        assert!(out.is_empty(), "no key was typed: {out:?}");
    }

    #[test]
    fn line_selection_takes_whole_rows_in_either_direction() {
        let (_client, mut rt) = fixture();
        press(&mut rt, "kVk");
        assert_eq!(selected(&rt).as_deref(), Some("alpha beta\ngamma.delta"));
        // The same key drops the selection; from the top row, down again.
        press(&mut rt, "VVj");
        assert_eq!(selected(&rt).as_deref(), Some("alpha beta\ngamma.delta"));
    }

    #[test]
    fn y_with_nothing_selected_copies_the_word_and_escape_leaves() {
        let (_client, mut rt) = fixture();
        let board = rune::clipboard::memory();
        rt.use_clipboard(&board);
        press(&mut rt, "k0wy");
        rt.frame(0.0);
        assert_eq!(board.text().as_ref().map(|t| t.as_str()), Some("."));

        rt.update(|desk, _| desk.toggle_copy_mode());
        rt.key(KeyEvent {
            key: Key::Escape,
            mods: Mods::default(),
        });
        rt.frame(0.0);
        assert!(rt.state.copy_mode.is_none());
        assert_eq!(selected(&rt), None);
    }
}
