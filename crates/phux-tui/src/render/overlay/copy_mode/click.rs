//! Repeat clicks: a second and third press on one cell select a word, then a
//! line.

use std::time::{Duration, Instant};

use phux_protocol::ResourceId;

/// The longest gap between presses that still counts as a repeat click.
pub(super) const REPEAT_CLICK: Duration = Duration::from_millis(500);

/// The last mouse press that entered copy-mode, kept by
/// [`super::OverlayState`] across overlays so a second and third press on the
/// same cell count as double- and triple-clicks (Ghostty: word, line).
#[derive(Debug, Clone)]
pub(in crate::render::overlay) struct ClickRecord {
    pane: ResourceId,
    col: u16,
    row: u16,
    at: Instant,
    count: u8,
}

impl ClickRecord {
    /// Record a press on `pane`'s `(col, row)` at `now` and return its count:
    /// 1, 2, or 3, a fourth repeat starting over at 1 as Ghostty's does.
    pub(in crate::render::overlay) fn press(
        last: &mut Option<Self>,
        pane: &ResourceId,
        col: u16,
        row: u16,
        now: Instant,
    ) -> u8 {
        let count = match last.as_ref() {
            Some(prev)
                if prev.pane == *pane
                    && (prev.col, prev.row) == (col, row)
                    && now.saturating_duration_since(prev.at) <= REPEAT_CLICK =>
            {
                prev.count % 3 + 1
            }
            _ => 1,
        };
        *last = Some(Self {
            pane: pane.clone(),
            col,
            row,
            at: now,
            count,
        });
        count
    }
}
