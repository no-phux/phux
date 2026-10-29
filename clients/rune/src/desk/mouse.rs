//! The pointer over a pane: reported to a program that tracks the mouse,
//! else local selection and history scrolling, as Ghostty does.
//!
//! - Shift keeps local selection even while a program tracks the mouse.
//! - A press the program took always gets its release, wherever it lands.
//! - Motion is sent once per cell, and only to programs that asked for it.
//! - The wheel is buttons 4 and 5 to a tracking program, arrow keys in the
//!   alternate screen (less, man), and history everywhere else.

use phux_client_runtime::engine::{MouseMode, Scroll};
use phux_protocol::ids::ResourceId;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use rune::{Mods, Phase};

use super::Desk;
use crate::keys;

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

    /// A press, drag or release over a pane, at `x, y` in its grid's points.
    pub fn pointer(
        &mut self,
        terminal: &ResourceId,
        phase: Phase,
        x: f32,
        y: f32,
        clicks: u32,
        mods: Mods,
    ) {
        if phase == Phase::Down {
            self.focus_pane(terminal);
        }
        let captured = self.mouse_capture.as_ref() == Some(terminal);
        let tracking = self.mouse_mode(terminal) != MouseMode::None && !mods.shift;
        if captured || (phase == Phase::Down && tracking) {
            self.report(terminal, phase, x, y, mods);
            return;
        }
        self.select(phase, x, y, clicks);
    }

    fn report(&mut self, terminal: &ResourceId, phase: Phase, x: f32, y: f32, mods: Mods) {
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
        let (cell_w, cell_h) = metrics.cell_px();
        let event = MouseEvent {
            action,
            button: MouseButton::Left,
            mods: keys::mods(mods),
            // Surface pixels inside the cell the point is in, clamped to the grid.
            x: f64::from(col) * f64::from(cell_w) + f64::from(cell_w) / 2.0,
            y: f64::from(row) * f64::from(cell_h) + f64::from(cell_h) / 2.0,
        };
        client.with_control(|control| control.send_mouse(terminal, event));
    }

    /// Wheel over a pane: `dy` points, positive revealing older rows.
    pub fn wheel(&mut self, terminal: &ResourceId, dy: f32, mods: Mods) {
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
            for _ in 0..steps.abs() {
                let event = MouseEvent {
                    action: MouseAction::Press,
                    button,
                    mods: keys::mods(mods),
                    x: 0.0,
                    y: 0.0,
                };
                client.with_control(|control| control.send_mouse(terminal, event));
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
