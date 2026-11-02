//! Mouse selection and what leaves it: the drag, its autoscroll past the
//! grid, Select All, and the copy guard.

use std::time::Duration;

use phux_client_runtime::engine::{BoundedSelectionText, Scroll, SelectionGestureEvent};
use phux_protocol::ids::ResourceId;
use rune::{Phase, Tx};

use super::Modal;
use crate::desk::mouse::Press;
use crate::desk::{Desk, NoticeKind};

/// Largest selection the clipboard takes, in UTF-8 bytes. A larger one is
/// refused whole, never truncated.
const COPY_LIMIT: usize = 1024 * 1024;

/// How often a drag held past the grid scrolls.
const AUTOSCROLL_TICK: Duration = Duration::from_millis(40);

/// The farthest one autoscroll tick moves, in rows.
const AUTOSCROLL_MAX_ROWS: f32 = 8.0;

/// A selection drag in progress, at its last point.
#[derive(Debug, Clone)]
pub struct Held {
    terminal: ResourceId,
    x: f32,
    y: f32,
    clicks: u32,
    /// App clock seconds of the next scroll, while past the top or bottom edge.
    next: Option<f64>,
}

/// A view's selection, as far as the clipboard is concerned.
pub(in crate::desk) enum Selected {
    Nothing,
    Text(String),
    TooLarge,
}

/// Text worth a clipboard entry: blank cells are not.
fn copyable(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    (!text.trim().is_empty()).then(|| text.into_owned())
}

/// Rows one tick scrolls for a pointer at `y` in the grid's points: negative
/// toward history above it, positive toward live output below it, zero
/// inside. Speed grows with how far past the edge the pointer is.
fn autoscroll_rows(y: f32, height: f32, cell_h: f32) -> i64 {
    let past = if y < 0.0 {
        -y
    } else if y > height {
        y - height
    } else {
        return 0;
    };
    let rows = (past / cell_h.max(1.0))
        .ceil()
        .clamp(1.0, AUTOSCROLL_MAX_ROWS) as i64;
    if y < 0.0 { -rows } else { rows }
}

/// A point held inside `0..extent` surface pixels: a drag past the pane stays
/// on the edge cell.
fn clamp_surface(point: f32, extent: u32) -> f64 {
    f64::from(point).clamp(0.0, f64::from(extent.saturating_sub(1)))
}

impl Desk {
    /// The focused view's selection, bounded.
    pub(in crate::desk) fn selected(&self) -> Selected {
        let engine = self.client.as_ref().and_then(|client| client.engine());
        let (Some(engine), Some(shown)) = (engine, &self.shown) else {
            return Selected::Nothing;
        };
        match engine.selection_text_view_bounded(shown.view, COPY_LIMIT) {
            Ok(BoundedSelectionText::Text(bytes)) => {
                copyable(&bytes).map_or(Selected::Nothing, Selected::Text)
            }
            Ok(
                BoundedSelectionText::ByteLimitExceeded | BoundedSelectionText::WorkLimitExceeded,
            ) => Selected::TooLarge,
            _ => Selected::Nothing,
        }
    }

    pub(in crate::desk) fn copy(&mut self, tx: &mut Tx) {
        match self.selected() {
            Selected::Text(text) => tx.copy(text),
            Selected::TooLarge => self.say(
                NoticeKind::Error,
                "Selection too large to copy (over 1 MiB, or too many rows).",
            ),
            Selected::Nothing => {}
        }
    }

    /// Everything the focused view holds, scrollback included.
    pub(in crate::desk) fn select_all(&mut self) {
        let engine = self.client.as_ref().and_then(|client| client.engine());
        if let (Some(engine), Some(shown)) = (engine, &self.shown) {
            let _ = engine.select_all_view(shown.view);
            self.generation += 1;
        }
    }

    /// A press, drag, or release over the grid selects through the engine.
    pub fn select(&mut self, terminal: &ResourceId, phase: Phase, x: f32, y: f32, clicks: u32) {
        let Some(view) = self.view_of(terminal) else {
            return;
        };
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(frame) = client.acquire_view(view) else {
            return;
        };
        let Some(engine) = client.engine() else {
            return;
        };
        let metrics = self.metrics();
        let (col, row) = metrics.cell_at(x, y, frame.cols, frame.rows);
        let (cell_w, cell_h) = metrics.cell_px();
        let phase = match phase {
            Phase::Down => 0,
            Phase::Move => 1,
            Phase::Up => 2,
        };
        let (width, height) = (
            u32::from(frame.cols) * u32::from(cell_w),
            u32::from(frame.rows) * u32::from(cell_h),
        );
        let event = SelectionGestureEvent {
            phase,
            clicks: clicks.clamp(1, 3),
            handle: self.gesture,
            column: col,
            rectangle: false,
            row: u32::from(row),
            x: clamp_surface(x * metrics.scale, width),
            y: clamp_surface(y * metrics.scale, height),
            columns: u32::from(frame.cols),
            cell_width: u32::from(cell_w),
            screen_height: height.max(1),
            padding_left: 0,
        };
        // A refused step ended the gesture, as a release does.
        self.gesture = match engine.view_selection_gesture(view, event) {
            Ok(result) if phase != 2 => result.handle,
            _ => 0,
        };
        self.generation += 1;
    }

    /// The rows a drag at `y` should scroll per tick.
    fn rows_past_edge(&self, terminal: &ResourceId, y: f32) -> i64 {
        let frame = self
            .view_of(terminal)
            .zip(self.client.as_ref())
            .and_then(|(view, client)| client.acquire_view(view));
        let cell_h = self.metrics().cell_h;
        frame.map_or(0, |f| {
            autoscroll_rows(y, f32::from(f.rows) * cell_h, cell_h)
        })
    }

    /// Note where a selection drag is, arming or dropping its autoscroll.
    pub(in crate::desk) fn track_drag(
        &mut self,
        terminal: &ResourceId,
        press: &Press,
        tx: &mut Tx,
    ) {
        if press.phase == Phase::Up {
            self.pointer.held = None;
            return;
        }
        let armed = self.pointer.held.as_ref().and_then(|held| held.next);
        let next = (self.rows_past_edge(terminal, press.y) != 0).then(|| {
            armed.unwrap_or_else(|| {
                tx.wake_after(AUTOSCROLL_TICK);
                self.clock + AUTOSCROLL_TICK.as_secs_f64()
            })
        });
        self.pointer.held = Some(Held {
            terminal: terminal.clone(),
            x: press.x,
            y: press.y,
            clicks: press.clicks,
            next,
        });
    }

    /// Finish a selection drag whose release won't come, where it last was.
    fn end_drag(&mut self) {
        if let Some(held) = self.pointer.held.take()
            && self.gesture != 0
        {
            self.select(&held.terminal, Phase::Up, held.x, held.y, held.clicks);
        }
        self.gesture = 0;
    }

    /// End what Rune will never release: its pane closed, so no handler hears
    /// the release, or a modal dropped the press. A window losing focus needs
    /// nothing here, since Rune sends that drag a cancelled release.
    pub(in crate::desk) fn settle_pointer(&mut self) {
        let covered = self.modal().is_some_and(Modal::takes_pointer);
        let dragging = self.pointer.held.as_ref().map(|held| &held.terminal);
        if dragging.is_some_and(|terminal| covered || self.view_of(terminal).is_none()) {
            self.end_drag();
        }
        if self
            .capture_terminal()
            .is_some_and(|terminal| covered || self.view_of(terminal).is_none())
        {
            self.release_capture();
        }
    }

    /// One autoscroll step, when one is due: scroll toward the pointer, then
    /// extend the selection over what scrolled in. True when it moved.
    pub(in crate::desk) fn autoscroll(&mut self, tx: &mut Tx) -> bool {
        let Some(held) = self.pointer.held.clone() else {
            return false;
        };
        if self.gesture == 0 {
            self.pointer.held = None;
            return false;
        }
        let Some(next) = held.next else {
            return false;
        };
        // Early by a frame's rounding: the wake that got here was the only one.
        if next > self.clock {
            tx.wake_after(Duration::from_secs_f64(next - self.clock));
            return false;
        }
        tx.wake_after(AUTOSCROLL_TICK);
        self.pointer.held = Some(Held {
            next: Some(self.clock + AUTOSCROLL_TICK.as_secs_f64()),
            ..held.clone()
        });
        if !self.scroll_toward(&held) {
            return false;
        }
        self.select(&held.terminal, Phase::Move, held.x, held.y, held.clicks);
        true
    }

    /// Scroll a held drag's view. False at the end it is pushing against, so
    /// a pointer parked there costs no runtime turn.
    fn scroll_toward(&self, held: &Held) -> bool {
        let rows = self.rows_past_edge(&held.terminal, held.y);
        let (Some(client), Some(view)) = (&self.client, self.view_of(&held.terminal)) else {
            return false;
        };
        let at_end = client.acquire_view(view).is_none_or(|frame| {
            if rows < 0 {
                frame.scrollbar.offset == 0
            } else {
                frame.scrollbar.at_tail()
            }
        });
        if rows == 0 || at_end {
            return false;
        }
        client
            .with_control(|control| control.scroll_view(view, Scroll::Delta(rows)))
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_selections_are_not_worth_copying() {
        assert_eq!(copyable(b"  \n\t "), None);
        assert_eq!(copyable(b""), None);
        assert_eq!(copyable(b" x "), Some(" x ".to_owned()));
    }

    #[test]
    fn autoscroll_runs_only_past_the_edges_and_speeds_up() {
        assert_eq!(autoscroll_rows(10.0, 100.0, 20.0), 0);
        assert_eq!(autoscroll_rows(100.0, 100.0, 20.0), 0);
        assert_eq!(autoscroll_rows(-1.0, 100.0, 20.0), -1);
        assert_eq!(autoscroll_rows(150.0, 100.0, 20.0), 3);
        assert_eq!(autoscroll_rows(-9000.0, 100.0, 20.0), -8);
    }

    #[test]
    fn points_stay_on_the_surface() {
        assert_eq!(clamp_surface(-5.0, 100), 0.0);
        assert_eq!(clamp_surface(500.0, 100), 99.0);
        assert_eq!(clamp_surface(50.5, 100), 50.5);
    }
}
