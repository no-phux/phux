//! The pointer over a pane: reported to a program that tracks the mouse,
//! else local selection and history scrolling, as Ghostty does.
//!
//! - Shift keeps local selection even while a program tracks the mouse.
//! - A press the program took always gets its release, wherever it lands.
//! - Motion is sent once per cell (device pixel under DEC 1016), only on request.
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

/// Match the application's coordinate precision, not the local selection grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    Cell(u16, u16),
    Pixel(u32, u32),
}

impl Position {
    fn surface(self, metrics: &crate::grid::Metrics) -> (f64, f64) {
        match self {
            Self::Cell(col, row) => cell_center(metrics, col, row),
            Self::Pixel(x, y) => (f64::from(x), f64::from(y)),
        }
    }
}

/// Motion in another pane, coordinate mode, or modifier/button state is new.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReportedPosition {
    terminal: ResourceId,
    position: Position,
    mods: ModSet,
    button: MouseButton,
}

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
        let Some(position) = self.surface_point(terminal, x, y) else {
            return;
        };
        let button = wire_button(press.button);
        let mods = keys::mods(press.mods);
        let action = match phase {
            Phase::Down => {
                self.connection.mouse_capture = Some((terminal.clone(), *press));
                self.remember_position(terminal, position, mods, button);
                MouseAction::Press
            }
            Phase::Move => {
                self.connection.mouse_capture = Some((terminal.clone(), *press));
                let mode = self.mouse_mode(terminal);
                if !matches!(mode, MouseMode::Button | MouseMode::Any)
                    || !self.remember_position(terminal, position, mods, button)
                {
                    return;
                }
                MouseAction::Motion
            }
            Phase::Up => {
                self.connection.mouse_capture = None;
                self.connection.mouse_position = None;
                MouseAction::Release
            }
        };
        let (x, y) = position.surface(&self.metrics());
        let event = MouseEvent {
            action,
            button,
            mods,
            x,
            y,
        };
        send_mouse(&client, terminal, event);
    }

    fn remember_position(
        &mut self,
        terminal: &ResourceId,
        position: Position,
        mods: ModSet,
        button: MouseButton,
    ) -> bool {
        let next = ReportedPosition {
            terminal: terminal.clone(),
            position,
            mods,
            button,
        };
        let changed = self.connection.mouse_position.as_ref() != Some(&next);
        self.connection.mouse_position = Some(next);
        changed
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
            self.connection.mouse_position = None;
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
                self.connection.mouse_position.clone(),
            );
            let phase = Phase::Down;
            self.report(terminal, &Press { phase, ..at });
            let phase = Phase::Up;
            self.report(terminal, &Press { phase, ..at });
            (
                self.connection.mouse_capture,
                self.connection.mouse_position,
            ) = held;
            return;
        }
        self.pointer.menu = Some(window);
    }

    /// The pointer moved with nothing pressed: `MouseMode::Any` programs hear
    /// it at the precision they requested, while the grid is under it.
    pub(super) fn report_motion(&mut self, terminal: &ResourceId, x: f32, y: f32, mods: Mods) {
        if mods.shift || self.connection.mouse_capture.is_some() || self.connection.gesture != 0 {
            return;
        }
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        if self.mouse_mode(terminal) != MouseMode::Any {
            return;
        }
        let Some(position) = self.hover_position(terminal, x, y) else {
            return;
        };
        let mods = keys::mods(mods);
        if !self.remember_position(terminal, position, mods, MouseButton::Unknown) {
            return;
        }
        let (x, y) = position.surface(&self.metrics());
        let event = MouseEvent {
            action: MouseAction::Motion,
            button: MouseButton::Unknown,
            mods,
            x,
            y,
        };
        send_mouse(&client, terminal, event);
    }

    fn hover_position(&self, terminal: &ResourceId, x: f32, y: f32) -> Option<Position> {
        let client = self.connection.client.as_ref()?;
        let frame = client.acquire_view(self.view_of(terminal)?)?;
        let metrics = self.metrics();
        let inside = x >= 0.0
            && y >= 0.0
            && x < f32::from(frame.cols) * metrics.cell_w
            && y < f32::from(frame.rows) * metrics.cell_h;
        inside.then(|| self.surface_point(terminal, x, y)).flatten()
    }

    /// Nothing is under the pointer any more.
    pub(super) fn forget_mouse_position(&mut self) {
        if self.connection.mouse_capture.is_none() {
            self.connection.mouse_position = None;
        }
    }

    /// A point in the application's requested coordinate space, clamped to its grid.
    fn surface_point(&self, terminal: &ResourceId, x: f32, y: f32) -> Option<Position> {
        let client = self.connection.client.as_ref()?;
        let frame = client.acquire_view(self.view_of(terminal)?)?;
        let pixels = client.engine()?.pixel_mouse(terminal).unwrap_or(false);
        Some(tracking_position(
            &self.metrics(),
            (frame.cols, frame.rows),
            (x, y),
            pixels,
        ))
    }

    /// Wheel over a pane at `x, y` in its grid's points: `dy` points,
    /// positive revealing older rows.
    pub fn wheel(&mut self, terminal: &ResourceId, x: f32, y: f32, dy: f32, mods: Mods) {
        let steps = self.wheel_steps(terminal, dy);
        if steps == 0 {
            return;
        }
        let Some(client) = self.connection.client.clone() else {
            return;
        };
        if self.mouse_mode(terminal) != MouseMode::None && !mods.shift {
            self.report_wheel(&client, terminal, (x, y), steps, mods);
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

    fn report_wheel(
        &self,
        client: &Client,
        terminal: &ResourceId,
        (x, y): (f32, f32),
        steps: i64,
        mods: Mods,
    ) {
        let button = if steps > 0 {
            MouseButton::Four
        } else {
            MouseButton::Five
        };
        // Programs route the wheel by position (vim splits, browser widgets).
        let (x, y) = self
            .surface_point(terminal, x, y)
            .map(|position| position.surface(&self.metrics()))
            .unwrap_or((0.0, 0.0));
        let event = MouseEvent {
            action: MouseAction::Press,
            button,
            mods: keys::mods(mods),
            x,
            y,
        };
        for _ in 0..steps.abs() {
            send_mouse(client, terminal, event);
        }
    }

    fn wheel_steps(&mut self, terminal: &ResourceId, dy: f32) -> i64 {
        // Leftover fractions belong to the pane they came from.
        if self.connection.wheel_pane.as_ref() != Some(terminal) {
            self.connection.wheel_pane = Some(terminal.clone());
            self.connection.wheel_rows = 0.0;
        }
        self.connection.wheel_rows += dy / self.metrics().cell_h;
        let rows = self.connection.wheel_rows.trunc();
        self.connection.wheel_rows -= rows;
        (rows as i64).clamp(-MAX_WHEEL_STEPS, MAX_WHEEL_STEPS)
    }
}

fn tracking_position(
    metrics: &crate::grid::Metrics,
    (cols, rows): (u16, u16),
    (x, y): (f32, f32),
    pixels: bool,
) -> Position {
    if !pixels {
        let (col, row) = metrics.cell_at(x, y, cols, rows);
        return Position::Cell(col, row);
    }
    let (cw, ch) = metrics.cell_px();
    let axis = |point: f32, cell: f32, px: u16, cells: u16| {
        (f64::from(point) / f64::from(cell) * f64::from(px))
            .floor()
            .clamp(0.0, f64::from(u32::from(px) * u32::from(cells)) - 1.0) as u32
    };
    Position::Pixel(
        axis(x, metrics.cell_w, cw, cols),
        axis(y, metrics.cell_h, ch, rows),
    )
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
mod tracking_tests;

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
