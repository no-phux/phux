//! The live projection of server-held approvals (ADR-0128) on the control
//! connection: list and read the `phux.approval/v1/<id>` records, follow the
//! `approval_requested` / `approval_decided` journal events, and decide one.
//!
//! The projection is opt-in ([`ControlPlane::watch_approvals`]) and gated on
//! `ServerFeature::APPROVALS`: an older server stores a decide key as an
//! ordinary value, so nothing is sent to one. Every connection re-lists, and
//! so does a journal or source gap. A decision is never retried; its outcome
//! is one [`Event::ApprovalDecision`].

use std::collections::HashMap;

use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::ApprovalId;
use phux_protocol::wire::frame::{APPROVAL_KEY_PREFIX, AgentEvent, ErrorCode, FrameKind, Scope};

use super::{ControlPlane, Event};
use crate::approvals::{Approval, Decision, DecisionOutcome};

/// The approval lane's state. Correlations belong to one connection.
#[derive(Debug, Default)]
pub(super) struct ApprovalWatch {
    /// The consumer asked for the projection; kept across reconnects.
    watching: bool,
    /// In-flight `LIST_METADATA { Global }`.
    listing: Option<u32>,
    /// In-flight record reads, by request id.
    reads: HashMap<u32, ApprovalId>,
    /// In-flight decisions, by the request id of their confirming read.
    decisions: HashMap<u32, InFlight>,
    /// The decision `SET_METADATA` request ids, to their confirming read.
    decide_ids: HashMap<u32, u32>,
    /// The pending set, by id.
    pending: HashMap<ApprovalId, Approval>,
}

#[derive(Debug)]
struct InFlight {
    id: ApprovalId,
    decision: Decision,
    decide_id: u32,
    refusal: Option<(ErrorCode, String)>,
}

impl ControlPlane {
    /// Project the server's held approvals (ADR-0128) on this connection and
    /// every later one: [`Event::ApprovalsChanged`] whenever
    /// [`Self::approvals`] changes. Nothing is sent to a server without
    /// `APPROVALS`.
    pub fn watch_approvals(&mut self) {
        if self.approvals.watching {
            return;
        }
        self.approvals.watching = true;
        if self.handshake_ready {
            self.list_approvals();
        }
    }

    /// Whether the negotiated server holds actions for approval.
    #[must_use]
    pub fn approvals_supported(&self) -> bool {
        self.handshake_ready && self.server_has(ServerFeature::Approvals)
    }

    /// The pending approvals, oldest first. Empty unless watching.
    #[must_use]
    pub fn approvals(&self) -> Vec<Approval> {
        let mut approvals: Vec<Approval> = self.approvals.pending.values().cloned().collect();
        approvals.sort_by(|a, b| (a.requested_at_ms, &a.id).cmp(&(b.requested_at_ms, &b.id)));
        approvals
    }

    /// Decide the held action `id`. The outcome is one
    /// [`Event::ApprovalDecision`]; it is never retried. `false` when nothing
    /// was sent: no negotiated server with `APPROVALS`, or a decision on `id`
    /// is already in flight.
    pub fn decide_approval(&mut self, id: ApprovalId, decision: Decision) -> bool {
        if !self.approvals_supported()
            || self
                .approvals
                .decisions
                .values()
                .any(|in_flight| in_flight.id == id)
        {
            return false;
        }
        let decide_id = self.next_request_id();
        self.queue_frame(&FrameKind::SetMetadata {
            request_id: decide_id,
            scope: Scope::Global,
            key: id.decide_key(),
            value: decision.value().to_vec(),
        });
        let confirm_id = self.next_request_id();
        self.queue_frame(&FrameKind::GetMetadata {
            request_id: confirm_id,
            scope: Scope::Global,
            key: id.record_key(),
        });
        self.approvals.decide_ids.insert(decide_id, confirm_id);
        self.approvals.decisions.insert(
            confirm_id,
            InFlight {
                id,
                decision,
                decide_id,
                refusal: None,
            },
        );
        true
    }

    /// `HELLO_OK`: re-list on the new connection.
    pub(super) fn approvals_negotiated(&mut self) {
        if self.approvals.watching {
            self.list_approvals();
        }
    }

    /// The connection ended: its correlations are void, and a decision still
    /// in flight is unconfirmed. The pending set stands until a re-list.
    pub(super) fn approvals_connection_ended(&mut self) {
        let watch = &mut self.approvals;
        watch.listing = None;
        watch.reads.clear();
        watch.decide_ids.clear();
        let unconfirmed: Vec<InFlight> =
            watch.decisions.drain().map(|(_, flight)| flight).collect();
        for flight in unconfirmed {
            self.push_event(Event::ApprovalDecision {
                id: flight.id,
                decision: flight.decision,
                outcome: DecisionOutcome::Unconfirmed,
            });
        }
    }

    fn list_approvals(&mut self) {
        if !self.approvals_supported() {
            if !self.approvals.pending.is_empty() {
                self.approvals.pending.clear();
                self.push_event(Event::ApprovalsChanged);
            }
            return;
        }
        if self.approvals.listing.is_some() {
            return;
        }
        let request_id = self.next_request_id();
        self.approvals.listing = Some(request_id);
        self.queue_frame(&FrameKind::ListMetadata {
            request_id,
            scope: Scope::Global,
        });
    }

    fn read_approval(&mut self, id: ApprovalId) {
        if self.approvals.reads.values().any(|reading| *reading == id) {
            return;
        }
        let request_id = self.next_request_id();
        self.approvals.reads.insert(request_id, id);
        self.queue_frame(&FrameKind::GetMetadata {
            request_id,
            scope: Scope::Global,
            key: id.record_key(),
        });
    }

    /// Follow the journal: a request is read, an ending drops the record, and
    /// a gap re-lists.
    pub(super) fn observe_approval_event(&mut self, event: &AgentEvent) {
        if !self.approvals.watching || !self.approvals_supported() {
            return;
        }
        match event {
            AgentEvent::ApprovalRequested { id } if !self.approvals.pending.contains_key(id) => {
                self.read_approval(*id);
            }
            AgentEvent::ApprovalDecided { id, .. } => self.forget_approval(*id),
            AgentEvent::JournalGap { .. } | AgentEvent::SourceGap { .. } => {
                self.list_approvals();
            }
            _ => {}
        }
    }

    fn forget_approval(&mut self, id: ApprovalId) {
        if self.approvals.pending.remove(&id).is_some() {
            self.push_event(Event::ApprovalsChanged);
        }
    }

    /// Consume this lane's replies; anything else passes through.
    pub(super) fn take_approval_frame(&mut self, frame: FrameKind) -> Option<FrameKind> {
        match frame {
            FrameKind::MetadataKeys { request_id, keys }
                if self.approvals.listing == Some(request_id) =>
            {
                self.approvals.listing = None;
                self.listed_approvals(&keys);
                None
            }
            FrameKind::MetadataValue { request_id, value }
                if self.approvals.reads.contains_key(&request_id) =>
            {
                if let Some(id) = self.approvals.reads.remove(&request_id) {
                    self.read_approval_record(id, value.as_deref());
                }
                None
            }
            FrameKind::MetadataValue { request_id, value }
                if self.approvals.decisions.contains_key(&request_id) =>
            {
                self.finish_decision(request_id, Some(value.is_some()));
                None
            }
            other => Some(other),
        }
    }

    /// Consume a correlated `ERROR` this lane owns. A refused listing or
    /// record read leaves the set as it was; a refused decision is recorded
    /// for its confirming read to classify.
    pub(super) fn approval_error(
        &mut self,
        request_id: u32,
        code: ErrorCode,
        message: &str,
    ) -> bool {
        if self.approvals.listing == Some(request_id) {
            self.approvals.listing = None;
            tracing::debug!(?code, message, "approval listing refused");
            return true;
        }
        if self.approvals.reads.remove(&request_id).is_some() {
            tracing::debug!(?code, message, "approval record read refused");
            return true;
        }
        if let Some(confirm_id) = self.approvals.decide_ids.remove(&request_id) {
            if let Some(flight) = self.approvals.decisions.get_mut(&confirm_id) {
                flight.refusal = Some((code, message.to_owned()));
            }
            return true;
        }
        if self.approvals.decisions.contains_key(&request_id) {
            self.finish_decision(request_id, None);
            return true;
        }
        false
    }

    fn listed_approvals(&mut self, keys: &[String]) {
        let listed: Vec<ApprovalId> = keys
            .iter()
            .filter_map(|key| key.strip_prefix(APPROVAL_KEY_PREFIX))
            .filter_map(ApprovalId::parse)
            .collect();
        let before = self.approvals.pending.len();
        self.approvals.pending.retain(|id, _| listed.contains(id));
        if self.approvals.pending.len() != before {
            self.push_event(Event::ApprovalsChanged);
        }
        for id in listed {
            if !self.approvals.pending.contains_key(&id) {
                self.read_approval(id);
            }
        }
    }

    fn read_approval_record(&mut self, id: ApprovalId, value: Option<&[u8]>) {
        // Absent: decided between the listing (or event) and the read.
        let Some(bytes) = value else {
            return;
        };
        match Approval::parse(bytes) {
            Ok(approval) => {
                self.approvals.pending.insert(id, approval);
                self.push_event(Event::ApprovalsChanged);
            }
            Err(error) => tracing::warn!(%id, %error, "unreadable approval record"),
        }
    }

    fn finish_decision(&mut self, confirm_id: u32, still_pending: Option<bool>) {
        let Some(flight) = self.approvals.decisions.remove(&confirm_id) else {
            return;
        };
        self.approvals.decide_ids.remove(&flight.decide_id);
        let outcome = DecisionOutcome::classify(flight.refusal, still_pending);
        if matches!(outcome, DecisionOutcome::Decided | DecisionOutcome::Stale) {
            self.forget_approval(flight.id);
        }
        self.push_event(Event::ApprovalDecision {
            id: flight.id,
            decision: flight.decision,
            outcome,
        });
    }
}
