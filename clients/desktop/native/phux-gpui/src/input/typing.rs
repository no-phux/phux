//! Keys and committed text: what reaches the PTY, its local echo guess, and
//! the selection typing clears.

use super::*;

impl TerminalInput {
    /// Register on the focused terminal's bubbling key-down path. Stop GPUI
    /// propagation only for Consumed. Printable keys reach the OS text handler.
    pub fn key_down(
        &mut self,
        down: &KeyDownEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Result<KeyDisposition, InputError> {
        self.pending_key = None;
        if down.keystroke.modifiers.platform {
            return Ok(KeyDisposition::Platform);
        }
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        if !self.composition.text.is_empty() {
            return Ok(KeyDisposition::Platform);
        }
        let action = if down.is_held {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        };
        let mut event = keys::event(&down.keystroke, action);
        event.mods.set(ModSet::CAPS_LOCK, window.capslock().on);
        if keys::uses_text(down, self.option_as_alt) {
            self.with_current(true, |_| Ok(()))?;
            self.pending_key = Some(event);
            self.key_probe.arm(self.painted_seq);
            return Ok(KeyDisposition::Platform);
        }
        let echoed = self.press(event)?;
        self.key_probe.arm(self.painted_seq);
        if echoed {
            cx.notify();
        }
        Ok(KeyDisposition::Consumed)
    }

    pub fn key_up(
        &mut self,
        up: &KeyUpEvent,
        window: &Window,
        cx: &App,
    ) -> Result<KeyDisposition, InputError> {
        self.pending_key = None;
        let released = keys::event(&up.keystroke, KeyAction::Release).key;
        let Some(mut event) = self.pressed.remove(&released) else {
            return Ok(KeyDisposition::Platform);
        };
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        event.action = KeyAction::Release;
        event.mods = keys::modifiers(up.keystroke.modifiers);
        event.mods.set(ModSet::CAPS_LOCK, window.capslock().on);
        event.consumed_mods = ModSet::empty();
        event.text = None;
        self.send_key(&event)?;
        Ok(KeyDisposition::Consumed)
    }

    fn send_key(&self, event: &KeyEvent) -> Result<(), InputError> {
        self.with_current(true, |control| {
            queued(control.send_key(&self.terminal, event.clone()))
        })
    }

    /// Send a press or repeat, draw its local echo guess, and hold it for the
    /// release. Returns whether the view changed locally (a guess drawn or a
    /// selection cleared) and needs a repaint.
    pub(super) fn press(&mut self, event: KeyEvent) -> Result<bool, InputError> {
        self.send_key(&event)?;
        let cleared = self.clear_typed_over_selection();
        let echoed = self.predict_sent(&event);
        self.pressed.insert(event.key, event);
        Ok(echoed || cleared)
    }

    pub(super) fn commit(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &Window,
        cx: &App,
    ) -> Result<(), InputError> {
        if !self.focused(window, cx) {
            return Err(InputError::WrongFocus);
        }
        self.composition
            .replace(range, text)
            .ok_or(InputError::InvalidRange)?;
        let committed = std::mem::take(&mut self.composition.text);
        self.composition.clear();
        if let Some(mut event) = self.pending_key.take() {
            event.consumed_mods = event.mods & (ModSet::SHIFT | ModSet::ALT);
            event.text = Some(committed);
            return self.press(event).map(drop);
        }
        self.with_current(true, |control| {
            queued(control.send_text(&self.terminal, &committed))
        })?;
        self.clear_typed_over_selection();
        self.publish_echo(&committed);
        Ok(())
    }

    /// Typing clears a selection this view shows, as Ghostty's
    /// `selection-clear-on-typing`, so a stale highlight never lingers over
    /// the new input. Copy and other Command chords never get here.
    fn clear_typed_over_selection(&self) -> bool {
        let Some(client) = self.client.upgrade() else {
            return false;
        };
        let shown = client.acquire_view(self.view).is_some_and(|frame| {
            frame
                .buffer
                .cells
                .iter()
                .any(|cell| cell.flags & phux_client_core::grid::CELL_SELECTED != 0)
        });
        shown
            && client
                .engine()
                .is_some_and(|engine| engine.clear_view_selection(self.view).is_ok())
    }

    /// Paint a local guess for this key before the server can echo it.
    ///
    /// The runtime predictor decides what is safe to draw, including hiding
    /// alternate-screen guesses until the application proves it echoes.
    /// A local socket still predicts: it may be the near end of a tunnel.
    fn predict_sent(&self, event: &KeyEvent) -> bool {
        echo_guess(event).is_some_and(|text| self.publish_echo(text))
    }

    fn publish_echo(&self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let Some(client) = self.client.upgrade() else {
            return false;
        };
        let Some(engine) = client.engine() else {
            return false;
        };
        engine
            .predict_view_text(self.view, text.to_owned())
            .unwrap_or(false)
    }
}

/// The text a key should draw locally, if it is a guess the predictor can own.
/// Control and super chords are commands. Alt stays: Option types a character.
pub(super) fn echo_guess(event: &KeyEvent) -> Option<&str> {
    let typed = matches!(event.action, KeyAction::Press | KeyAction::Repeat);
    if !typed || event.mods.intersects(ModSet::CTRL | ModSet::SUPER) {
        return None;
    }
    match event.key {
        PhysicalKey::Enter => Some("\r"),
        PhysicalKey::Backspace => Some("\u{7f}"),
        _ => event.text.as_deref().filter(|text| !text.is_empty()),
    }
}
