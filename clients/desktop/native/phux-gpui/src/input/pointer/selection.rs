//! Mouse selection: the drag gesture, its autoscroll past the grid, and
//! Linux's primary selection.

use super::*;

impl TerminalInput {
    /// Whether a selection drag or a captured application button owns the
    /// pointer, so motion anywhere in the window belongs to this view.
    pub fn capturing(&self) -> bool {
        self.gesture != 0 || self.reported_button.is_some()
    }

    /// Extend the selection to the pointer, inside the pane or past its
    /// edge. Motion is quantized to fifths of a cell, not whole cells, so
    /// the half-cell rule that decides whether the cell under the pointer is
    /// in the selection sees the position the person released on. Past the
    /// top or bottom edge the view keeps scrolling while the pointer stays
    /// there.
    pub(super) fn drag_selection(
        &mut self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<bool, InputError> {
        self.admit_focused(window, cx, false)?;
        self.autoscroll_from(position, modifiers, cx);
        let cell = self
            .surface(position, true)
            .ok()
            .map(|surface| drag_cell(&surface));
        if cell.is_some() && cell == self.drag_cell {
            return Ok(true);
        }
        self.drag_cell = cell;
        self.select(1, 1, position, modifiers)?;
        cx.notify();
        Ok(true)
    }

    /// Start, steer, or stop scrolling for a drag held past the grid.
    fn autoscroll_from(
        &mut self,
        position: Point<Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let rows = self.metrics.map_or(0, |metrics| {
            autoscroll_rows(metrics.bounds, metrics.line_height, position.y)
        });
        if rows == 0 {
            self.autoscroll = None;
            return;
        }
        let running = self.autoscroll.is_some();
        self.autoscroll = Some((position, modifiers));
        if running {
            return;
        }
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_TICK).await;
                let more = this
                    .update(cx, |state, cx| state.autoscroll_tick(cx))
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        })
        .detach();
    }

    /// One autoscroll step: scroll toward the pointer, then extend the
    /// selection over the rows that scrolled in. False stops the timer.
    pub(in crate::input) fn autoscroll_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((position, modifiers)) = self.autoscroll.filter(|_| self.gesture != 0) else {
            self.autoscroll = None;
            return false;
        };
        let Some(metrics) = self.metrics else {
            self.autoscroll = None;
            return false;
        };
        let rows = autoscroll_rows(metrics.bounds, metrics.line_height, position.y);
        let Ok(client) = self.client() else {
            self.autoscroll = None;
            return false;
        };
        let offset = |client: &Client| {
            client
                .acquire_view(self.view)
                .map(|frame| frame.scrollbar.offset)
        };
        // Held past an end the view cannot scroll toward: wait without a
        // runtime turn per tick.
        let at_end = client.acquire_view(self.view).is_some_and(|frame| {
            if rows < 0 {
                frame.scrollbar.offset == 0
            } else {
                frame.scrollbar.at_tail()
            }
        });
        if at_end {
            return true;
        }
        let before = offset(&client);
        let scrolled = self.with_current(false, |control| {
            control
                .scroll_view(self.view, Scroll::Delta(rows))
                .map_err(engine_error)
        });
        if scrolled.is_err() {
            self.autoscroll = None;
            return false;
        }
        if offset(&client) != before {
            self.scroll_fraction = None;
            self.drag_cell = None;
            if self.select(1, 1, position, modifiers).is_ok() {
                cx.notify();
            }
        }
        true
    }

    /// Paste Linux's primary selection on a middle click the program does
    /// not take. Elsewhere a middle click is the window's.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    pub(super) fn paste_primary(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(item) = cx.read_from_primary() else {
            return false;
        };
        if let Some(notice) = self.paste_item(&item, true, cx) {
            cx.emit(super::InputEvent::Notice(notice));
        }
        cx.notify();
        true
    }

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    pub(super) fn paste_primary(&mut self, _: &mut Context<Self>) -> bool {
        false
    }

    pub(super) fn select(
        &mut self,
        phase: u32,
        clicks: u32,
        position: Point<Pixels>,
        modifiers: Modifiers,
    ) -> Result<(), InputError> {
        // Clamped: a drag that leaves the pane stays on the edge cell. Pixels
        // are the integer grid `cell_px` reports, not the pane's logical size.
        let surface = self.surface(position, true)?;
        let result = self.with_current(false, |control| {
            let frame = self.live_frame(control)?;
            let event = SelectionGestureEvent {
                phase,
                clicks: clicks.clamp(1, 3),
                handle: self.gesture,
                column: surface.col,
                row: u32::from(surface.row),
                rectangle: modifiers.alt,
                x: surface.x,
                y: surface.y,
                columns: u32::from(frame.cols),
                cell_width: surface.cell_width,
                screen_height: u32::from(frame.rows)
                    .saturating_mul(surface.cell_height)
                    .max(1),
                padding_left: 0,
            };
            control
                .engine()
                .ok_or(InputError::NotReady)?
                .view_selection_gesture(self.view, event)
                .map_err(engine_error)
        })?;
        self.gesture = result.handle;
        Ok(())
    }
}

/// How often a drag held past the grid scrolls.
const AUTOSCROLL_TICK: std::time::Duration = std::time::Duration::from_millis(40);

#[cfg(test)]
pub(in crate::input) const fn tick() -> std::time::Duration {
    AUTOSCROLL_TICK
}

/// The farthest one autoscroll tick moves, in rows.
const AUTOSCROLL_MAX_ROWS: f32 = 8.;

/// Rows one autoscroll tick scrolls for a pointer at window `y`: negative
/// toward history above the grid, positive toward live output below it,
/// zero inside. Speed grows with how far past the edge the pointer is.
pub(super) fn autoscroll_rows(bounds: Bounds<Pixels>, line_height: Pixels, y: Pixels) -> i64 {
    let past = if y < bounds.top() {
        bounds.top() - y
    } else if y > bounds.bottom() {
        y - bounds.bottom()
    } else {
        return 0;
    };
    let lines = f32::from(past) / f32::from(line_height).max(1.);
    let rows = lines.ceil().clamp(1., AUTOSCROLL_MAX_ROWS) as i64;
    if y < bounds.top() { -rows } else { rows }
}

/// A drag position quantized to its cell and the fifth of that cell's width
/// it sits in.
fn drag_cell(surface: &SurfacePoint) -> (u16, u16, u8) {
    let width = f64::from(surface.cell_width.max(1));
    let inside = surface.x - f64::from(surface.col) * width;
    let fifth = (inside / width * 5.).floor().clamp(0., 4.) as u8;
    (surface.row, surface.col, fifth)
}
