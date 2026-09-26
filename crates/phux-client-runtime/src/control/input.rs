//! Raw and acknowledged input delivery.

use super::{
    APPLY_LINE_OVERHEAD, APPLY_PASTE_OVERHEAD, ControlPlane, DeliveryOutcome, ErrorCode, Event,
    FocusEvent, FrameKind, INPUT_RETRY_HORIZON, InputEvent, InputOperationId, Instant, KeyEvent,
    MAX_APPLY_INPUT_COMMAND_BODY, MouseEvent, PasteEvent, PasteTrust, Pending, ReplayDisposition,
    ReplayReport, ResourceId, ServerFeature, Status, keys,
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
    pub fn send_focus(&mut self, terminal_id: &ResourceId, event: FocusEvent) -> bool {
        if self.delivery_fenced(terminal_id) {
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

    /// Whether raw input for the terminal would pass the server's gate
    /// right now: attached, a published replica, no delivery fence, and
    /// either the attached session's terminal or a confirmed per-terminal
    /// subscription.
    #[must_use]
    pub fn input_ready(&self, terminal_id: &ResourceId) -> bool {
        if self.status != Status::Attached || self.delivery_fenced(terminal_id) {
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

    /// Whether the terminal is fenced behind an acknowledged input whose
    /// delivery is unknown.
    #[must_use]
    pub fn delivery_fenced(&self, terminal_id: &ResourceId) -> bool {
        self.input_replay.delivery_fenced(terminal_id)
    }

    /// Confirm that fresh authoritative presentation was handed to the user.
    /// This is the only evidence that may clear an unknown-delivery fence.
    pub fn acknowledge_projection(&mut self, terminal_id: &ResourceId) {
        self.input_replay.clear_delivery_fence(terminal_id);
        self.delivery_fences.remove(terminal_id);
    }

    /// The token an authoritative projection must carry to safely clear this
    /// terminal's current unknown-delivery fence. Reconnect changes the epoch
    /// even when the underlying ambiguity survives it.
    #[must_use]
    pub fn projection_fence(&self, terminal_id: &ResourceId) -> Option<super::ProjectionFence> {
        if !self.delivery_fenced(terminal_id) {
            return None;
        }
        self.delivery_fences
            .get(terminal_id)
            .map(|delivery_id| super::ProjectionFence {
                connection_epoch: self.connection_epoch,
                delivery_id: *delivery_id,
            })
    }

    /// Atomically validate and clear exactly the fence associated with the
    /// user's presented projection. A stale epoch or delivery is rejected.
    #[must_use]
    pub fn acknowledge_projection_if(
        &mut self,
        terminal_id: &ResourceId,
        expected: super::ProjectionFence,
    ) -> bool {
        if self.projection_fence(terminal_id) != Some(expected) {
            return false;
        }
        self.acknowledge_projection(terminal_id);
        true
    }

    /// When the earliest queued acknowledged input crosses the retry
    /// horizon; `None` while none is queued. The driver sleeps until then
    /// and calls [`Self::expire_inputs`].
    #[must_use]
    pub fn next_input_deadline(&self) -> Option<Instant> {
        self.input_deadlines.values().min().copied()
    }

    /// Earliest retry-horizon deadline across acknowledged input and durable
    /// uploads.
    #[must_use]
    pub fn next_operation_deadline(&self) -> Option<Instant> {
        match (self.next_input_deadline(), self.next_upload_deadline()) {
            (Some(input), Some(upload)) => Some(input.min(upload)),
            (input, upload) => input.or(upload),
        }
    }

    /// Resolve every operation past its retry horizon. Returns whether any
    /// lossless receipt was published.
    pub fn expire_operations(&mut self) -> bool {
        let input = self.expire_inputs();
        let uploads = self.expire_uploads();
        input || uploads
    }

    /// Resolve every acknowledged input past the retry horizon. Returns
    /// whether any resolved.
    pub fn expire_inputs(&mut self) -> bool {
        let before = self.events.len();
        let now_ms = self.now_ms();
        let mut request_id = self.request_seq;
        let (reports, frames) = self.input_replay.next_frames_at(&mut request_id, now_ms);
        self.request_seq = request_id;
        self.publish_replay_reports(reports, None);
        for frame in &frames {
            self.queue_frame(frame);
        }
        self.events.len() != before
    }
    // ----- acknowledged input --------------------------------------------

    pub(crate) fn refuse_acknowledged_input(&mut self, message: &str) -> u64 {
        let delivery_id = self.next_delivery_id();
        self.push_event(Event::InputDelivery {
            delivery_id,
            outcome: DeliveryOutcome::Refused,
            code: Some(ErrorCode::InvalidCommand.as_wire()),
            message: message.to_owned(),
        });
        delivery_id
    }

    pub(super) fn begin_acknowledged_input(
        &mut self,
        terminal_id: &ResourceId,
        events: Vec<InputEvent>,
    ) -> u64 {
        let delivery_id = self.next_delivery_id();
        if matches!(terminal_id, ResourceId::Satellite { .. }) {
            self.push_event(Event::InputDelivery {
                delivery_id,
                outcome: DeliveryOutcome::Refused,
                code: Some(ErrorCode::UnsupportedSatelliteRoute.as_wire()),
                message: "acknowledged input is local-only".to_owned(),
            });
            return delivery_id;
        }
        let operation_id = new_operation_id();
        let key = operation_id_hex(&operation_id);
        let now_ms = self.now_ms();
        self.input_delivery_ids
            .insert(key.clone(), (delivery_id, terminal_id.clone()));
        match self
            .input_replay
            .submit_at(operation_id, terminal_id.clone(), events, now_ms)
        {
            Ok(()) => {
                self.input_deadlines
                    .insert(key, Instant::now() + INPUT_RETRY_HORIZON);
                self.start_input_replay_if_needed(now_ms);
                self.queue_durable_frames();
            }
            Err(report) => self.publish_replay_reports(vec![report], None),
        }
        delivery_id
    }

    pub(super) fn start_input_replay_if_needed(&mut self, now_ms: u64) {
        if !self.handshake_ready || self.input_replay.active() {
            return;
        }
        let Some(server) = &self.server else {
            return;
        };
        let acknowledged = server.has(ServerFeature::AcknowledgedInput);
        let server_id = server.id.clone();
        let reports = self
            .input_replay
            .begin_connection_at(Some(&server_id), acknowledged, now_ms);
        self.publish_replay_reports(reports, None);
    }

    /// Build the next serialized `APPLY_INPUT` attempt per terminal.
    pub(super) fn queue_durable_frames(&mut self) {
        if !self.handshake_ready {
            return;
        }
        let now_ms = self.now_ms();
        let mut request_id = self.request_seq;
        let (reports, frames) = self.input_replay.next_frames_at(&mut request_id, now_ms);
        self.request_seq = request_id;
        self.publish_replay_reports(reports, None);
        for frame in &frames {
            self.queue_frame(frame);
        }
    }

    pub(super) fn publish_replay_reports(&mut self, reports: Vec<ReplayReport>, code: Option<u16>) {
        for report in reports {
            let Some((delivery_id, terminal_id)) =
                self.input_delivery_ids.remove(&report.operation_id)
            else {
                continue;
            };
            self.input_deadlines.remove(&report.operation_id);
            let outcome = match report.disposition {
                ReplayDisposition::Delivered => DeliveryOutcome::Delivered,
                ReplayDisposition::Refused => DeliveryOutcome::Refused,
                ReplayDisposition::Unknown => {
                    // Damage that predates this ambiguity cannot prove what
                    // the server rendered after it.
                    self.damaged.retain(|id| id != &terminal_id);
                    self.record_delivery_fence(&terminal_id, delivery_id);
                    DeliveryOutcome::Unknown
                }
            };
            self.push_event(Event::InputDelivery {
                delivery_id,
                outcome,
                code,
                message: report.message,
            });
        }
    }

    pub(super) fn strand_durable(&mut self, message: &str) {
        let reports = self.input_replay.drain_unresolved(message);
        self.publish_replay_reports(reports, None);
    }

    fn record_delivery_fence(&mut self, terminal_id: &ResourceId, delivery_id: u64) {
        if self.delivery_fenced(terminal_id) {
            self.delivery_fences
                .insert(terminal_id.clone(), delivery_id);
        } else {
            // Retiring a terminal can report an attempted operation Unknown
            // after the journal has already removed its obsolete fence.
            self.delivery_fences.remove(terminal_id);
        }
    }
}

fn new_operation_id() -> InputOperationId {
    loop {
        if let Some(id) = InputOperationId::new(uuid::Uuid::new_v4().into_bytes()) {
            return id;
        }
    }
}

fn operation_id_hex(operation_id: &InputOperationId) -> String {
    use std::fmt::Write as _;
    operation_id
        .as_bytes()
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}
