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
use rune::{Mods, Phase, Tx};

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
            button: MouseButton::Left,
            mods: keys::mods(press.mods),
            x,
            y,
        };
        client.with_control(|control| control.send_mouse(terminal, event));
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

/// Surface pixels at the middle of a cell, as the server divides by the
/// reported cell size.
fn cell_center(metrics: &crate::grid::Metrics, col: u16, row: u16) -> (f64, f64) {
    let (cell_w, cell_h) = metrics.cell_px();
    (
        f64::from(col) * f64::from(cell_w) + f64::from(cell_w) / 2.0,
        f64::from(row) * f64::from(cell_h) + f64::from(cell_h) / 2.0,
    )
}
