//! Giving the keyboard back to the terminal.
//!
//! The terminal types only while no control has focus (`Desk::key`). Focus
//! that the keyboard or a screen reader put on a control stays there. Focus a
//! pointer press left behind does not, nor does focus Rune hands to a
//! neighbour when the control that had it goes away: the next key would press
//! something nobody chose.

use std::cell::Cell;

use rune::{Cx, Key, KeyEvent, Tx};

use super::{Desk, Modal};
use crate::desk::questions::is_field_key;
use crate::ui::questions::PANE_HOST;

#[derive(Default)]
pub struct Focus {
    /// The control with focus at the last poll.
    held: Option<String>,
    /// Whether a view has found `held` laid out, and has since missed it.
    seen: Cell<bool>,
    gone: Cell<bool>,
    /// The control a closing sheet may hand focus back to.
    kept: Option<String>,
    /// A sheet had the keyboard at the last poll.
    sheet: bool,
    /// The view has run with no sheet open.
    uncovered: Cell<bool>,
    /// Space or Return pressed a control since the last poll.
    pressed: bool,
}

impl Desk {
    /// Put the keyboard on the control `key` at the person's own asking,
    /// which a sheet closing over it (the palette that ran it) does not undo.
    pub(in crate::desk) fn focus_chrome(&mut self, key: String, tx: &mut Tx) {
        tx.focus_when_shown(key.clone(), true);
        self.focus.kept = Some(key);
    }

    /// Hear the keys of whatever has focus before it does, and take none.
    pub(crate) fn heard_key(&mut self, event: &KeyEvent) -> bool {
        let space = matches!(&event.key, Key::Text(text) if text == " ");
        self.focus.pressed |= space || event.key == Key::Enter;
        false
    }

    /// Watch the focused control, so its leaving the window is noticed in
    /// the commit that drops it.
    pub(crate) fn watch_focus(&self, cx: &Cx<Self>) {
        self.focus.uncovered.set(self.modal().is_none());
        let Some(held) = &self.focus.held else { return };
        if cx.layout_with(held, |_| ()).is_some() {
            self.focus.seen.set(true);
        } else if self.focus.seen.get() {
            self.focus.gone.set(true);
        }
    }

    /// Drop focus the terminal should have back. Runs before a frame's keys
    /// are routed. True when the view must run. A phone's focus is its
    /// soft keyboard's, and answers to that.
    pub(crate) fn return_keyboard(&mut self, tx: &mut Tx) -> bool {
        if self.phone.is_some() {
            return false;
        }
        let mut now = tx.focused().map(str::to_owned);
        let modal = self.modal();
        // Closed, but still drawn: focus has yet to leave it.
        if modal.is_none() && self.focus.sheet && !self.focus.uncovered.get() {
            return false;
        }
        // Tab leaves the find field for its buttons, which stay.
        let sheet = modal.is_some_and(|modal| modal != Modal::FindField);
        if modal.is_none() {
            // A press moves focus only by taking its control away: an answer
            // chip that disables itself while it sends, say.
            let moved = self.focus.pressed && self.focus.held.is_some() && self.focus.held != now;
            let left = self.focus.gone.take() || self.focus.sheet || moved;
            // Focus already nowhere may still be on its way to a neighbour;
            // a blur says it is not.
            let dropped = now.is_none() && left;
            if dropped || !self.keeps_focus(now.as_deref(), left, tx.focus_visible()) {
                tx.blur();
                now = None;
            }
            self.focus.kept.clone_from(&now);
        }
        self.focus.sheet = sheet;
        self.focus.pressed = false;
        if self.focus.held == now {
            return false;
        }
        self.focus.held = now;
        self.focus.seen.set(false);
        self.focus.gone.set(false);
        true
    }

    /// Whether focus on `key` stands with no modal open. `left`: what had it
    /// before went away. `shown`: it came by the keyboard or a screen reader.
    fn keeps_focus(&self, key: Option<&str>, left: bool, shown: bool) -> bool {
        let Some(key) = key else { return true };
        if is_field_key(key) {
            return true;
        }
        if key.starts_with(PANE_HOST) {
            // A question that closed under its half-typed answer leaves focus
            // on the pane's host, so the rest of it is not typed as commands.
            let held = self.focus.held.as_deref();
            return held.is_some_and(|held| held == key || is_field_key(held));
        }
        shown && (!left || self.focus.kept.as_deref() == Some(key))
    }
}
