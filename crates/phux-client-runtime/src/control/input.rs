//! Raw and acknowledged input delivery.

use super::{
    ControlPlane, Event, FocusEvent, FrameKind, InputEvent, InputOperationId, KeyEvent,
    MAX_APPLY_INPUT_COMMAND_BODY, MouseEvent, PasteEvent, PasteTrust, Pending, ResourceId, Status,
    keys,
};
use phux_client_core::input_replay::validate_apply_input_batch;
use phux_protocol::input::key::PhysicalKey;

/// Exact encoded `APPLY_INPUT` command-body overhead of a local
/// [`paste_then_key`] batch beyond its text: the tag, operation id, local
/// terminal id, and event count, then each event's `u32` record length, tag,
/// and fixed body (a paste's trust and data length, a named key's fields).
const APPLY_LINE_OVERHEAD: usize = 54;
/// The same overhead for a local single-[`paste`] batch.
const APPLY_PASTE_OVERHEAD: usize = 34;

/// One paste of `text`, the batch `apply_paste` and `apply_user_paste` send.
fn paste(text: &str, trust: PasteTrust) -> Vec<InputEvent> {
    vec![InputEvent::Paste(PasteEvent {
        trust,
        data: text.as_bytes().to_vec(),
    })]
}

/// A paste of `text`, then `key`: `apply_line` and `apply_tab_completion`.
fn paste_then_key(text: &str, trust: PasteTrust, key: PhysicalKey) -> Vec<InputEvent> {
    let mut events = paste(text, trust);
    events.push(InputEvent::Key(keys::named(key)));
    events
}

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
        let events = paste_then_key(text, PasteTrust::Trusted, PhysicalKey::Enter);
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
        self.begin_acknowledged_input(terminal_id, paste(text, PasteTrust::Untrusted))
    }

    /// Deliver the user's own clipboard paste: the same intent boundary as a
    /// DEC 2004 paste reaching the TUI, or `phux paste`, so it is trusted and
    /// the server brackets it without refusing multi-line text. A paste that
    /// fits one `APPLY_INPUT` goes acknowledged (the returned correlation
    /// resolves [`Event::InputDelivery`]). A larger one, or one for a
    /// satellite pane (acknowledged input is local-only), goes as one atomic
    /// `INPUT_PASTE` event, never split (docs/protocol.md#input), and has no receipt:
    /// `None`. Unresolved acknowledged input for the terminal refuses the
    /// raw event with a receipt rather than let it reach the PTY first.
    pub fn apply_user_paste(&mut self, terminal_id: &ResourceId, text: &str) -> Option<u64> {
        if matches!(terminal_id, ResourceId::Local { .. }) {
            // The journal's batch rule is the gate that refuses, so it alone
            // decides; a paste it would refuse goes raw instead.
            let events = paste(text, PasteTrust::Trusted);
            if validate_apply_input_batch(&events).is_ok() {
                return Some(self.begin_acknowledged_input(terminal_id, events));
            }
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
        let events = paste_then_key(text, PasteTrust::Untrusted, PhysicalKey::Tab);
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
    use phux_protocol::wire::DecodeError;
    use phux_protocol::wire::decode::Decoder;
    use phux_protocol::wire::frame::Command;

    /// A sender's batch builder.
    type Batch = fn(&str) -> Vec<InputEvent>;
    /// A sender under test.
    type Send = fn(&mut ControlPlane, &ResourceId, &str) -> u64;

    /// Each local apply batch, by its overhead, built by the senders' own
    /// builders. Every local terminal id encodes in the same 5 bytes.
    fn apply_batches() -> [(&'static str, usize, Batch); 4] {
        [
            ("apply_line", APPLY_LINE_OVERHEAD, |text| {
                paste_then_key(text, PasteTrust::Trusted, PhysicalKey::Enter)
            }),
            ("apply_tab_completion", APPLY_LINE_OVERHEAD, |text| {
                paste_then_key(text, PasteTrust::Untrusted, PhysicalKey::Tab)
            }),
            ("apply_paste", APPLY_PASTE_OVERHEAD, |text| {
                paste(text, PasteTrust::Untrusted)
            }),
            ("apply_user_paste", APPLY_PASTE_OVERHEAD, |text| {
                paste(text, PasteTrust::Trusted)
            }),
        ]
    }

    fn apply_frame(events: Vec<InputEvent>) -> Vec<u8> {
        crate::control::encode(&FrameKind::Command {
            request_id: u32::MAX,
            command: Command::ApplyInput {
                operation_id: mint_input_operation_id(),
                terminal_id: ResourceId::local(u32::MAX),
                events,
            },
        })
    }

    /// The `COMMAND` field (id 2) the server measures against the cap.
    fn command_body_len(frame: &[u8]) -> usize {
        let mut dec = Decoder::new(&frame[5..]);
        while let Some((id, value)) = dec.read_field().expect("well-formed frame") {
            if id == 2 {
                return value.len();
            }
        }
        panic!("COMMAND without a command field");
    }

    /// Text exactly `MAX - OVERHEAD` long encodes to exactly the cap through
    /// the real encoder and decodes; one byte more is what the server refuses.
    #[test]
    fn apply_overheads_are_exact_encoded_command_overheads() {
        for (name, overhead, batch) in apply_batches() {
            let text = "x".repeat(MAX_APPLY_INPUT_COMMAND_BODY - overhead);
            let at_cap = apply_frame(batch(&text));
            assert_eq!(
                command_body_len(&at_cap),
                MAX_APPLY_INPUT_COMMAND_BODY,
                "{name}"
            );
            assert!(FrameKind::decode(&at_cap).is_ok(), "{name} at the cap");

            let over = apply_frame(batch(&format!("{text}x")));
            assert_eq!(
                FrameKind::decode(&over).unwrap_err(),
                DecodeError::ApplyInputLimitExceeded,
                "{name} one byte over"
            );
        }
    }

    /// The journal's conservative batch rule still bounds the real encoding
    /// now each event carries a `u32` record length: the largest batch it
    /// admits of each fixed-width event, behind a 255-byte satellite host,
    /// encodes within the cap.
    #[test]
    fn the_batch_rule_bounds_the_length_prefixed_encoding() {
        use phux_protocol::input::key::{KeyAction, ModSet};
        use phux_protocol::input::mouse::{MouseAction, MouseButton};
        use phux_protocol::wire::frame::MAX_APPLY_INPUT_EVENTS;

        let mouse = InputEvent::Mouse(MouseEvent {
            action: MouseAction::Press,
            button: MouseButton::Left,
            mods: ModSet::empty(),
            x: 1.0,
            y: 1.0,
        });
        let key = InputEvent::Key(KeyEvent {
            action: KeyAction::Press,
            key: PhysicalKey::A,
            mods: ModSet::empty(),
            consumed_mods: ModSet::empty(),
            composing: false,
            text: Some(String::new()),
            unshifted_codepoint: Some(u32::from('a')),
        });
        let focus = InputEvent::Focus(FocusEvent::Gained);
        let satellite = ResourceId::satellite("h".repeat(255), u32::MAX);
        for widest in [mouse, key, focus] {
            let mut events = vec![widest; MAX_APPLY_INPUT_EVENTS - 1];
            let filler = (0..=MAX_APPLY_INPUT_COMMAND_BODY)
                .rev()
                .find(|len| {
                    let mut batch = events.clone();
                    batch.extend(paste(&"x".repeat(*len), PasteTrust::Trusted));
                    validate_apply_input_batch(&batch).is_ok()
                })
                .expect("an empty paste fits");
            events.extend(paste(&"x".repeat(filler), PasteTrust::Trusted));
            let frame = crate::control::encode(&FrameKind::Command {
                request_id: 1,
                command: Command::ApplyInput {
                    operation_id: mint_input_operation_id(),
                    terminal_id: satellite.clone(),
                    events,
                },
            });
            assert!(command_body_len(&frame) <= MAX_APPLY_INPUT_COMMAND_BODY);
        }
    }

    /// One byte over the exact overhead is refused before anything is queued.
    #[test]
    fn one_byte_over_the_command_cap_is_refused_client_side() {
        let pane = ResourceId::local(7);
        let line = "x".repeat(MAX_APPLY_INPUT_COMMAND_BODY - APPLY_LINE_OVERHEAD + 1);
        let pasted = "x".repeat(MAX_APPLY_INPUT_COMMAND_BODY - APPLY_PASTE_OVERHEAD + 1);
        let sends: [(&str, Send, &str); 3] = [
            ("apply_line", ControlPlane::apply_line, &line),
            (
                "apply_tab_completion",
                ControlPlane::apply_tab_completion,
                &line,
            ),
            ("apply_paste", ControlPlane::apply_paste, &pasted),
        ];
        for (name, send, text) in sends {
            let mut control = ControlPlane::new(ControlOptions::default());
            send(&mut control, &pane, text);
            let message = refusal(&mut control).unwrap_or_else(|| panic!("{name} refused"));
            assert!(
                message.contains("64 KiB command limit"),
                "{name}: {message}"
            );
            assert!(!control.has_outbound(), "{name} queued nothing");
        }
    }

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
