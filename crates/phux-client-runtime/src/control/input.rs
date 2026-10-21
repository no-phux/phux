//! Raw and acknowledged input delivery.

use super::{
    APPLY_LINE_OVERHEAD, APPLY_PASTE_OVERHEAD, ControlPlane, Event, FocusEvent, FrameKind,
    InputEvent, InputOperationId, KeyEvent, MAX_APPLY_INPUT_COMMAND_BODY, MouseEvent, PasteEvent,
    PasteTrust, Pending, ResourceId, Status, keys,
};

impl ControlPlane {
    /// Send one structured key event on the raw input path.
    pub fn send_key(&mut self, terminal_id: &ResourceId, event: KeyEvent) -> bool {
        if self.delivery_fenced(terminal_id) {
            return false;
        }
        self.queue_frame(&FrameKind::InputKey {
            terminal_id: terminal_id.clone(),
            event,
        });
        true
    }

    /// Type `text` as one key event per scalar on the raw path.
    pub fn send_text(&mut self, terminal_id: &ResourceId, text: &str) -> bool {
        if self.delivery_fenced(terminal_id) {
            return false;
        }
        for event in keys::key_events_for_text(text) {
            self.queue_frame(&FrameKind::InputKey {
                terminal_id: terminal_id.clone(),
                event,
            });
        }
        true
    }

    /// Send a paste as one `INPUT_PASTE` frame; the server brackets it per
    /// the terminal's DEC 2004 state and classifies an untrusted payload.
    pub fn send_paste(
        &mut self,
        terminal_id: &ResourceId,
        data: Vec<u8>,
        trust: PasteTrust,
    ) -> bool {
        if self.delivery_fenced(terminal_id) {
            return false;
        }
        self.queue_frame(&FrameKind::InputPaste {
            terminal_id: terminal_id.clone(),
            event: PasteEvent { trust, data },
        });
        true
    }

    /// Send one mouse event.
    pub fn send_mouse(&mut self, terminal_id: &ResourceId, event: MouseEvent) -> bool {
        if self.delivery_fenced(terminal_id) {
            return false;
        }
        self.queue_frame(&FrameKind::InputMouse {
            terminal_id: terminal_id.clone(),
            event,
        });
        true
    }

    /// Report host focus to the terminal.
    ///
    /// Local clipboard focus intent is recorded even before bootstrap or
    /// behind an input fence. Returns whether a wire focus event was queued;
    /// only input-ready terminals send one.
    pub fn send_focus(&mut self, terminal_id: &ResourceId, event: FocusEvent) -> bool {
        let changed = match event {
            FocusEvent::Gained => self.clipboard_focus.as_ref() != Some(terminal_id),
            FocusEvent::Lost => self.clipboard_focus.as_ref() == Some(terminal_id),
        };
        if changed {
            self.clipboard_focus = match event {
                FocusEvent::Gained => Some(terminal_id.clone()),
                FocusEvent::Lost => None,
            };
            // A request belongs to the focus interval in which it arrived,
            // not whichever terminal happens to be focused at the next drain.
            self.events
                .retain(|event| !matches!(event, Event::ClipboardWrite { .. }));
            self.clipboard_event_bytes = 0;
        }
        if !self.input_ready(terminal_id) {
            return false;
        }
        self.queue_frame(&FrameKind::InputFocus {
            terminal_id: terminal_id.clone(),
            event,
        });
        true
    }

    /// Atomically deliver a composed line and Enter through the
    /// acknowledged path. Returns the correlation [`Event::InputDelivery`]
    /// resolves.
    pub fn apply_line(&mut self, terminal_id: &ResourceId, text: &str) -> u64 {
        if text.len() > MAX_APPLY_INPUT_COMMAND_BODY - APPLY_LINE_OVERHEAD {
            return self.refuse_acknowledged_input("input exceeds the 64 KiB command limit");
        }
        let events = vec![
            InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: text.as_bytes().to_vec(),
            }),
            InputEvent::Key(keys::named(phux_protocol::input::key::PhysicalKey::Enter)),
        ];
        self.begin_acknowledged_input(terminal_id, events)
    }

    /// Answer the ask `question_id` names on `terminal_id`: [`Self::apply_line`]
    /// behind the staleness gate `phux agent answer` applies. The live ask is
    /// the pane's ADR-0035 title sentinel when it carries one, else the id the
    /// event stream last announced (a hook-reported ask sets no title).
    /// Nothing is typed when the pane is not asking, is asking something
    /// else, or the text is unsafe to type as one line; the refusal names
    /// which, as the acknowledged receipt.
    pub fn apply_answer(&mut self, terminal_id: &ResourceId, question_id: &str, text: &str) -> u64 {
        if let Some(reason) = phux_client_core::ask::answer_text_refusal(text) {
            return self.refuse_acknowledged_input(&reason);
        }
        let live = self
            .topology
            .as_ref()
            .and_then(|topology| topology.pane(terminal_id))
            .and_then(|pane| pane.title.as_deref())
            .and_then(phux_client_core::ask::parse_ask_title)
            .map_or_else(
                || self.live_asks.get(terminal_id).cloned(),
                |marker| Some(marker.id),
            );
        match live.as_deref() {
            None => {
                return self.refuse_acknowledged_input("the pane is not asking anything");
            }
            Some("") => {
                return self.refuse_acknowledged_input(
                    "the pane's ask carries no id, so an answer cannot be correlated to it",
                );
            }
            Some(id) if id != question_id => {
                return self.refuse_acknowledged_input(
                    "the pane has moved on to another question; re-read the live ask",
                );
            }
            Some(_) => {}
        }
        self.apply_line(terminal_id, text)
    }

    /// Atomically deliver one untrusted paste through the acknowledged
    /// path, surfacing the server's safety-policy refusal.
    pub fn apply_paste(&mut self, terminal_id: &ResourceId, text: &str) -> u64 {
        if text.len() > MAX_APPLY_INPUT_COMMAND_BODY - APPLY_PASTE_OVERHEAD {
            return self.refuse_acknowledged_input("input exceeds the 64 KiB command limit");
        }
        let events = vec![InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Untrusted,
            data: text.as_bytes().to_vec(),
        })];
        self.begin_acknowledged_input(terminal_id, events)
    }

    /// Deliver the user's own clipboard paste: the same intent boundary as a
    /// DEC 2004 paste reaching the TUI, or `phux paste`, so it is trusted and
    /// the server brackets it without refusing multi-line text. A paste that
    /// fits one `APPLY_INPUT` goes acknowledged (the returned correlation
    /// resolves [`Event::InputDelivery`]). A larger one, or one for a
    /// satellite pane (acknowledged input is local-only), goes as one atomic
    /// `INPUT_PASTE` event, never split (input.md §5.1), and has no receipt:
    /// `None`. Unresolved acknowledged input for the terminal refuses the
    /// raw event with a receipt rather than let it reach the PTY first.
    pub fn apply_user_paste(&mut self, terminal_id: &ResourceId, text: &str) -> Option<u64> {
        let fits = text.len() <= MAX_APPLY_INPUT_COMMAND_BODY - APPLY_PASTE_OVERHEAD;
        if fits && matches!(terminal_id, ResourceId::Local { .. }) {
            let events = vec![InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Trusted,
                data: text.as_bytes().to_vec(),
            })];
            return Some(self.begin_acknowledged_input(terminal_id, events));
        }
        if self.input_replay.must_order_after(terminal_id) {
            return Some(
                self.refuse_acknowledged_input("an earlier paste to this terminal is unresolved"),
            );
        }
        if !self.send_paste(terminal_id, text.as_bytes().to_vec(), PasteTrust::Trusted) {
            return Some(self.refuse_acknowledged_input("delivery to this terminal is fenced"));
        }
        None
    }

    /// Atomically flush a draft and press Tab through the acknowledged
    /// path, so a reconnect can neither reorder nor duplicate them.
    pub fn apply_tab_completion(&mut self, terminal_id: &ResourceId, text: &str) -> u64 {
        if text.len() > MAX_APPLY_INPUT_COMMAND_BODY - APPLY_LINE_OVERHEAD {
            return self.refuse_acknowledged_input("input exceeds the 64 KiB command limit");
        }
        let events = vec![
            InputEvent::Paste(PasteEvent {
                trust: PasteTrust::Untrusted,
                data: text.as_bytes().to_vec(),
            }),
            InputEvent::Key(keys::named(phux_protocol::input::key::PhysicalKey::Tab)),
        ];
        self.begin_acknowledged_input(terminal_id, events)
    }

    /// Whether the terminal's shell is running a command, as this
    /// connection's shell events report it: `Some(true)` after a start
    /// (OSC 133 C), `Some(false)` after a finish (OSC 133 D) or the shell's
    /// exit, `None` before either. A start that predates the connection is
    /// `None`; [`crate::Client::command_running`] then reads the replica.
    #[must_use]
    pub fn command_state(&self, terminal_id: &ResourceId) -> Option<bool> {
        self.command_states.get(terminal_id).copied()
    }

    /// Whether raw input for the terminal would pass the server's gate
    /// right now: attached with no `DETACHED` since, a published replica, no
    /// delivery fence, and either the attached session's terminal or a
    /// confirmed per-terminal subscription.
    #[must_use]
    pub fn input_ready(&self, terminal_id: &ResourceId) -> bool {
        if self.status != Status::Attached || self.attach_ended || self.delivery_fenced(terminal_id)
        {
            return false;
        }
        if !self
            .engine
            .as_ref()
            .is_some_and(|engine| engine.input_ready(terminal_id))
        {
            return false;
        }
        self.attach_terminals.contains(terminal_id)
            || self.own_spawns.contains(terminal_id)
            || (self.terminal_attached.contains(terminal_id)
                && !self.pending.values().any(
                    |pending| matches!(pending, Pending::AttachTerminal(id) if id == terminal_id),
                ))
    }
}

/// Mint a fresh non-zero 128-bit acknowledged-input operation id.
#[must_use]
pub fn mint_input_operation_id() -> InputOperationId {
    loop {
        if let Some(id) = InputOperationId::new(uuid::Uuid::new_v4().into_bytes()) {
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{AgentEvent, ControlOptions, DeliveryOutcome};

    fn refusal(control: &mut ControlPlane) -> Option<String> {
        control
            .take_events()
            .into_iter()
            .find_map(|event| match event {
                Event::InputDelivery {
                    outcome: DeliveryOutcome::Refused,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
    }

    /// The gate `phux agent answer` applies, on the runtime path: no ask,
    /// a different ask, and an unsafe answer each type nothing; the ask the
    /// event stream announced is answerable even with no title sentinel.
    #[test]
    fn an_answer_only_types_into_the_question_it_names() {
        let mut control = ControlPlane::new(ControlOptions::default());
        let pane = ResourceId::local(7);
        control.apply_answer(&pane, "q1", "yes");
        assert!(refusal(&mut control).is_some(), "no live ask must refuse");

        control.fold_agent_event(
            pane.clone(),
            AgentEvent::Asked {
                id: "q1".into(),
                question: "Deploy?".into(),
                suggestions: vec!["yes".into()],
                elapsed_seconds: None,
            },
        );
        control.take_events();
        control.apply_answer(&pane, "q0", "yes");
        assert!(refusal(&mut control).is_some(), "a stale id must refuse");
        control.apply_answer(&pane, "q1", "yes\nno");
        assert!(refusal(&mut control).is_some(), "a line break must refuse");
        control.apply_answer(&pane, "q1", "yes");
        assert!(
            refusal(&mut control).is_none(),
            "the live ask is answerable"
        );
    }
}
