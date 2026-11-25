//! The connection's app state: sending, failure, badges, and building it.

use super::*;

impl Drop for App {
    fn drop(&mut self) {
        self.dispose();
    }
}

fn measured_canvas_context(
    canvas: &HtmlCanvasElement,
) -> Result<(CanvasRenderingContext2d, Metrics), JsValue> {
    let ctx: CanvasRenderingContext2d = canvas
        .get_context("2d")?
        .ok_or_else(|| JsValue::from_str("no 2D context"))?
        .dyn_into()?;
    let metrics = Metrics::measure(&ctx)?;
    crate::canvas::publish_metrics(canvas, &metrics)?;
    Ok((ctx, metrics))
}

/// Grab the canvas context and assemble an [`App`] around a preloaded engine
/// and established transport send half.
pub(super) fn build_app(
    vt: &Rc<Vt>,
    tx: WireTx,
    canvas: HtmlCanvasElement,
    cols: u16,
    rows: u16,
    synthesized_only: bool,
) -> Result<AppReady, JsValue> {
    let (ctx, metrics) = measured_canvas_context(&canvas)?;
    let (ready_tx, ready_rx) = oneshot::channel();
    let canvas_geometry = Cell::new(crate::canvas::CanvasGeometry::new(
        &metrics,
        cols,
        rows,
        crate::canvas::device_pixel_ratio(),
    ));
    // Pointer and wheel input read cells from the canvas's laid-out box
    // against this geometry, so give the box that size before the first
    // paint does; the browser's default canvas box would scale every cell.
    canvas_geometry.get().prepare(&canvas, &ctx)?;
    let mut session = if synthesized_only {
        crate::Connection::new_synthesized_compat(vt, cols, rows)
    } else {
        crate::Connection::new(vt, cols, rows)
    };
    // Mouse reports carry positions on the grid the canvas draws; the
    // viewport tells the server that grid's cell size.
    let (cell_w, cell_h) = metrics.cell_px();
    session.set_cell_size(cell_w, cell_h);
    let engine_palette = {
        let fresh = vt.terminal(1, 1).grid();
        (fresh.default_bg, fresh.default_fg)
    };
    let app = Rc::new(RefCell::new(App {
        session,
        tx,
        canvas,
        ctx,
        metrics,
        canvas_geometry,
        host_paths_enabled: !synthesized_only,
        hosted_edge: false,
        prefix: Cell::new(false),
        cursor_on: Cell::new(true),
        glass_mode: Cell::new(false),
        wheel_carry: Cell::new(0.0),
        painted: RefCell::new(None),
        painted_cursor_rows: RefCell::new(Vec::with_capacity(4)),
        title: RefCell::new(String::new()),
        painted_marks: RefCell::new(Vec::new()),
        selection: Cell::new(None),
        selecting: Cell::new(false),
        search: RefCell::new(Search::default()),
        search_stale: Cell::new(false),
        forwarded_button: Cell::new(None),
        mouse_cell: Cell::new(None),
        link_hover: Cell::new(false),
        touch: Cell::new(None),
        ctrl_latch: Cell::new(false),
        bell: Cell::new(crate::bell::BellGate::default()),
        bell_unread: Cell::new(false),
        badges: RefCell::new(badges::BadgeMemory::default()),
        engine_palette,
        bindings: RefCell::new(AppBindings::default()),
        ready: RefCell::new(Some(ready_tx)),
        failure_reason: RefCell::new(None),
        reconnect: RefCell::new(None),
        self_owner: RefCell::new(None),
    }));
    install_bootstrap_expiry(&app)?;
    install_frame(&app)?;
    Ok((app, ready_rx))
}

impl App {
    pub(super) fn send(&self, frames: Vec<Vec<u8>>) -> Result<(), String> {
        for f in frames
            .into_iter()
            .filter(|frame| self.permits_outbound(frame))
        {
            self.tx.send(&f)?;
        }
        Ok(())
    }
    /// Settle actual transport handoffs, not frame construction. Only a
    /// definitely unhanded suffix may undo the journal's attempt bookkeeping.
    pub(super) fn flush_input(&self) -> Result<(), String> {
        let frames = self.session.input_attempts();
        for (index, frame) in frames.iter().enumerate() {
            let encoded = crate::session::encode(frame);
            if let Err((message, attempted)) = self.tx.handoff_input(&encoded) {
                self.session
                    .rollback_input_unsent(&frames[index + usize::from(attempted)..]);
                self.publish_input_reports();
                return Err(message);
            }
        }
        self.publish_input_reports();
        Ok(())
    }

    pub(super) fn publish_input_reports(&self) {
        for (delivery_id, report) in self.session.take_input_reports() {
            self.announce(&report.notice_line());
            publish_input_report(&self.canvas, delivery_id, &report);
        }
    }

    /// Apply the backend policy before HELLO, without changing codec capabilities.
    pub(super) fn configure_hosted_backend(&mut self, preamble: &JsValue) {
        self.hosted_edge = js_sys::Reflect::get(preamble, &JsValue::from_str("backend"))
            .ok()
            .and_then(|backend| backend.as_string())
            .as_deref()
            == Some("edge");
        self.host_paths_enabled &= !self.hosted_edge;
    }

    /// SUBSCRIBE_EVENTS is a base native operation (not EventJournal replay).
    /// The curated edge shell has no event stream. Omit both startup and kernel
    /// post-ready registrations there; never hide its inbound errors or alter
    /// native subscriptions, even when the negotiated codec is synthesized.
    pub(super) fn permits_outbound(&self, frame: &[u8]) -> bool {
        !self.hosted_edge
            || frame.get(4) != Some(&phux_protocol::wire::frame::TYPE_SUBSCRIBE_EVENTS)
    }

    pub(super) fn signal_ready(&self) {
        if let Some(ready) = self.ready.borrow_mut().take() {
            self.mark_connection("connected");
            let _ = ready.send(Ok(()));
        }
    }

    /// Reflect the connection on the canvas as [`CONNECTION_ATTRIBUTE`], so
    /// a page can dim or badge a stale terminal while a reconnect is pending.
    fn mark_connection(&self, state: &str) {
        let _ = self.canvas.set_attribute(CONNECTION_ATTRIBUTE, state);
    }

    fn signal_failed(&self, message: &str) {
        if let Some(ready) = self.ready.borrow_mut().take() {
            let _ = ready.send(Err(message.to_owned()));
        }
    }

    pub(super) fn fail(&mut self, message: &str) {
        if self.failure_reason.borrow().is_some() {
            return;
        }
        self.failure_reason.replace(Some(message.to_owned()));
        // The transport is gone: the agents on screen are lost, not still
        // working. Painted before the kernel is failed.
        self.paint_badges();
        let pane_pending = self.session.pane_pending();
        self.session.stop_input_delivery(
            self.reconnect.borrow().is_some() && !self.session.is_version_refused(),
        );
        self.publish_input_reports();
        self.session.fail_protocol(message);
        if pane_pending && !self.session.pane_pending() {
            self.publish_panes();
        }
        super::approvals::retain_uncertainty(&self.canvas, self.session.approval_status());
        let refused = self.session.is_version_refused();
        if refused {
            mark_incompatible(&self.canvas, message);
        } else {
            self.mark_connection("disconnected");
        }
        self.signal_failed(message);
        self.bindings.get_mut().dispose();
        self.tx.close();
        let reconnect = self.reconnect.get_mut().take();
        self.self_owner.get_mut().take();
        // A protocol-major refusal answers every redial the same way.
        if let Some(config) = reconnect.filter(|_| !refused) {
            spawn_reconnect(config);
        }
    }

    pub(super) fn dispose(&mut self) {
        if !self.session.is_failed() {
            self.session.stop_input_delivery(false);
            self.publish_input_reports();
        }
        self.dispose_documents();
        self.bindings.get_mut().dispose();
        self.tx.close();
        self.reconnect.get_mut().take();
        self.self_owner.get_mut().take();
    }

    /// Move the input surface over the cursor cell, so an IME candidate
    /// window opens where the text will land.
    pub(super) fn place_input_surface(&self) {
        let bindings = self.bindings.borrow();
        let Some(input) = bindings.input.as_ref() else {
            return;
        };
        let rect = self.canvas.get_bounding_client_rect();
        let painted = self.painted.borrow();
        let current;
        let grid = if let Some(grid) = painted.as_ref() {
            grid
        } else {
            current = self.session.grid();
            &current
        };
        let (css_w, css_h) = self.css_cell(&rect);
        let pane = self.focused_rect();
        let left = rect.left() + f64::from(pane.x + grid.cursor_col) * css_w;
        let top = rect.top() + f64::from(pane.y + grid.cursor_row) * css_h;
        // Client-rect coordinates are physical, so this one box is placed
        // with the physical `inset` shorthand (top, right, bottom, left).
        let _ = input.surface.set_attribute(
            "style",
            &format!("{INPUT_SURFACE_STYLE}inset:{top}px auto auto {left}px;"),
        );
    }

    /// Paint on the next animation frame. Frames arriving in a burst (a
    /// flood of output, a WebTransport chunk of many frames) share one
    /// paint, and a hidden tab paints nothing until it is shown again.
    pub(super) fn request_paint(&self) {
        if let Some(frame) = self.bindings.borrow().frame.as_ref() {
            frame.request();
        }
    }

    /// Toggle the cursor blink phase and redraw only the cursor's row.
    pub(super) fn blink(&self) {
        if self.canvas_geometry.get().dpr != crate::canvas::device_pixel_ratio() {
            self.request_paint();
            return;
        }
        self.cursor_on.set(!self.cursor_on.get());
        if self.session.viewport_scrolled() {
            return;
        }
        if self.bell_showing() {
            // The flash covers the cursor cell too; its end repaints it.
            return;
        }
        if let Some(grid) = self.painted.borrow().as_ref() {
            let marks = self.painted_marks.borrow();
            let overlay = Overlay {
                selected: self.selected_cells(grid.cols),
                marks: &marks,
            };
            let pane = self.focused_rect();
            self.clip_to_pane(&pane);
            render_cursor_row(
                &self.ctx,
                grid,
                &self.metrics,
                self.cursor_on.get(),
                &overlay,
                self.glass_mode.get(),
            );
            self.ctx.restore();
            self.paint_pane_border(&pane, true, &crate::ink::roles_of(grid));
        }
    }
}

pub(super) fn publish_input_report(
    canvas: &HtmlCanvasElement,
    delivery_id: u64,
    report: &phux_client_core::input_replay::ReplayReport,
) {
    use phux_client_core::input_replay::ReplayDisposition;
    let status = match report.disposition {
        ReplayDisposition::Delivered => "delivered",
        ReplayDisposition::Refused => "refused",
        ReplayDisposition::Unknown => "unknown",
    };
    let notice = report.notice_line();
    let _ = canvas.set_attribute("data-phux-input-status", status);
    let _ = canvas.set_attribute("data-phux-input-delivery-id", &delivery_id.to_string());
    let _ = canvas.set_attribute("data-phux-input-operation", &report.operation_id);
    let _ = canvas.set_attribute("data-phux-input-notice", &notice);
    show_input_notice(canvas, &notice);
    let detail: JsValue = js_sys::Object::new().into();
    set_js(&detail, "status", status);
    set_js(&detail, "deliveryId", delivery_id.to_string());
    set_js(&detail, "operationId", report.operation_id.clone());
    set_js(&detail, "message", report.message.clone());
    let target = canvas.clone();
    wasm_bindgen_futures::spawn_local(async move {
        dispatch_custom(&target, INPUT_DELIVERY_EVENT, Some(&detail));
    });
    if report.disposition != ReplayDisposition::Delivered {
        web_sys::console::warn_1(&JsValue::from_str(&notice));
    }
}

pub(super) fn show_input_notice(canvas: &HtmlCanvasElement, notice: &str) {
    if let Some(document) = canvas.owner_document() {
        let node = document.get_element_by_id("phux-input-notice").or_else(|| {
            let node = document.create_element("output").ok()?;
            node.set_id("phux-input-notice");
            node.set_attribute("role", "status").ok()?;
            canvas.parent_element()?.append_child(&node).ok()?;
            Some(node)
        });
        if let Some(node) = node {
            node.set_text_content(Some(notice));
        }
    }
}
