//! Native focus authorizes live clipboard effects, even before input is ready.

use super::Terminal;
use gpui::{Context, FocusHandle, Subscription, Window};
use phux_protocol::input::focus::FocusEvent;

pub(super) fn subscribe(
    focus: &FocusHandle,
    window: &mut Window,
    cx: &mut Context<Terminal>,
) -> Vec<Subscription> {
    cx.on_release(|terminal, cx| {
        let still_focused = cx.active_window().is_some_and(|active| {
            active
                .update(cx, |_, window, _| terminal.focus.is_focused(window))
                .unwrap_or(false)
        });
        if still_focused
            && terminal
                .reported_focus
                .is_some_and(|(_, _, focused)| focused)
        {
            let _ = terminal
                .client
                .send_focus(&terminal.terminal, FocusEvent::Lost);
        }
    })
    .detach();
    vec![
        cx.on_focus(focus, window, |_, window, cx| {
            // All losing views withdraw first, including another placement
            // of this same terminal whose listener may run after ours.
            cx.defer_in(window, |terminal, window, _| {
                terminal.reported_focus = None;
                terminal.report_focus(window);
            });
        }),
        cx.on_blur(focus, window, |terminal, window, _| {
            terminal.report_focus(window)
        }),
        cx.observe_window_activation(window, |terminal, window, _| terminal.report_focus(window)),
    ]
}

impl Terminal {
    pub(super) fn report_focus(&mut self, window: &Window) {
        let focused = self.focus.is_focused(window) && window.is_window_active();
        let epoch = self.client.connection_epoch();
        // Every render asks, so a settled report must not cost an engine
        // request: only a focused view still waiting for its terminal to
        // take input asks whether it is ready yet.
        let settled = self.reported_focus.is_some_and(|(was_epoch, ready, was)| {
            was_epoch == epoch && was == focused && (ready || !focused)
        });
        if settled {
            return;
        }
        let state = (epoch, self.client.input_ready(&self.terminal), focused);
        if self.reported_focus == Some(state) {
            return;
        }
        // Unfocused views must not withdraw another view's focus. Reassert a
        // focused view after reconnect/bootstrap, which clears runtime intent.
        if focused || self.reported_focus.is_some_and(|(_, _, was)| was) {
            let event = if focused {
                FocusEvent::Gained
            } else {
                FocusEvent::Lost
            };
            let _ = self.client.send_focus(&self.terminal, event);
        }
        self.reported_focus = Some(state);
    }
}
