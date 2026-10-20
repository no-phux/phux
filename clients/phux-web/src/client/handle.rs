//! The live [`Client`] handle a page holds.

use super::*;

impl Client {
    /// Keep this connection alive past transport loss: after a failure, a
    /// fresh client redials (WebTransport first when `wt_url` is given,
    /// else the WebSocket) until one attaches, then supervises itself the
    /// same way. The client is retained by its own handlers from here on.
    pub fn enable_auto_reconnect(&self, wt_url: Option<&str>, ws_url: &str) {
        let (canvas, cols, rows) = {
            let app = self.app.borrow();
            let grid = app.session.grid();
            (app.canvas.clone(), grid.cols, grid.rows)
        };
        let config = ReconnectConfig {
            wt_url: wt_url.map(str::to_owned),
            ws_url: ws_url.to_owned(),
            canvas,
            cols,
            rows,
        };
        let mut app = self.app.borrow_mut();
        if app.session.is_failed() {
            drop(app);
            spawn_reconnect(config);
            return;
        }
        app.reconnect.get_mut().replace(config);
        app.self_owner.get_mut().replace(Rc::clone(&self.app));
    }

    pub(crate) fn retain_until_failure(&self) {
        let mut app = self.app.borrow_mut();
        if !app.session.is_failed() {
            app.self_owner.get_mut().replace(Rc::clone(&self.app));
            #[cfg(test)]
            RETAINED_APP_FOR_TEST.with(|retained| {
                retained.replace(Some(Rc::downgrade(&self.app)));
            });
        }
    }

    /// The current styled grid as one `String` per row (for inspection/tests).
    #[must_use]
    pub fn rows_text(&self) -> Vec<String> {
        let grid = self.app.borrow().session.grid();
        let cols = usize::from(grid.cols);
        grid.cells
            .chunks(cols.max(1))
            .map(|row| row.iter().map(|c| c.ch).collect::<String>())
            .collect()
    }

    /// Exact profile selected by the validated server handshake.
    #[must_use]
    pub fn selected_profile(&self) -> Option<BootstrapProfile> {
        self.app.borrow().session.selected_profile()
    }

    /// Agent badges the DOM currently shows for the focused pane.
    #[must_use]
    pub fn agent_badges(&self) -> Vec<crate::AgentBadge> {
        self.app.borrow().session.agent_badges()
    }

    /// Whether the transport or protocol has failed since readiness. Callers
    /// can create a fresh client with the same URLs to reconnect.
    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.app.borrow().session.is_failed()
    }

    /// Split the focused pane vertically (side-by-side) or horizontally.
    ///
    /// # Errors
    /// Refuses unsupported axes, unavailable sessions, or more than four panes.
    pub fn split_pane(&self, axis: &str) -> Result<(), JsValue> {
        pane_action(&self.app, PaneAction::Split(axis))
    }

    /// Move focus to the next visible terminal.
    ///
    /// # Errors
    /// Fails if no terminal is ready.
    pub fn focus_next_pane(&self) -> Result<(), JsValue> {
        pane_action(&self.app, PaneAction::Next)
    }

    /// Opt into transparent default cells and borderless pane presentation.
    /// Other clients remain opaque; this changes no terminal state or wire data.
    pub fn set_glass_mode(&self, enabled: bool) {
        let app = self.app.borrow();
        if app.glass_mode.replace(enabled) != enabled {
            app.request_paint();
        }
    }

    /// Focus one canonical id from the visible pane descriptors, without
    /// creating another view or connection.
    ///
    /// # Errors
    /// Refuses missing, hidden, unready, or failed terminals.
    pub fn focus_pane(&self, selector: &str) -> Result<(), JsValue> {
        let id = self
            .app
            .borrow()
            .session
            .pane_rects()
            .into_iter()
            .map(|(id, _)| id)
            .find(|id| id.to_string() == selector)
            .ok_or_else(|| JsValue::from_str("That pane is not visible."))?;
        pane_action(&self.app, PaneAction::Focus(&id))
    }

    /// Send bounded text through the same structured paste path as the
    /// clipboard. This does not append Enter or create a terminal.
    ///
    /// # Errors
    /// Refuses more than 4096 UTF-8 bytes or unavailable terminal input.
    pub fn send_text(&self, text: &str) -> Result<(), JsValue> {
        if text.len() > 4096 {
            return Err(JsValue::from_str(
                "Text exceeds the 4096-byte hosted input limit.",
            ));
        }
        let Some(paste) = crate::input::paste_event(text) else {
            return Ok(());
        };
        if !send_input(&self.app, [InputEvent::Paste(paste)]) {
            return Err(JsValue::from_str("Terminal input is unavailable."));
        }
        Ok(())
    }

    /// Close the focused pane's view; its terminal keeps running.
    ///
    /// # Errors
    /// Refuses the last view or a pending operation.
    pub fn close_view(&self) -> Result<(), JsValue> {
        pane_action(&self.app, PaneAction::CloseView)
    }

    /// Kill the focused terminal, ending its process, while preserving the
    /// other panes.
    ///
    /// # Errors
    /// Refuses the last pane or a pending operation.
    pub fn kill_pane(&self) -> Result<(), JsValue> {
        pane_action(&self.app, PaneAction::Kill)
    }

    /// Send one on-screen key as typed at the terminal: a named key
    /// (`Escape`, `Tab`, `Enter`, `Backspace`, `Delete`, the arrows, `Home`,
    /// `End`, `PageUp`, `PageDown`) or a single character. A held Ctrl latch
    /// chords it and is spent.
    ///
    /// # Errors
    /// Refuses any other key name.
    pub fn send_key(&self, key: &str) -> Result<(), JsValue> {
        let event = crate::input::on_screen_key(key)
            .ok_or_else(|| JsValue::from_str("unsupported on-screen key"))?;
        let events = typed_keys(&self.app, vec![event]);
        send_input(&self.app, events);
        Ok(())
    }

    /// Hold (or release) Ctrl for the next key typed at the terminal, from
    /// the on-screen keyboard or [`Client::send_key`]. The canvas announces
    /// every change as a `phux-modifiers` event.
    pub fn set_ctrl_latch(&self, held: bool) {
        let app = self.app.borrow();
        app.ctrl_latch.set(held);
        app.publish_modifiers();
    }

    /// Close the transport and drop browser handlers and timers.
    pub fn close(&self) {
        self.app.borrow_mut().dispose();
    }

    /// Resize the terminal viewport to `cols`x`rows` cells. The server
    /// resizes the pane (subject to its multi-client size policy) and the
    /// canvas follows the replica's new geometry; a reconnect reattaches at
    /// this size.
    pub fn resize(&self, cols: u16, rows: u16) {
        self.app.borrow().clear_selection();
        let mut app = self.app.borrow_mut();
        if let Some(config) = app.reconnect.get_mut().as_mut() {
            config.cols = cols.max(1);
            config.rows = rows.max(1);
        }
        let frames = app.session.resize_panes(cols, rows);
        if let Err(message) = app.send(frames) {
            app.fail(&message);
        }
        app.request_paint();
    }

    /// Privacy-safe terminal failure reason, if this connection ended.
    #[must_use]
    pub fn failure_reason(&self) -> Option<String> {
        self.app.borrow().failure_reason.borrow().clone()
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let supervised = self.app.borrow().self_owner.borrow().is_some();
        if !supervised {
            self.app.borrow_mut().dispose();
        }
    }
}
