//! The wheel: smooth local scrollback, or whole-line presses for a program
//! that reports the mouse.

use super::*;

impl TerminalInput {
    /// Scroll the view under the pointer. Like every macOS scroll view, the
    /// wheel follows the pointer, not keyboard focus, so it needs only this
    /// window.
    ///
    /// A local scroll keeps a continuous position: whole rows move the
    /// runtime view, and the remaining fraction of a row stays here for the
    /// painter, which shifts the grid up by it and shows the frame's fringe
    /// row underneath. A move that stays within the current row takes no
    /// runtime turn. A program that reports the mouse gets whole-line wheel
    /// presses instead, accumulated across events.
    pub fn scroll(
        &mut self,
        event: &ScrollEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, InputError> {
        if window.window_handle().window_id() != self.window {
            return Err(InputError::WrongFocus);
        }
        let metrics = self.metrics.ok_or(InputError::StalePresentation)?;
        if !self.hit(event.position) {
            return Ok(false);
        }
        if event.touch_phase == TouchPhase::Started {
            self.scroll_remainder = 0.0;
        }
        let delta = event.delta.pixel_delta(metrics.line_height);
        let rows_up = f32::from(delta.y) / f32::from(metrics.line_height).max(1.0);
        let client = self.client()?;
        let frame = client
            .acquire_view(self.view)
            .ok_or(InputError::StalePresentation)?;
        if self.reports_wheel(&client, &frame, event.modifiers)? {
            self.scroll_fraction = None;
            self.scroll_remainder += rows_up;
            let lines = self.scroll_remainder.trunc() as i64;
            self.scroll_remainder -= lines as f32;
            if lines != 0 {
                self.with_current(false, |control| self.report_scroll(control, lines, event))?;
            }
            return Ok(true);
        }
        let bar = frame.scrollbar;
        let tail = bar.total.saturating_sub(bar.len);
        let (offset, fraction) = smooth_target(bar.offset, self.fraction_of(&frame), tail, rows_up);
        if offset != bar.offset {
            let rows = i64::try_from(offset).unwrap_or(i64::MAX)
                - i64::try_from(bar.offset).unwrap_or(i64::MAX);
            self.with_current(false, |control| {
                control
                    .scroll_view(self.view, Scroll::Delta(rows))
                    .map_err(engine_error)
            })?;
        }
        let landed = client
            .acquire_view(self.view)
            .map_or(offset, |frame| frame.scrollbar.offset);
        let fraction = if landed == offset { fraction } else { 0.0 };
        self.scroll_fraction = (fraction > 0.0).then_some((landed, fraction));
        cx.notify();
        Ok(true)
    }

    /// The part of a row above `frame`'s viewport that the painter scrolls
    /// past, in `[0, 1)`. Zero unless the last local scroll landed on this
    /// frame's offset and the frame carries the row below its viewport.
    pub fn fraction_of(&self, frame: &GridFrame) -> f32 {
        match self.scroll_fraction {
            Some((offset, fraction))
                if offset == frame.scrollbar.offset
                    && frame.buffer.fringe.len() == usize::from(frame.cols) =>
            {
                fraction
            }
            _ => 0.0,
        }
    }

    /// Whether the program receives the wheel. Mouse modes change only with
    /// terminal output, so the answer holds while the replica's sequence
    /// does and a scroll through idle output asks the runtime once.
    fn reports_wheel(
        &mut self,
        client: &Client,
        frame: &GridFrame,
        modifiers: Modifiers,
    ) -> Result<bool, InputError> {
        if modifiers.shift {
            return Ok(false);
        }
        let key = (frame.stream_id, frame.bootstrap_id, frame.last_seq);
        if let Some((cached, reports)) = self.wheel_mode
            && cached == key
        {
            return Ok(reports);
        }
        let reports = client.with_control(|control| reporting(control, &self.terminal))?;
        self.wheel_mode = Some((key, reports));
        Ok(reports)
    }

    fn report_scroll(
        &self,
        control: &mut ControlPlane,
        lines: i64,
        event: &ScrollEvent,
    ) -> Result<(), InputError> {
        if !input_ready(control, &self.terminal) {
            return Err(InputError::NotReady);
        }
        let button = if lines > 0 {
            WireButton::Four
        } else {
            WireButton::Five
        };
        let surface = self.surface(event.position, false)?;
        for _ in 0..lines.unsigned_abs().min(100) {
            self.send_mouse(
                control,
                MouseAction::Press,
                button,
                event.modifiers,
                surface,
            )?;
        }
        Ok(())
    }
}

/// Where a local scroll of `rows_up` rows (toward history when positive)
/// lands from `offset` plus `fraction`: the whole-row offset to show and the
/// part of a row above it to scroll past. The position stays within the
/// scrollback, and the live tail never shows a fraction.
pub(super) fn smooth_target(offset: u64, fraction: f32, tail: u64, rows_up: f32) -> (u64, f32) {
    let position =
        (offset as f64 + f64::from(fraction) - f64::from(rows_up)).clamp(0.0, tail as f64);
    let row = position.floor();
    (row as u64, (position - row) as f32)
}
