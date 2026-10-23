//! Window-local, passive tab peeking. Nothing here creates runtime views.

use std::collections::HashMap;

use gpui::{Context, Window};
use phux_gpui::terminal::Preview;

use super::desk::{Desk, Modal};

#[derive(Default)]
pub(crate) struct TabPeek {
    pub open: bool,
    /// Only an ordinary press on the already-selected tab may toggle on click.
    pub click: Option<String>,
    pub scenes: HashMap<String, Preview>,
    pub scroll: gpui::ScrollHandle,
}

impl TabPeek {
    pub fn close(&mut self) {
        self.open = false;
        self.click = None;
        self.scenes.clear();
    }
}

impl Desk {
    pub(crate) fn dismiss_tab_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.tab_peek.open;
        self.tab_peek.close();
        if open {
            self.after_command(window, cx);
        }
    }

    pub(crate) fn toggle_tab_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_peek.open {
            self.tab_peek.close();
        } else if self.active_tab().is_some() {
            self.tab_peek.open = true;
            self.reveal_peek_tab();
        }
        self.after_command(window, cx);
    }

    /// The peek takes keys, not the terminal beneath it. Escape is consumed
    /// before conversation or terminal handling; all other input stays native.
    pub(crate) fn tab_peek_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.tab_peek.open || event.keystroke.modifiers.modified() {
            return false;
        }
        match event.keystroke.key.as_str() {
            "escape" => self.toggle_tab_peek(window, cx),
            "left" | "right" => {
                let step = if event.keystroke.key == "left" { -1 } else { 1 };
                self.workspace_call(|link| link.workspace.step_tab(step));
                self.reveal_peek_tab();
                self.after_command(window, cx);
            }
            "enter" => self.toggle_tab_peek(window, cx),
            _ => return false,
        }
        true
    }

    fn reveal_peek_tab(&self) {
        if let Some(link) = &self.link
            && let Some(index) = link
                .workspace
                .tabs
                .iter()
                .position(|tab| tab.id == link.workspace.active_id)
        {
            self.tab_peek.scroll.scroll_to_item(index);
        }
    }

    pub(crate) fn settle_tab_peek(&mut self) {
        if self.modal != Modal::None || self.active_tab().is_none() {
            self.tab_peek.close();
        }
    }
}
