//! Browser-owned transport settlement around the portable replay journal.
use super::*;
use phux_client_core::input_replay::{InputReplayJournal, InputSubmission, ReplayDisposition, ReplayReport, UserPasteDelivery, operation_id_hex, user_paste_delivery};
use phux_client_core::session::ReplicaKey;
use phux_protocol::ids::InputOperationId;

#[derive(Default)]
pub(crate) struct InputDelivery {
    pub(super) journal: InputReplayJournal,
    request_id: u32,
    delivery_id: u64,
    correlations: HashMap<String, u64>,
    reports: Vec<(u64, ReplayReport)>,
    coalesced: [u64; 3],
    fences: HashMap<ResourceId, Option<(ReplicaKey, u64)>>,
}

impl InputDelivery {
    fn retain_report(&mut self, delivery_id: u64, report: ReplayReport) {
        if self.reports.len() < 64 {
            self.reports.push((delivery_id, report));
        } else {
            let index = match report.disposition {
                ReplayDisposition::Delivered => 0,
                ReplayDisposition::Refused => 1,
                ReplayDisposition::Unknown => 2,
            };
            self.coalesced[index] = self.coalesced[index].saturating_add(1);
        }
    }

    fn drain_reports(&mut self) -> Vec<(u64, ReplayReport)> {
        let mut reports = std::mem::take(&mut self.reports);
        for (index, disposition) in [ReplayDisposition::Delivered, ReplayDisposition::Refused, ReplayDisposition::Unknown].into_iter().enumerate() {
            let count = std::mem::take(&mut self.coalesced[index]);
            if count > 0 {
                reports.push((0, ReplayReport {
                    disposition, operation_id: "multiple".to_owned(),
                    message: format!("{count} additional input outcomes were coalesced"),
                }));
            }
        }
        reports
    }
    pub(crate) fn expire_disconnected(&mut self) -> Vec<(u64, ReplayReport)> {
        if self.journal.active() { return Vec::new(); }
        let (reports, _) = self.journal.next_frames_at(&mut self.request_id, browser_monotonic_ms());
        reports.into_iter().map(|report| {
            let id = self.correlations.remove(&report.operation_id).unwrap_or(0);
            (id, report)
        }).collect()
    }
    pub(crate) fn stop_reconnecting(&mut self) -> Vec<(u64, ReplayReport)> {
        self.journal.drain_unresolved("reconnect stopped: incompatible server protocol")
            .into_iter().map(|report| {
                let id = self.correlations.remove(&report.operation_id).unwrap_or(0);
                (id, report)
            }).collect()
    }
}

impl Connection {
    pub(crate) fn input_delivery_state(&self) -> Rc<std::cell::RefCell<InputDelivery>> {
        Rc::clone(&self.input_delivery)
    }
    pub(crate) fn stop_input_delivery(&self, reconnecting: bool) {
        if reconnecting {
            self.input_delivery.borrow_mut().journal.connection_lost();
        } else {
            let reports = self.input_delivery.borrow_mut().journal.drain_unresolved("the browser connection was closed");
            self.record_input_reports(reports);
        }
    }

    pub(crate) fn refuse_browser_input(&self, message: &str) -> InputSubmission {
        let mut state = self.input_delivery.borrow_mut();
        state.delivery_id = state.delivery_id.wrapping_add(1).max(1);
        let delivery_id = state.delivery_id;
        state.retain_report(delivery_id, ReplayReport {
            disposition: ReplayDisposition::Refused,
            operation_id: "not-sent".to_owned(),
            message: message.to_owned(),
        });
        InputSubmission::Refused { delivery_id }
    }
    pub(crate) fn unknown_raw_input(&self, message: &str) {
        let mut state = self.input_delivery.borrow_mut();
        state.delivery_id = state.delivery_id.wrapping_add(1).max(1);
        let delivery_id = state.delivery_id;
        state.retain_report(delivery_id, ReplayReport {
            disposition: ReplayDisposition::Unknown,
            operation_id: "unreceipted".to_owned(),
            message: message.to_owned(),
        });
    }

    pub(crate) fn adopt_input_delivery(&mut self, state: Rc<std::cell::RefCell<InputDelivery>>) {
        self.input_delivery = state;
        self.begin_input_connection();
    }

    pub(super) fn begin_input_connection(&mut self) {
        let reports = self.input_delivery.borrow_mut().journal.begin_connection_at(
            self.input_server_id.as_deref(), self.acknowledged_input, browser_monotonic_ms());
        self.record_input_reports(reports);
    }

    fn record_input_reports(&self, reports: Vec<ReplayReport>) {
        let mut state = self.input_delivery.borrow_mut();
        for report in reports {
            let id = state.correlations.remove(&report.operation_id).unwrap_or(0);
            state.retain_report(id, report);
        }
        for terminal in &self.terminal_order {
            if state.journal.delivery_fenced(terminal) && !state.fences.contains_key(terminal) {
                let identity = self.kernel.as_ref().and_then(|kernel| kernel.published(terminal))
                    .map(|replica| (replica.key().clone(), replica.last_seq()));
                state.fences.insert(terminal.clone(), identity);
            }
        }
    }

    /// Drain settled delivery outcomes with their original local submission
    /// correlation. Local queue acceptance is never a delivered outcome.
    pub fn take_input_reports(&self) -> Vec<(u64, ReplayReport)> {
        let reports = self.input_delivery.borrow_mut().journal.take_reports();
        self.record_input_reports(reports);
        self.input_delivery.borrow_mut().drain_reports()
    }

    /// Captured immediately before painting, checked again after the canvas draw.
    pub(crate) fn input_presentation(&self, terminal: &ResourceId) -> Option<(ReplicaKey, u64)> {
        if self.failed { return None; }
        if !self.input_delivery.borrow().journal.delivery_fenced(terminal) { return None; }
        let replica = self.kernel.as_ref()?.published(terminal)?;
        Some((replica.key().clone(), replica.last_seq()))
    }

    pub(crate) fn present_input(&self, terminal: &ResourceId, identity: &(ReplicaKey, u64)) {
        if self.input_presentation(terminal).as_ref() != Some(identity)
            || self.pane_terminal(terminal).is_none_or(Terminal::viewport_scrolled) { return; }
        let mut state = self.input_delivery.borrow_mut();
        let fresh = state.fences.get(terminal).is_some_and(|baseline| {
            baseline.as_ref().is_none_or(|old| old.0 != identity.0 || identity.1 > old.1)
        });
        if fresh {
            state.journal.clear_delivery_fence(terminal);
            state.fences.remove(terminal);
        }
    }

    pub(crate) fn prepare_user_paste(&self, text: &str) -> Result<PasteEvent, InputSubmission> {
        let Some(terminal) = self.first_published_terminal() else { return Err(InputSubmission::NotQueued); };
        if text.is_empty() || self.failed
            || !self.kernel.as_ref().is_some_and(|kernel| matches!(kernel.input_eligibility(&terminal), InputEligibility::Eligible { .. })) {
            return Err(InputSubmission::NotQueued);
        }
        let policy = {
            let state = self.input_delivery.borrow();
            user_paste_delivery(text,
                state.journal.active() && matches!(terminal, ResourceId::Local { .. }),
                state.journal.must_order_after(&terminal), state.journal.delivery_fenced(&terminal))
        };
        if let UserPasteDelivery::Refused(message) = policy {
            return Err(self.refuse_browser_input(message));
        }
        Ok(PasteEvent { trust: PasteTrust::Trusted, data: text.as_bytes().to_vec() })
    }
    pub(crate) fn submit_browser_input(&mut self, event: InputEvent) -> (InputSubmission, Option<Vec<u8>>) {
        let Some(terminal) = self.first_published_terminal() else { return (InputSubmission::NotQueued, None); };
        if self.failed || !self.kernel.as_ref().is_some_and(|kernel| matches!(kernel.input_eligibility(&terminal), InputEligibility::Eligible { .. })) {
            return (InputSubmission::NotQueued, None);
        }
        let (active, ordered, fenced) = {
            let state = self.input_delivery.borrow();
            (state.journal.active(), state.journal.must_order_after(&terminal), state.journal.delivery_fenced(&terminal))
        };
        let policy = match &event {
            InputEvent::Paste(paste) => match std::str::from_utf8(&paste.data) {
                Ok(text) => user_paste_delivery(text, active && matches!(terminal, ResourceId::Local { .. }), ordered, fenced),
                Err(_) => UserPasteDelivery::Refused("browser paste is not UTF-8"),
            },
            _ if ordered || fenced => UserPasteDelivery::Acknowledged,
            _ => UserPasteDelivery::Raw,
        };
        if policy == UserPasteDelivery::Raw {
            return match self.input_frame(event) {
                Some(frame) => (InputSubmission::QueuedRaw, Some(frame)),
                None => (InputSubmission::NotQueued, None),
            };
        }
        let refusal = match policy {
            UserPasteDelivery::Refused(message) => Some(message),
            _ if !active => Some("acknowledged input is unavailable"),
            _ => None,
        };
        if let Some(message) = refusal {
            return (self.refuse_browser_input(message), None);
        }
        let Some(operation) = mint_operation() else {
            return (self.refuse_browser_input("secure operation identity is unavailable"), None);
        };
        let operation_id = operation_id_hex(&operation);
        let mut state = self.input_delivery.borrow_mut();
        state.delivery_id = state.delivery_id.wrapping_add(1).max(1);
        let delivery_id = state.delivery_id;
        state.correlations.insert(operation_id, delivery_id);
        let submitted = state.journal.submit_at(operation, terminal, vec![event], browser_monotonic_ms());
        drop(state);
        // submit_at defers its refusal report; the visible drain preserves its correlation.
        if submitted.is_err() { (InputSubmission::Refused { delivery_id }, None) }
        else { (InputSubmission::AwaitingReceipt { delivery_id }, None) }
    }

    pub(crate) fn input_attempts(&self) -> Vec<FrameKind> {
        self.input_attempts_at(browser_monotonic_ms())
    }

    pub(crate) fn input_attempts_at(&self, now_ms: u64) -> Vec<FrameKind> {
        if self.failed || !self.attach_ready { return Vec::new(); }
        let mut state = self.input_delivery.borrow_mut();
        let InputDelivery { journal, request_id, .. } = &mut *state;
        // A separate high request-id range cannot alias browser pane/path requests.
        if *request_id < 0x8000_0000 { *request_id = 0x8000_0000; }
        let (reports, frames) = journal.next_frames_at(request_id, now_ms);
        drop(state);
        self.record_input_reports(reports);
        frames
    }

    pub(crate) fn rollback_input_unsent(&self, frames: &[FrameKind]) {
        self.input_delivery.borrow_mut().journal.rollback_unsent(frames);
    }

    pub(super) fn resolve_input_frame(&self, frame: &FrameKind) -> bool {
        let report = match frame {
            FrameKind::CommandResult { request_id, result } => self.input_delivery.borrow_mut().journal.resolve(*request_id, result),
            FrameKind::Error { request_id: Some(request_id), code, message } => self.input_delivery.borrow_mut().journal.resolve(*request_id, &CommandResult::Error { code: *code, message: message.clone() }),
            _ => None,
        };
        if let Some(report) = report { self.record_input_reports(vec![report]); true } else { false }
    }

    pub(super) fn retire_input(&self, terminal: &ResourceId, why: &str) {
        let reports = self.input_delivery.borrow_mut().journal.retire_terminal(terminal, why);
        self.record_input_reports(reports);
        self.input_delivery.borrow_mut().fences.remove(terminal);
    }
}

fn mint_operation() -> Option<InputOperationId> {
    use wasm_bindgen::{JsCast, JsValue};
    let global = js_sys::global();
    let crypto = js_sys::Reflect::get(&global, &JsValue::from_str("crypto")).ok()?;
    let random = js_sys::Reflect::get(&crypto, &JsValue::from_str("getRandomValues")).ok()?.dyn_into::<js_sys::Function>().ok()?;
    let bytes = js_sys::Uint8Array::new_with_length(16);
    random.call1(&crypto, &bytes).ok()?;
    let mut id = [0; 16];
    bytes.copy_to(&mut id);
    InputOperationId::new(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn bounded_browser_reports_do_not_silently_drop_unknown_delivery() {
        let mut state = InputDelivery::default();
        for delivery_id in 1..=100 {
            state.retain_report(delivery_id, ReplayReport {
                disposition: ReplayDisposition::Refused,
                operation_id: delivery_id.to_string(),
                message: "not handed off".to_owned(),
            });
        }
        state.retain_report(101, ReplayReport {
            disposition: ReplayDisposition::Unknown,
            operation_id: "uncertain".to_owned(),
            message: "receipt lost".to_owned(),
        });
        let reports = state.drain_reports();
        assert_eq!(reports.len(), 66);
        assert_eq!(reports[64].1.disposition, ReplayDisposition::Refused);
        assert!(reports[64].1.message.starts_with("36 "));
        assert_eq!(reports[65].1.disposition, ReplayDisposition::Unknown);
        assert!(reports[65].1.notice_line().contains("read the pane before retyping"));
        assert!(state.drain_reports().is_empty());
    }
}
