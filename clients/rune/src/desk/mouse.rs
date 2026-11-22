//! The pointer over a pane: reported to a program that tracks the mouse,
//! else local selection and history scrolling, as Ghostty does.
//!
//! - Shift keeps local selection even while a program tracks the mouse.
//! - A press the program took always gets its release, wherever it lands.
//! - Motion is sent once per cell, and only to programs that asked for it.
//! - Rune reports the secondary button once, at its release, so a tracking
//!   program hears its press and release together; elsewhere it opens a menu.
//! - The wheel is buttons 4 and 5 to a tracking program, arrow keys in the
//!   alternate screen (less, man), and history everywhere else.
//! - Option-click (Alt-click) on the line a shell prompt is reading moves
//!   its cursor there with arrow keys, as Ghostty and iTerm do.

use phux_client_runtime::Client;
use phux_client_runtime::engine::{MouseMode, ReplicaPrompt, Scroll};
use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use rune::{Mods, Phase, PointerButton, PointerPress, Tx};

use super::Desk;
use crate::keys;

/// One step of a button's gesture over a pane, at `x, y` in its grid's points.
#[derive(Debug, Clone, Copy)]
pub struct Press {
    pub phase: Phase,
    pub x: f32,
    pub y: f32,
    pub clicks: u32,
    pub mods: Mods,
    pub button: PointerButton,
}

/// The wire's name for a button, in xterm's numbering. A wheel never comes
/// through here, so Four to Seven stay the wheel's.
const fn wire_button(button: PointerButton) -> MouseButton {
    match button {
        PointerButton::Primary => MouseButton::Left,
        PointerButton::Secondary => MouseButton::Right,
        PointerButton::Middle => MouseButton::Middle,
        PointerButton::Back => MouseButton::Eight,
        PointerButton::Forward => MouseButton::Nine,
        _ => MouseButton::Unknown,
    }
}

/// The most wheel steps one event sends, so a fling can't flood a program.
const MAX_WHEEL_STEPS: i64 = 100;

impl Desk {
    pub(super) fn mouse_mode(&self, terminal: &ResourceId) -> MouseMode {
        self.connection
            .client
            .as_ref()
            .and_then(|c| c.engine())
            .and_then(|e| e.mouse_mode(terminal).ok())
            .unwrap_or(MouseMode::None)
    }

    /// A press, drag or release over a pane.
    pub fn pointer(&mut self, terminal: &ResourceId, press: &Press, tx: &mut Tx) {
        if press.phase == Phase::Down {
            self.chief.disarm();
            self.focus_pane(terminal);
        }
        let captured = self.capture_terminal() == Some(terminal);
        let tracking = self.mouse_mode(terminal) != MouseMode::None && !press.mods.shift;
        if captured || (press.phase == Phase::Down && tracking) {
            self.report(terminal, press);
            return;
        }
        // Selection is the primary button's; with no program to tell, the
        // others do nothing here.
        if press.button != PointerButton::Primary {
            return;
        }
        if press.phase == Phase::Down
            && press.mods.alt
            && self.click_to_cursor(terminal, press.x, press.y)
        {
            return;
        }
        // The rest of a drag that already ended.
        if press.phase != Phase::Down && self.connection.gesture == 0 {
            return;
        }
        self.track_drag(terminal, press, tx);
        self.select(terminal, press.phase, press.x, press.y, press.clicks);
    }

    /// Walk a prompt's cursor to the clicked cell with arrow keys. Only
    /// while the shell reads a command line (OSC 133) and the click is on
    /// the cursor's line, wrapped rows and all; false leaves the click to
    /// selection.
    fn click_to_cursor(&mut self, terminal: &ResourceId, x: f32, y: f32) -> bool {
        let Some(client) = self.connection.client.clone() else {
            return false;
        };
        let at_prompt = client
            .engine()
            .is_some_and(|e| e.replica_prompt(terminal) == ReplicaPrompt::AtPrompt);
        let frame = self.view_of(terminal).and_then(|v| client.acquire_view(v));
        let Some(frame) = frame.filter(|f| at_prompt && f.cursor.visible && f.scrollbar.at_tail())
        else {
            return false;
        };
        let to = self.metrics().cell_at(x, y, frame.cols, frame.rows);
        let from = (frame.cursor.col, frame.cursor.row);
        let Some(steps) = cursor_steps(&frame.buffer.row_wrapped, frame.cols, from, to) else {
            return false;
        };
        let key = if steps < 0 {
            PhysicalKey::ArrowLeft
        } else {
            PhysicalKey::ArrowRight
        };
        client.with_control(|control| {
            control.input_ready(terminal)
                && (0..steps.unsigned_abs()).all(|_| control.send_key(terminal, arrow(key)))
        });
        true
    }

    fn report(&mut self, terminal: &ResourceId, press: &Press) {
        let Press { phase, x, y, .. } = *press;
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        let Some(view) = self.view_of(terminal) else {
            return;
        };
        let Some(frame) = client.acquire_view(view) else {
            return;
        };
        let metrics = self.metrics();
        let (col, row) = metrics.cell_at(x, y, frame.cols, frame.rows);
        let action = match phase {
            Phase::Down => {
                self.connection.mouse_capture = Some((terminal.clone(), *press));
                self.connection.mouse_cell = Some((col, row));
                MouseAction::Press
            }
            Phase::Move => {
                self.connection.mouse_capture = Some((terminal.clone(), *press));
                // Only motion-tracking programs hear drags, once per cell.
                let mode = self.mouse_mode(terminal);
                if !matches!(mode, MouseMode::Button | MouseMode::Any)
                    || self.connection.mouse_cell == Some((col, row))
                {
                    return;
                }
                self.connection.mouse_cell = Some((col, row));
                MouseAction::Motion
            }
            Phase::Up => {
                self.connection.mouse_capture = None;
                self.connection.mouse_cell = None;
                MouseAction::Release
            }
        };
        // Surface pixels inside the cell the point is in, clamped to the grid.
        let (x, y) = cell_center(&metrics, col, row);
        let event = MouseEvent {
            action,
            button: wire_button(press.button),
            mods: keys::mods(press.mods),
            x,
            y,
        };
        send_mouse(&client, terminal, event);
    }

    /// The pane whose program holds a press.
    pub(super) fn capture_terminal(&self) -> Option<&ResourceId> {
        self.connection
            .mouse_capture
            .as_ref()
            .map(|(terminal, _)| terminal)
    }

    /// Give the program the release Rune won't deliver, where the press last
    /// was. A pane that is gone just forgets it.
    pub(super) fn release_capture(&mut self) {
        if let Some((terminal, press)) = self.connection.mouse_capture.take() {
            let phase = Phase::Up;
            self.report(&terminal, &Press { phase, ..press });
            self.connection.mouse_cell = None;
        }
    }

    /// The secondary button's click at `at`, in a pane: the program's if it
    /// tracks the mouse, else the context menu. The click holds nothing, so a
    /// press the program already has keeps its capture and gets its release.
    pub fn secondary_press(&mut self, terminal: &ResourceId, at: Press, window: PointerPress) {
        self.focus_pane(terminal);
        if self.mouse_mode(terminal) != MouseMode::None && !at.mods.shift {
            let held = (
                self.connection.mouse_capture.clone(),
                self.connection.mouse_cell,
            );
            let phase = Phase::Down;
            self.report(terminal, &Press { phase, ..at });
            let phase = Phase::Up;
            self.report(terminal, &Press { phase, ..at });
            (self.connection.mouse_capture, self.connection.mouse_cell) = held;
            return;
        }
        self.pointer.menu = Some(window);
    }

    /// The pointer moved with nothing pressed: `MouseMode::Any` programs hear
    /// it, once per cell, while the grid is under it.
    pub(super) fn report_motion(&mut self, terminal: &ResourceId, x: f32, y: f32, mods: Mods) {
        if mods.shift || self.connection.mouse_capture.is_some() || self.connection.gesture != 0 {
            return;
        }
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        let Some(frame) = self.view_of(terminal).and_then(|v| client.acquire_view(v)) else {
            return;
        };
        let metrics = self.metrics();
        let cell = metrics.cell_at(x, y, frame.cols, frame.rows);
        if self.connection.mouse_cell == Some(cell) {
            return;
        }
        self.connection.mouse_cell = Some(cell);
        let inside = x >= 0.0
            && y >= 0.0
            && x < f32::from(frame.cols) * metrics.cell_w
            && y < f32::from(frame.rows) * metrics.cell_h;
        if !inside || self.mouse_mode(terminal) != MouseMode::Any {
            return;
        }
        let (x, y) = cell_center(&metrics, cell.0, cell.1);
        let event = MouseEvent {
            action: MouseAction::Motion,
            button: MouseButton::Unknown,
            mods: keys::mods(mods),
            x,
            y,
        };
        send_mouse(&client, terminal, event);
    }

    /// Nothing is under the pointer any more.
    pub(super) fn forget_mouse_cell(&mut self) {
        if self.connection.mouse_capture.is_none() {
            self.connection.mouse_cell = None;
        }
    }

    /// The surface pixels of the cell under a point in a pane's grid.
    fn surface_point(&self, terminal: &ResourceId, x: f32, y: f32) -> Option<(f64, f64)> {
        let frame = self
            .connection
            .client
            .as_ref()?
            .acquire_view(self.view_of(terminal)?)?;
        let metrics = self.metrics();
        let (col, row) = metrics.cell_at(x, y, frame.cols, frame.rows);
        Some(cell_center(&metrics, col, row))
    }

    /// Wheel over a pane at `x, y` in its grid's points: `dy` points,
    /// positive revealing older rows.
    pub fn wheel(&mut self, terminal: &ResourceId, x: f32, y: f32, dy: f32, mods: Mods) {
        // Leftover fractions belong to the pane they came from.
        if self.connection.wheel_pane.as_ref() != Some(terminal) {
            self.connection.wheel_pane = Some(terminal.clone());
            self.connection.wheel_rows = 0.0;
        }
        let cell_h = self.metrics().cell_h;
        self.connection.wheel_rows += dy / cell_h;
        let rows = self.connection.wheel_rows.trunc();
        if rows == 0.0 {
            return;
        }
        self.connection.wheel_rows -= rows;
        let steps = (rows as i64).clamp(-MAX_WHEEL_STEPS, MAX_WHEEL_STEPS);
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        if self.mouse_mode(terminal) != MouseMode::None && !mods.shift {
            let button = if steps > 0 {
                MouseButton::Four
            } else {
                MouseButton::Five
            };
            // Programs route the wheel by position (vim splits, tmux panes).
            let (x, y) = self.surface_point(terminal, x, y).unwrap_or((0.0, 0.0));
            for _ in 0..steps.abs() {
                let event = MouseEvent {
                    action: MouseAction::Press,
                    button,
                    mods: keys::mods(mods),
                    x,
                    y,
                };
                send_mouse(&client, terminal, event);
            }
            return;
        }
        if client.is_alt_screen(terminal) {
            // No history to scroll: arrows only when the program keeps DEC
            // 1007 on and the person hasn't turned them off.
            let arrows = self.settings.wheel_arrows
                && client
                    .engine()
                    .is_some_and(|engine| engine.alt_scroll(terminal).unwrap_or(true));
            if !arrows {
                return;
            }
            let key = if steps > 0 {
                PhysicalKey::ArrowUp
            } else {
                PhysicalKey::ArrowDown
            };
            for _ in 0..steps.abs() {
                client.with_control(|control| control.send_key(terminal, arrow(key)));
            }
            return;
        }
        let Some(view) = self.view_of(terminal) else {
            return;
        };
        let _ = client.with_control(|control| control.scroll_view(view, Scroll::Delta(-steps)));
        self.generation += 1;
    }
}

/// An unmodified press of `key`.
const fn arrow(key: PhysicalKey) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

/// Arrow presses from cell `from` to `to` along one line, soft wraps
/// joining rows: negative is left. `None` when `to` is on another line.
// ponytail: counts cells, so a wide character costs one press too many;
// walk the row's cell widths if that matters.
fn cursor_steps(wrapped: &[bool], cols: u16, from: (u16, u16), to: (u16, u16)) -> Option<i64> {
    let (top, bottom) = (from.1.min(to.1), from.1.max(to.1));
    let joined = (top..bottom).all(|row| wrapped.get(usize::from(row)) == Some(&true));
    let at = |(col, row): (u16, u16)| i64::from(row) * i64::from(cols) + i64::from(col);
    joined.then(|| at(to) - at(from))
}

/// Send a mouse event if the terminal can take input now. A mouse event is
/// momentary: one that can't arrive is not worth holding or reporting.
fn send_mouse(client: &Client, terminal: &ResourceId, event: MouseEvent) {
    client.with_control(|control| {
        control.input_ready(terminal) && control.send_mouse(terminal, event)
    });
}

/// Surface pixels at the middle of a cell, as the server divides by the
/// reported cell size.
fn cell_center(metrics: &crate::grid::Metrics, col: u16, row: u16) -> (f64, f64) {
    let (cell_w, cell_h) = metrics.cell_px();
    (
        f64::from(col) * f64::from(cell_w) + f64::from(cell_w) / 2.0,
        f64::from(row) * f64::from(cell_h) + f64::from(cell_h) / 2.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_keep_their_xterm_numbers() {
        assert_eq!(wire_button(PointerButton::Primary), MouseButton::Left);
        assert_eq!(wire_button(PointerButton::Secondary), MouseButton::Right);
        assert_eq!(wire_button(PointerButton::Middle), MouseButton::Middle);
        assert_eq!(wire_button(PointerButton::Back), MouseButton::Eight);
        assert_eq!(wire_button(PointerButton::Forward), MouseButton::Nine);
        assert_eq!(wire_button(PointerButton::Other(30)), MouseButton::Unknown);
    }

    #[test]
    fn a_click_walks_only_the_cursors_line_wrapped_rows_and_all() {
        // Row 1 wraps into row 2; row 2 ends its line.
        let wrapped = [false, true, false, false];
        assert_eq!(cursor_steps(&wrapped, 10, (4, 2), (1, 2)), Some(-3));
        assert_eq!(cursor_steps(&wrapped, 10, (4, 2), (8, 1)), Some(-6));
        assert_eq!(cursor_steps(&wrapped, 10, (2, 1), (5, 2)), Some(13));
        assert_eq!(cursor_steps(&wrapped, 10, (4, 2), (0, 3)), None);
        assert_eq!(cursor_steps(&wrapped, 10, (4, 1), (4, 0)), None);
    }
}
