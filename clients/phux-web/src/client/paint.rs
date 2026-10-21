//! Painting the canvas: panes, borders, selection, search marks, and published state.

use super::*;

impl App {
    pub(super) fn paint(&self) -> bool {
        if !self.session.render_visible() {
            return false;
        }
        let grid = self.session.grid();
        let marks = self.search_marks(grid.cols, grid.rows);
        let (cols, rows, panes) = self.pane_geometry();
        let geometry = crate::canvas::CanvasGeometry::new(
            &self.metrics,
            cols,
            rows,
            crate::canvas::device_pixel_ratio(),
        );
        if geometry.prepare(&self.canvas, &self.ctx).is_err() {
            return false;
        }
        self.canvas_geometry.set(geometry);
        self.paint_backdrop(&geometry);
        let focused = self.session.focused_pane();
        let mut cursor_rows = Vec::with_capacity(panes.len());
        for (id, rect) in &panes {
            let active = focused.as_ref() == Some(id);
            if let Some(row) = self.paint_pane(id, rect, active, &grid, &marks) {
                cursor_rows.push((id.clone(), row));
            }
            if panes.len() > 1 {
                self.paint_pane_border(rect, active);
            }
        }
        if self.bell_showing() {
            self.ctx.set_fill_style_str(BELL_FLASH_FILL);
            self.ctx
                .fill_rect(0.0, 0.0, geometry.width, geometry.height);
        }
        self.painted.replace(Some(grid));
        self.painted_cursor_rows.replace(cursor_rows);
        self.painted_marks.replace(marks);
        self.publish_title();
        self.place_input_surface();
        true
    }

    fn paint_backdrop(&self, geometry: &crate::canvas::CanvasGeometry) {
        if self.glass_mode.get() {
            self.ctx
                .clear_rect(0.0, 0.0, geometry.width, geometry.height);
        } else {
            self.ctx.set_fill_style_str("#0b0b0d");
            self.ctx
                .fill_rect(0.0, 0.0, geometry.width, geometry.height);
        }
    }

    fn paint_pane(
        &self,
        id: &phux_protocol::ids::ResourceId,
        rect: &crate::PaneRect,
        active: bool,
        grid: &phux_vt_web::Grid,
        marks: &[Mark],
    ) -> Option<u16> {
        self.ctx.save();
        let _ = self.ctx.translate(
            f64::from(rect.x) * self.metrics.cell_w,
            f64::from(rect.y) * self.metrics.cell_h,
        );
        self.ctx.begin_path();
        self.ctx.rect(
            0.0,
            0.0,
            f64::from(rect.cols) * self.metrics.cell_w,
            f64::from(rect.rows) * self.metrics.cell_h,
        );
        self.ctx.clip();
        let cursor_row = if active {
            let overlay = Overlay {
                selected: self.selected_cells(grid.cols),
                marks,
            };
            render_selected_appearance(
                &self.ctx,
                grid,
                &self.metrics,
                self.cursor_on.get() && !self.session.viewport_scrolled(),
                &overlay,
                self.glass_mode.get(),
            );
            Some(grid.cursor_row)
        } else if let Some(terminal) = self.session.pane_terminal(id) {
            let grid = terminal.grid();
            render_selected_appearance(
                &self.ctx,
                &grid,
                &self.metrics,
                false,
                &Overlay::default(),
                self.glass_mode.get(),
            );
            Some(grid.cursor_row)
        } else {
            None
        };
        self.ctx.restore();
        cursor_row
    }

    pub(super) fn paint_pane_border(&self, rect: &crate::PaneRect, active: bool) {
        if self.glass_mode.get() || self.session.pane_rects().len() <= 1 {
            return;
        }
        crate::canvas::paint_pane_border(&self.ctx, &self.metrics, rect, active);
    }

    /// Whether the visual bell is on screen now.
    pub(super) fn bell_showing(&self) -> bool {
        monotonic_now_ms()
            .is_ok_and(|now| now - self.bell_started_ms.get() < f64::from(BELL_FLASH_MS))
    }

    /// The highlights of the open search's matches on screen, re-running
    /// the search first when output has changed the screen since.
    fn search_marks(&self, cols: u16, rows: u16) -> Vec<Mark> {
        if self.search.borrow().query().is_empty() {
            return Vec::new();
        }
        if self.search_stale.get() {
            let query = self.search.borrow().query().to_owned();
            self.run_search(&query);
        }
        let Some(terminal) = self.session.terminal() else {
            return Vec::new();
        };
        let top = terminal.scrollbar().offset;
        self.search.borrow().marks(top, rows, cols)
    }

    /// Search the replica's whole screen for `query` and show the count.
    pub(super) fn run_search(&self, query: &str) {
        self.search_stale.set(false);
        let (matches, truncated) = match self.session.terminal() {
            Some(terminal) if !query.is_empty() => {
                let vt = self.session.vt();
                find_matches(&terminal.screen_rows(), query, |ch| vt.codepoint_width(ch))
            }
            _ => (Vec::new(), false),
        };
        let label = {
            let mut search = self.search.borrow_mut();
            search.set_results(query, matches, truncated);
            search.label()
        };
        find::set_label(self, &label);
    }

    /// Scroll the current match into view, if it is off screen, and repaint.
    /// Returns whether the viewport moved, so the caller can demand older
    /// history once the borrow is released.
    pub(super) fn reveal_current_match(&self) -> bool {
        let current = self.search.borrow().current();
        let mut moved = false;
        if let (Some(found), Some(terminal)) = (current, self.session.terminal()) {
            let bar = terminal.scrollbar();
            if let Some(top) = reveal_row(found.row, bar.offset, bar.len, bar.total) {
                terminal.scroll_to_row(top);
                self.clear_selection();
                moved = true;
            }
        }
        self.request_paint();
        moved
    }

    /// Row-major indices of the selected cells, empty without a selection.
    pub(super) fn selected_cells(&self, cols: u16) -> std::ops::Range<usize> {
        self.selection
            .get()
            .filter(|selection| !selection.is_click())
            .map_or(0..0, |selection| selection.cells(cols))
    }

    /// The selected text, if anything is selected, as the engine copies it.
    /// Read from the replica's current viewport, which the canvas shows by
    /// the next frame.
    pub(super) fn selected_text(&self) -> Option<String> {
        let selection = self.selection.get().filter(|s| !s.is_click())?;
        let (cols, _) = self.session.dims();
        let clamp = |(col, row): (u16, u16)| (col.min(cols.saturating_sub(1)), row);
        self.session
            .terminal()?
            .selection_text(clamp(selection.anchor), clamp(selection.head))
    }

    /// Drop the selection (it names viewport cells, which input, scrolling,
    /// and resizing move out from under it), repainting if one was shown.
    pub(super) fn clear_selection(&self) {
        if self.selection.take().is_some() {
            self.request_paint();
        }
    }

    /// A cell's drawn size on the page, in CSS pixels: the canvas's cell
    /// size scaled by however CSS, page zoom, or the device pixel ratio
    /// shows the canvas.
    pub(super) fn css_cell(&self, rect: &web_sys::DomRect) -> (f64, f64) {
        self.canvas_geometry
            .get()
            .css_cell(&self.metrics, rect.width(), rect.height())
    }

    /// The viewport cell under a pointer event.
    pub(super) fn cell_under(&self, event: &web_sys::MouseEvent) -> (u16, u16) {
        self.cell_at_point((event.client_x(), event.client_y()))
    }

    /// The viewport cell at a point in client (CSS pixel) coordinates.
    pub(super) fn cell_at_point(&self, (x, y): (f64, f64)) -> (u16, u16) {
        let rect = self.canvas.get_bounding_client_rect();
        let (css_w, css_h) = self.css_cell(&rect);
        let (cols, rows) = self.session.dims();
        cell_at(
            x - rect.left() - f64::from(self.focused_rect().x) * css_w,
            y - rect.top() - f64::from(self.focused_rect().y) * css_h,
            css_w,
            css_h,
            cols,
            rows,
        )
    }

    /// The pointer in pixels of the cell grid the viewport reports to the
    /// server ([`Metrics::cell_px`] per cell), clamped to the grid: what
    /// `INPUT_MOUSE` carries, and what the server's mouse encoder divides by
    /// the same cell size.
    pub(super) fn surface_pixels(&self, event: &web_sys::MouseEvent) -> (f64, f64) {
        let rect = self.canvas.get_bounding_client_rect();
        let (css_w, css_h) = self.css_cell(&rect);
        let (cell_w, cell_h) = self.metrics.cell_px();
        let (cols, rows) = self.session.dims();
        (
            crate::input::surface_pixel(
                event.client_x() - rect.left() - f64::from(self.focused_rect().x) * css_w,
                css_w,
                cell_w,
                cols,
            ),
            crate::input::surface_pixel(
                event.client_y() - rect.top() - f64::from(self.focused_rect().y) * css_h,
                css_h,
                cell_h,
                rows,
            ),
        )
    }

    pub(super) fn focused_rect(&self) -> crate::PaneRect {
        if self.session.pane_rects().len() <= 1 {
            let (cols, rows) = self.session.dims();
            return crate::PaneRect {
                x: 0,
                y: 0,
                cols,
                rows,
            };
        }
        let focused = self.session.focused_pane();
        self.session
            .pane_rects()
            .into_iter()
            .find(|(id, _)| Some(id) == focused.as_ref())
            .map_or_else(
                || {
                    let (cols, rows) = self.session.canvas_dims();
                    crate::PaneRect {
                        x: 0,
                        y: 0,
                        cols,
                        rows,
                    }
                },
                |(_, rect)| rect,
            )
    }

    pub(super) fn focus_changed(&mut self, previous: Option<phux_protocol::ids::ResourceId>) {
        let input_focused = self.bindings.borrow().input.as_ref().is_some_and(|input| {
            self.canvas
                .owner_document()
                .and_then(|document| document.active_element())
                .is_some_and(|active| active.is_same_node(Some(input.surface.as_ref())))
        });
        if input_focused {
            let frames = self.session.pane_focus_frames(previous);
            if let Err(message) = self.send(frames) {
                self.fail(&message);
                return;
            }
        }
        self.clear_selection();
        self.selecting.set(false);
        self.forwarded_button.set(None);
        self.mouse_cell.set(None);
        self.wheel_carry.set(0.0);
        self.search_stale.set(true);
        self.painted.replace(None);
        self.paint_badges();
        self.place_input_surface();
        self.request_paint();
    }

    /// The exact rectangles the renderer uses, including the published
    /// single-pane size while a server resize is in flight.
    fn pane_geometry(
        &self,
    ) -> (
        u16,
        u16,
        Vec<(phux_protocol::ids::ResourceId, crate::PaneRect)>,
    ) {
        let mut panes = self.session.pane_rects();
        let (cols, rows) = if panes.len() <= 1 {
            self.session.dims()
        } else {
            self.session.canvas_dims()
        };
        if panes.len() == 1 {
            panes[0].1.cols = cols;
            panes[0].1.rows = rows;
        }
        (cols, rows, panes)
    }

    pub(super) fn presentation_detail(&self) -> js_sys::Object {
        let (cols, rows, panes) = self.pane_geometry();
        let detail = js_sys::Object::new();
        let descriptors = js_sys::Array::new();
        for (id, rect) in panes {
            let pane = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&pane, &"id".into(), &JsValue::from_str(&id.to_string()));
            for (key, value) in [
                ("x", rect.x),
                ("y", rect.y),
                ("cols", rect.cols),
                ("rows", rect.rows),
            ] {
                let _ =
                    js_sys::Reflect::set(&pane, &key.into(), &JsValue::from_f64(f64::from(value)));
            }
            if let Some((_, row)) = self
                .painted_cursor_rows
                .borrow()
                .iter()
                .find(|(painted, _)| painted == &id)
            {
                let _ = js_sys::Reflect::set(
                    &pane,
                    &"cursorRow".into(),
                    &JsValue::from_f64(f64::from(*row)),
                );
            }
            descriptors.push(&pane);
        }
        let _ = js_sys::Reflect::set(&detail, &"panes".into(), &descriptors);
        let _ = js_sys::Reflect::set(&detail, &"cols".into(), &JsValue::from_f64(f64::from(cols)));
        let _ = js_sys::Reflect::set(&detail, &"rows".into(), &JsValue::from_f64(f64::from(rows)));
        detail
    }

    pub(super) fn publish_panes(&self) {
        let count = self.session.pane_rects().len();
        let focused = self
            .session
            .focused_pane()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let _ = self
            .canvas
            .set_attribute("data-phux-pane-count", &count.to_string());
        let _ = self
            .canvas
            .set_attribute("data-phux-focused-pane", &focused);
        let detail = self.presentation_detail();
        let _ = js_sys::Reflect::set(&detail, &"count".into(), &JsValue::from_f64(count as f64));
        let _ = js_sys::Reflect::set(&detail, &"focused".into(), &JsValue::from_str(&focused));
        let _ = js_sys::Reflect::set(
            &detail,
            &"pending".into(),
            &JsValue::from_bool(self.session.pane_pending()),
        );
        if let Some(error) = self.session.pane_error() {
            let _ = js_sys::Reflect::set(&detail, &"error".into(), &JsValue::from_str(error));
        }
        let init = web_sys::CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&detail);
        if let Ok(event) = web_sys::CustomEvent::new_with_event_init_dict("phux-panes", &init) {
            let _ = self.canvas.dispatch_event(&event);
        }
    }

    /// Announce the on-screen Ctrl latch, so a page's key bar shows it held
    /// or spent: a bubbling `phux-modifiers` event with `{ ctrl }`.
    pub(super) fn publish_modifiers(&self) {
        let detail = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            &detail,
            &"ctrl".into(),
            &JsValue::from_bool(self.ctrl_latch.get()),
        );
        let init = web_sys::CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&detail);
        if let Ok(event) = web_sys::CustomEvent::new_with_event_init_dict("phux-modifiers", &init) {
            let _ = self.canvas.dispatch_event(&event);
        }
    }

    /// Mirror a changed program title onto the canvas and announce it, so a
    /// page can show it (the standalone page sets its document title).
    fn publish_title(&self) {
        let title = self.session.title();
        if *self.title.borrow() == title {
            return;
        }
        let _ = self.canvas.set_attribute(TITLE_ATTRIBUTE, &title);
        let init = web_sys::CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&JsValue::from_str(&title));
        if let Ok(event) = web_sys::CustomEvent::new_with_event_init_dict(TITLE_EVENT, &init) {
            let _ = self.canvas.dispatch_event(&event);
        }
        self.title.replace(title);
    }
}
