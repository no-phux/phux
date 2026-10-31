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

use phux_client_runtime::Client;
use phux_client_runtime::engine::{MouseMode, Scroll};
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
    fn mouse_mode(&self, terminal: &ResourceId) -> MouseMode {
        self.client
            .as_ref()
            .and_then(|c| c.engine())
            .and_then(|e| e.mouse_mode(terminal).ok())
            .unwrap_or(MouseMode::None)
    }

    /// A press, drag or release over a pane.
    pub fn pointer(&mut self, terminal: &ResourceId, press: &Press, tx: &mut Tx) {
        if press.phase == Phase::Down {
            self.focus_pane(terminal);
        }
        let captured = self.mouse_capture.as_ref() == Some(terminal);
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
        self.track_drag(terminal, press, tx);
        self.select(terminal, press.phase, press.x, press.y, press.clicks);
    }

    fn report(&mut self, terminal: &ResourceId, press: &Press) {
        let Press { phase, x, y, .. } = *press;
        let Some(client) = self.client.clone() else {
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
                self.mouse_capture = Some(terminal.clone());
                self.mouse_cell = Some((col, row));
                MouseAction::Press
            }
            Phase::Move => {
                // Only motion-tracking programs hear drags, once per cell.
                let mode = self.mouse_mode(terminal);
                if !matches!(mode, MouseMode::Button | MouseMode::Any)
                    || self.mouse_cell == Some((col, row))
                {
                    return;
                }
                self.mouse_cell = Some((col, row));
                MouseAction::Motion
            }
            Phase::Up => {
                self.mouse_capture = None;
                self.mouse_cell = None;
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

    /// The secondary button's click at `at`, in a pane: the program's if it
    /// tracks the mouse, else the context menu.
    pub fn secondary_press(&mut self, terminal: &ResourceId, at: Press, window: PointerPress) {
        self.focus_pane(terminal);
        if self.mouse_mode(terminal) != MouseMode::None && !at.mods.shift {
            let phase = Phase::Down;
            self.report(terminal, &Press { phase, ..at });
            let phase = Phase::Up;
            self.report(terminal, &Press { phase, ..at });
            return;
        }
        self.pointer.menu = Some(window);
    }

    /// The pointer moved with nothing pressed: `MouseMode::Any` programs hear
    /// it, once per cell, while the grid is under it.
    pub(super) fn report_motion(&mut self, terminal: &ResourceId, x: f32, y: f32, mods: Mods) {
        if mods.shift || self.mouse_capture.is_some() || self.gesture != 0 {
            return;
        }
        let Some(client) = self.client.clone() else {
            return;
        };
        let Some(frame) = self.view_of(terminal).and_then(|v| client.acquire_view(v)) else {
            return;
        };
        let metrics = self.metrics();
        let cell = metrics.cell_at(x, y, frame.cols, frame.rows);
        if self.mouse_cell == Some(cell) {
            return;
        }
        self.mouse_cell = Some(cell);
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
        if self.mouse_capture.is_none() {
            self.mouse_cell = None;
        }
    }

    /// The surface pixels of the cell under a point in a pane's grid.
    fn surface_point(&self, terminal: &ResourceId, x: f32, y: f32) -> Option<(f64, f64)> {
        let frame = self
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
        if self.wheel_pane.as_ref() != Some(terminal) {
            self.wheel_pane = Some(terminal.clone());
            self.wheel_rows = 0.0;
        }
        let cell_h = self.metrics().cell_h;
        self.wheel_rows += dy / cell_h;
        let rows = self.wheel_rows.trunc();
        if rows == 0.0 {
            return;
        }
        self.wheel_rows -= rows;
        let steps = (rows as i64).clamp(-MAX_WHEEL_STEPS, MAX_WHEEL_STEPS);
        let Some(client) = self.client.clone() else {
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
            let key = if steps > 0 {
                PhysicalKey::ArrowUp
            } else {
                PhysicalKey::ArrowDown
            };
            for _ in 0..steps.abs() {
                let event = KeyEvent {
                    action: KeyAction::Press,
                    key,
                    mods: ModSet::empty(),
                    consumed_mods: ModSet::empty(),
                    composing: false,
                    text: None,
                    unshifted_codepoint: None,
                };
                client.with_control(|control| control.send_key(terminal, event));
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
}
