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
    let mut session = if synthesized_only {
        crate::Connection::new_synthesized_compat(vt, cols, rows)
    } else {
        crate::Connection::new(vt, cols, rows)
    };
    // Mouse reports carry positions on the grid the canvas draws; the
    // viewport tells the server that grid's cell size.
    let (cell_w, cell_h) = metrics.cell_px();
    session.set_cell_size(cell_w, cell_h);
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
        bell_started_ms: Cell::new(f64::NEG_INFINITY),
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
        self.session.fail_protocol(message);
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
        self.bindings.get_mut().dispose();
        self.tx.close();
        self.reconnect.get_mut().take();
        self.self_owner.get_mut().take();
    }

    /// Project the focused pane's agent badges into the DOM: one
    /// `<span class="phux-agent-badge">` per `AgentSession` under a
    /// `#phux-agent-badges` container beside the canvas, hidden when empty.
    pub(super) fn paint_badges(&self) {
        let Some(container) = self.badge_container() else {
            return;
        };
        let badges = self.session.agent_badges();
        container.set_text_content(None);
        let Some(document) = container.owner_document() else {
            return;
        };
        for badge in &badges {
            let Ok(span) = document.create_element("span") else {
                continue;
            };
            span.set_class_name("phux-agent-badge");
            let _ = span.set_attribute("data-provider", &badge.provider);
            let _ = span.set_attribute("data-state", &badge.state);
            let provider = if badge.provider.is_empty() {
                "agent"
            } else {
                badge.provider.as_str()
            };
            let text = if badge.state.is_empty() || badge.state == "unknown" {
                provider.to_owned()
            } else {
                format!("{provider}: {}", badge.state)
            };
            span.set_text_content(Some(&text));
            let _ = container.append_child(&span);
        }
        let _ = container.set_attribute("hidden", "");
        if !badges.is_empty() {
            let _ = container.remove_attribute("hidden");
        }
    }

    /// The badge container, created next to the canvas on first use.
    fn badge_container(&self) -> Option<web_sys::Element> {
        let document = self.canvas.owner_document()?;
        if let Some(existing) = document.get_element_by_id(BADGE_CONTAINER_ID) {
            return Some(existing);
        }
        let container = document.create_element("div").ok()?;
        container.set_id(BADGE_CONTAINER_ID);
        let _ = container.set_attribute("hidden", "");
        let parent = self
            .canvas
            .parent_element()
            .or_else(|| document.body().map(Into::into))?;
        parent.append_child(&container).ok()?;
        Some(container)
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
        let _ = input.surface.set_attribute(
            "style",
            &format!("{INPUT_SURFACE_STYLE}left:{left}px;top:{top}px;"),
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
            self.ctx.save();
            let pane = self.focused_rect();
            let _ = self.ctx.translate(
                f64::from(pane.x) * self.metrics.cell_w,
                f64::from(pane.y) * self.metrics.cell_h,
            );
            self.ctx.begin_path();
            self.ctx.rect(
                0.0,
                0.0,
                f64::from(pane.cols) * self.metrics.cell_w,
                f64::from(pane.rows) * self.metrics.cell_h,
            );
            self.ctx.clip();
            render_cursor_row_appearance(
                &self.ctx,
                grid,
                &self.metrics,
                self.cursor_on.get(),
                &overlay,
                self.glass_mode.get(),
            );
            self.ctx.restore();
            self.paint_pane_border(&pane, true);
        }
    }
}
