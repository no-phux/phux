//! Window-local prefix interception, before shortcuts and terminal input.

use super::*;
use gpui::prelude::*;
use gpui::{AnyElement, Keystroke, div, px, relative};
use phux_protocol::input::key::{KeyAction, PhysicalKey};

pub(super) struct Leader {
    pending: Option<Pending>,
    down: Option<PhysicalKey>,
    hint_task: Option<Task<()>>,
    show_hint: bool,
    _subscriptions: [Subscription; 2],
}

struct Pending {
    focus: FocusHandle,
    chord: String,
}

impl Leader {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desk>) -> Self {
        let owner = window.window_handle();
        let desk = cx.weak_entity();
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() != owner {
                return;
            }
            let _ = desk.update(cx, |desk, cx| {
                if desk.leader_keystroke(&event.keystroke, window, cx) {
                    cx.stop_propagation();
                }
            });
        });
        let activation = cx.observe_window_activation(window, |desk, _, cx| {
            desk.leader.down = None;
            desk.cancel_leader(cx);
        });
        Self {
            pending: None,
            down: None,
            hint_task: None,
            show_hint: false,
            _subscriptions: [keys, activation],
        }
    }
}

impl Desk {
    fn leader_focus(&self, window: &Window, cx: &gpui::App) -> Option<FocusHandle> {
        if self.text_field_has_keys() || self.modal != Modal::None || self.tab_peek.open {
            return None;
        }
        let focus = window.focused(cx)?;
        let terminal = self.focused_id().and_then(|id| self.panes.get(&id));
        (focus == self.focus
            || terminal.is_some_and(|terminal| *terminal.read(cx).focus_handle() == focus))
        .then_some(focus)
    }

    pub(crate) fn settle_leader(&mut self, window: &Window, cx: &mut Context<Self>) {
        if let Some(pending) = &self.leader.pending
            && (self.prefs.leader_key.as_deref() != Some(pending.chord.as_str())
                || self.leader_focus(window, cx).as_ref() != Some(&pending.focus))
        {
            self.cancel_leader(cx);
        }
    }

    pub(crate) fn cancel_leader(&mut self, cx: &mut Context<Self>) {
        if self.leader.pending.take().is_some() {
            self.leader.hint_task = None;
            self.leader.show_hint = false;
            cx.notify();
        }
    }

    pub(crate) fn release_leader(&mut self, stroke: &Keystroke) {
        if self
            .leader
            .down
            .is_some_and(|key| key == crate::input::key_event(stroke, KeyAction::Release).key)
        {
            self.leader.down = None;
        }
    }

    fn leader_keystroke(
        &mut self,
        stroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.settle_leader(window, cx);
        let Some(chord) = self.prefs.leader_key.as_deref() else {
            return false;
        };
        let Some(focus) = self.leader_focus(window, cx) else {
            return false;
        };
        // The interceptor has no repeat flag. A release, not auto-repeat,
        // must separate two presses of the leader.
        if self
            .leader
            .down
            .is_some_and(|key| key == crate::input::key_event(stroke, KeyAction::Release).key)
        {
            return true;
        }
        let modifiers = stroke.modifiers;
        let is_leader = (modifiers.control || modifiers.alt || modifiers.platform)
            && settings::key_chord(&settings::KeyChord {
                key: stroke.key.clone(),
                cmd: modifiers.platform,
                ctrl: modifiers.control,
                alt: modifiers.alt,
                shift: modifiers.shift,
                held: false,
            }) == chord;
        if is_leader {
            self.leader.down = Some(crate::input::key_event(stroke, KeyAction::Release).key);
            if self.leader.pending.is_some() {
                self.cancel_leader(cx);
                self.send_literal_leader(stroke);
            } else {
                self.leader.pending = Some(Pending {
                    focus,
                    chord: chord.to_owned(),
                });
                self.leader.hint_task = Some(cx.spawn_in(window, async move |this, cx| {
                    smol::Timer::after(Duration::from_millis(400)).await;
                    let _ = this.update_in(cx, |desk, window, cx| {
                        desk.settle_leader(window, cx);
                        if desk.leader.pending.is_some() {
                            desk.leader.show_hint = true;
                            cx.notify();
                        }
                    });
                }));
            }
            return true;
        }
        if self.leader.pending.is_none() {
            return false;
        }
        self.cancel_leader(cx);
        if stroke.key == "escape" && !modifiers.modified() {
            return true;
        }
        // Unbound suffixes retain their normal meaning; only the prefix is
        // swallowed. Command/Ghostty shortcuts continue to dispatch normally.
        if modifiers.control || modifiers.alt || modifiers.platform {
            return false;
        }
        if let Some(id) = settings::leader::command(&stroke.key, modifiers.shift) {
            self.execute(id, window, cx);
            return true;
        }
        false
    }

    fn send_literal_leader(&mut self, stroke: &Keystroke) {
        let Some(terminal) = self.focused_terminal() else {
            return;
        };
        if self.refuse_native_input(&terminal) {
            return;
        }
        let Some(client) = self.client() else { return };
        let sent = client.with_control(|control| {
            if crate::input::native_owned(control, &terminal) {
                return false;
            }
            // Bypass app shortcuts, but use the terminal's own structured
            // key conversion. Complete the release here: the intercepted
            // press was never entered in TerminalInput's pressed-key table.
            [KeyAction::Press, KeyAction::Release]
                .into_iter()
                .all(|action| control.send_key(&terminal, crate::input::key_event(stroke, action)))
        });
        if !sent {
            self.post(
                Kind::Error,
                "Could not send leader key",
                Some("The terminal did not accept the input.".into()),
            );
        }
        client.wake();
    }

    pub(crate) fn leader_hint(&self) -> Option<AnyElement> {
        if !self.leader.show_hint {
            return None;
        }
        let pending = self.leader.pending.as_ref()?;
        let look = self.look();
        Some(
            div()
                .id("desktop-leader-hint")
                .debug_selector(|| "desktop-leader-hint".into())
                .absolute()
                .bottom(px(16.))
                .right(px(16.))
                .w(px(380.))
                .max_w(relative(0.95))
                .max_h(relative(0.9))
                .overflow_y_scroll()
                .p(px(12.))
                .rounded(px(8.))
                .bg(look.card)
                .border_1()
                .border_color(look.divider)
                .text_color(look.ink)
                .text_sm()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(format!(
                    "{} · Leader",
                    settings::display_chord(&pending.chord)
                )))
                .children(settings::leader::HINTS.iter().map(|(keys, title)| {
                    div()
                        .flex()
                        .gap(px(12.))
                        .child(div().w(px(180.)).child(*keys))
                        .child(*title)
                }))
                .child(
                    div()
                        .text_xs()
                        .child("Esc cancels · Double leader sends the key"),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests;
