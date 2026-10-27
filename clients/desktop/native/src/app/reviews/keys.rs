//! The surface's own keys, while none of its fields has the keyboard, and
//! where the keyboard goes when it closes.

use gpui::{Context, Focusable, Window};

use super::super::desk::Desk;
use super::Tab;

impl Desk {
    /// Keys the surface owns while no field of it has the keyboard.
    pub(crate) fn reviews_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.reviews.open || !matches!(self.modal, super::super::desk::Modal::None) {
            return false;
        }
        let modifiers = &event.keystroke.modifiers;
        if modifiers.modified() {
            return false;
        }
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            return self.reviews_escape(window, cx);
        }
        if self.reviews.has_keys(window, cx) {
            return false;
        }
        let diff = self.reviews.diff_has_keys(window, cx);
        match key {
            "j" => self.step_review(1, cx),
            "k" => self.step_review(-1, cx),
            "down" | "up" if !diff => self.step_review(if key == "down" { 1 } else { -1 }, cx),
            "n" => self.step_change(true, cx),
            "p" => self.step_change(false, cx),
            "r" => {
                self.refresh_inbox(cx);
                if let Some(key) = self.reviews.opened_key().cloned() {
                    self.read_pull(&key, cx);
                }
            }
            "o" => self.open_pull_in_browser(cx),
            "1" => self.set_review_tab(Tab::Files, cx),
            "2" => self.set_review_tab(Tab::Conversation, cx),
            "3" => self.set_review_tab(Tab::Checks, cx),
            "c" => self.focus_review_summary(window, cx),
            _ => return false,
        }
        true
    }

    /// Escape leaves the summary, then drops the comment being written, then
    /// disarms Merge, then leaves the surface.
    fn reviews_escape(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let surface = self
            .reviews
            .focus
            .clone()
            .unwrap_or_else(|| self.focus.clone());
        let summary_focused = self
            .reviews
            .opened
            .as_ref()
            .and_then(|o| o.summary.as_ref())
            .is_some_and(|(input, _)| input.read(cx).focus_handle(cx).is_focused(window));
        if summary_focused {
            window.focus(&surface, cx);
            cx.notify();
            return true;
        }
        if let Some(opened) = self.reviews.opened.as_mut() {
            if opened.draft.take().is_some() {
                window.focus(&surface, cx);
                cx.notify();
                return true;
            }
            if opened.confirm_merge {
                opened.confirm_merge = false;
                cx.notify();
                return true;
            }
        }
        self.close_reviews(window, cx);
        true
    }

    /// A closed surface must not keep the keyboard: give it back to the panes.
    pub(crate) fn settle_reviews_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reviews.open {
            return;
        }
        if self
            .reviews
            .focus
            .as_ref()
            .is_some_and(|focus| focus.contains_focused(window, cx))
        {
            self.focus_terminal(window, cx);
        }
    }

    fn step_change(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some((state, _)) = self.reviews.opened.as_ref().and_then(|o| o.diff.as_ref()) else {
            return;
        };
        state.update(cx, |state, cx| {
            if forward {
                state.next_change(cx);
            } else {
                state.previous_change(cx);
            }
        });
    }
}
