//! Live program clipboard writes belong to the active terminal's focus interval.

use super::*;

impl Desk {
    pub(super) fn deliver_clipboard_writes(&self, events: &[Event], cx: &mut Context<Self>) {
        if !events
            .iter()
            .any(|event| matches!(event, Event::ClipboardWrite { .. }))
            || !self.session_is_attached()
        {
            return;
        }
        let Some(terminal) = self.focused_terminal() else {
            return;
        };
        let Some(entity) = self.focused_id().and_then(|id| self.panes.get(&id)) else {
            return;
        };
        let focus = entity.read(cx).focus_handle().clone();
        let focused = cx.active_window().is_some_and(|active| {
            active
                .update(cx, |_, window, _| {
                    window.is_window_active() && focus.is_focused(window)
                })
                .unwrap_or(false)
        });
        if !focused {
            return;
        }
        for event in events {
            if let Event::ClipboardWrite { terminal_id, text } = event
                && *terminal_id == terminal
            {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.0.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests;
