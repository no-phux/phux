//! Search in copy mode, as the TUI's: `/` (forward) and `?` (back) read a
//! query in the Find field, Return searches from the cursor, and `n` and
//! `N` go on the same way or the other. Each match lands selected, so `y`
//! copies just it. Find hands its current match to copy mode the same way.

use phux_client_runtime::ViewId;
use phux_client_runtime::engine::{EngineHandle, Scroll};
use phux_client_runtime::publication::GridFrame;

use super::{Pin, Shape};
use crate::desk::{Desk, NoticeKind};

/// A cell as the history counts it: the row from the top of the scrollable
/// area, then the column.
pub type Spot = (u64, u16);

/// Where `anchor` is now; `None` once pruned.
pub fn spot(engine: &EngineHandle, view: ViewId, anchor: u64) -> Option<Spot> {
    let point = engine.view_anchor_point(view, anchor).ok()??;
    Some((u64::from(point.row), point.column))
}

/// Which of `len` matches (in document order; `at` gives one's start and
/// end) a search from `cursor` lands on: forward, the first starting after
/// it; back, the last ending before it; wrapping around either end. A
/// pruned end counts as before everything, where pruning happens. A binary
/// search, so only a few matches are ever looked up.
pub fn pick(
    len: usize,
    at: impl Fn(usize) -> (Option<Spot>, Option<Spot>),
    cursor: Spot,
    back: bool,
) -> Option<usize> {
    let all: Vec<usize> = (0..len).collect();
    if back {
        let before = all.partition_point(|&i| at(i).1.is_none_or(|end| end < cursor));
        before.checked_sub(1).or(len.checked_sub(1))
    } else {
        let after = all.partition_point(|&i| at(i).0.is_none_or(|start| start <= cursor));
        (after < len).then_some(after).or((len > 0).then_some(0))
    }
}

impl Desk {
    /// `/` or `?`: read the query in the Find field.
    pub(super) fn copy_search_prompt(&mut self, back: bool) {
        if let Some(mode) = &mut self.connection.copy_mode {
            mode.back = back;
        }
        self.open_find();
    }

    /// Return in the Find field over copy mode: the field closes and the
    /// search runs from the cursor. False with copy mode off.
    pub(in crate::desk) fn copy_search_commit(&mut self) -> bool {
        let Some(mode) = &mut self.connection.copy_mode else {
            return false;
        };
        let query = self.connection.find_query.text.clone();
        mode.query = Some(query).filter(|query| !query.is_empty());
        let back = mode.back;
        self.close_find();
        self.copy_search(back);
        true
    }

    /// The copy cursor again, after Find's highlight took its place.
    pub(in crate::desk) fn copy_cursor_again(&mut self) {
        let Some(mode) = &self.connection.copy_mode else {
            return;
        };
        let frame = self
            .connection
            .client
            .as_ref()
            .and_then(|c| c.acquire_view(mode.view));
        if let Some(top) = frame.map(|frame| frame.scrollbar.offset) {
            self.show_copy_cursor(top);
        }
    }

    /// `n`, or `N` (`reverse`): the last search again.
    pub(super) fn copy_search_again(&mut self, reverse: bool) {
        let back = self
            .connection
            .copy_mode
            .as_ref()
            .is_some_and(|mode| mode.back != reverse);
        self.copy_search(back);
    }

    fn copy_search(&mut self, back: bool) {
        let Some(mode) = &self.connection.copy_mode else {
            return;
        };
        let (view, col, row) = (mode.view, mode.col, mode.row);
        let Some(query) = mode.query.clone() else {
            return;
        };
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        let (Some(engine), Some(frame)) = (client.engine(), client.acquire_view(view)) else {
            return;
        };
        let cursor = (frame.scrollbar.offset + u64::from(row), col);
        let found = engine.search_view_bounded(view, query.clone(), false);
        let matches: Vec<_> = found.map(|f| f.matches).unwrap_or_default();
        let ends = |i: usize| {
            let m = &matches[i];
            (spot(&engine, view, m.start), spot(&engine, view, m.end))
        };
        let landing = pick(matches.len(), ends, cursor, back).and_then(|i| {
            let (start, end) = ends(i);
            Some((start?, end?))
        });
        let _ = engine.clear_view_search(view);
        match landing {
            Some((start, end)) => self.copy_land(start, end, &frame),
            None => self.say(
                NoticeKind::Info,
                format!("No match for \u{201c}{query}\u{201d}."),
            ),
        }
    }

    /// Select `start` to `end` with the cursor on `end`, scrolling the start
    /// to the top when the match isn't all in view.
    pub(super) fn copy_land(&mut self, start: Spot, end: Spot, frame: &GridFrame) {
        // The old selection's anchors go first, while the viewport is as
        // `frame` saw it.
        self.clear_copy_selection();
        let Some(view) = self.connection.copy_mode.as_ref().map(|mode| mode.view) else {
            return;
        };
        let bar = frame.scrollbar;
        let mut top = bar.offset;
        if start.0 < top || end.0 >= top + u64::from(frame.rows) {
            if let Some(client) = &self.connection.client {
                let _ = client.with_control(|c| c.scroll_view(view, Scroll::Row(start.0)));
            }
            top = start.0.min(bar.total.saturating_sub(bar.len));
        }
        let bottom = frame.rows.saturating_sub(1);
        let row_of =
            |row: u64| u16::try_from(row.saturating_sub(top)).map_or(bottom, |r| r.min(bottom));
        let row = row_of(start.0);
        let cells = [start.1, 0, frame.cols.saturating_sub(1)].map(|c| self.pin_cell(view, c, row));
        let pin = match cells {
            [Some(cell), Some(first), Some(last)] => Some(Pin {
                shape: Shape::Char,
                row: start.0,
                anchors: [cell, first, last],
            }),
            _ => None,
        };
        if let Some(mode) = &mut self.connection.copy_mode {
            mode.pin = pin;
            mode.col = end.1;
            mode.row = row_of(end.0);
        }
        self.show_copy_cursor(top);
    }
}

#[cfg(test)]
mod tests;
