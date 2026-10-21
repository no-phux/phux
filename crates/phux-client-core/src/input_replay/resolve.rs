//! Folding results into verdicts, stranding, and report bookkeeping.

use std::collections::{HashSet, VecDeque};

use phux_protocol::ids::{InputOperationId, ResourceId};
use phux_protocol::input::InputEvent;
use phux_protocol::wire::frame::{CommandResult, ErrorCode};

use super::{
    INPUT_RETRY_HORIZON_MS, InputReplayJournal, MAX_DEFERRED_REPORTS, PendingOp, ReplayDisposition,
    ReplayReport,
};

impl InputReplayJournal {
    /// Fold one `COMMAND_RESULT` for the outstanding attempt into a verdict.
    ///
    /// `Ok` is the receipt. On a first attempt, explicit server errors retain
    /// their pre-handoff refusal meaning except `INPUT_DELIVERY_UNKNOWN`. On a
    /// replay, every non-OK answer is unknown: an older compatible server can
    /// refuse the retry before consulting dedupe even though the original may
    /// have written, so the retry's refusal cannot establish operation-level
    /// certainty.
    ///
    /// Returns `None` for a request id this journal does not own.
    pub fn resolve(&mut self, request_id: u32, result: &CommandResult) -> Option<ReplayReport> {
        if !self.owns(request_id) {
            return None;
        }
        let attempt = self.connection.as_mut()?.in_flight.remove(&request_id)?;
        let position = self
            .ops
            .iter()
            .position(|op| op.operation_id == attempt.operation_id)?;
        let op = self.remove_at(position)?;
        let (disposition, message) = match result {
            CommandResult::Ok | CommandResult::OkWith(_) => {
                (ReplayDisposition::Delivered, String::new())
            }
            CommandResult::Error { code, message } => {
                (error_disposition(*code, op.attempts), message.clone())
            }
            // `CommandResult` is `#[non_exhaustive]`: a reply this build
            // cannot read is not evidence that nothing was written.
            _ => (
                ReplayDisposition::Unknown,
                "the server answered APPLY_INPUT with a result this build cannot read".to_owned(),
            ),
        };
        let report = ReplayReport {
            disposition,
            operation_id: operation_id_hex(&op.operation_id),
            message,
        };
        if disposition == ReplayDisposition::Unknown {
            self.poisoned_terminals.insert(op.terminal_id.clone());
            let blocked = self.strand_terminal(
                &op.terminal_id,
                "an earlier acknowledged input has unknown delivery; read the pane before retrying",
            );
            self.defer_reports(blocked);
        }
        Some(report)
    }

    /// Resolve everything still queued — the no-more-reconnects teardown.
    pub fn drain_unresolved(&mut self, why: &str) -> Vec<ReplayReport> {
        self.connection = None;
        self.strand_all(why)
    }

    /// Strand queued operations that can no longer be replayed against the
    /// current connection: expired, or first-attempted against a different
    /// incarnation.
    pub(super) fn sweep(&mut self, now_ms: u64) -> Vec<ReplayReport> {
        let server_id = self
            .connection
            .as_ref()
            .map(|ctx| ctx.server_id.clone())
            .unwrap_or_default();
        let mut reports = Vec::new();
        let mut poisoned_terminals = HashSet::new();
        let mut kept = VecDeque::with_capacity(self.ops.len());
        while let Some(op) = self.ops.pop_front() {
            let poison_message = poisoned_terminals.contains(&op.terminal_id).then_some(
                "an earlier acknowledged input has unknown delivery; read the pane before retrying",
            );
            if let Some(message) = poison_message {
                reports.push(strand_report(&op, message));
            } else if operation_expired(&op, now_ms) {
                let report = strand_report(&op, "the acknowledged-input retry horizon expired");
                if report.disposition == ReplayDisposition::Unknown {
                    poisoned_terminals.insert(op.terminal_id.clone());
                    self.poisoned_terminals.insert(op.terminal_id.clone());
                }
                reports.push(report);
            } else if op
                .expected_server_id
                .as_ref()
                .is_some_and(|expected| *expected != server_id)
            {
                let report = strand_report(&op, "the server restarted in between");
                if report.disposition == ReplayDisposition::Unknown {
                    poisoned_terminals.insert(op.terminal_id.clone());
                    self.poisoned_terminals.insert(op.terminal_id.clone());
                }
                reports.push(report);
            } else {
                kept.push_back(op);
                continue;
            }
            self.release_accounting(&op);
        }
        self.ops = kept;
        reports
    }

    pub(super) fn strand_all(&mut self, why: &str) -> Vec<ReplayReport> {
        let mut reports = Vec::with_capacity(self.ops.len());
        reports.extend(self.take_reports());
        while let Some(op) = self.ops.pop_front() {
            let report = strand_report(&op, why);
            if report.disposition == ReplayDisposition::Unknown {
                self.poisoned_terminals.insert(op.terminal_id.clone());
            }
            reports.push(report);
            self.release_accounting(&op);
        }
        reports
    }

    pub(super) fn sweep_expired(&mut self, now_ms: u64) -> Vec<ReplayReport> {
        let mut reports = Vec::new();
        let mut index = 0;
        while index < self.ops.len() {
            if !operation_expired(&self.ops[index], now_ms) {
                index += 1;
                continue;
            }
            let Some(op) = self.remove_at(index) else {
                break;
            };
            reports.push(strand_report(
                &op,
                "the acknowledged-input retry horizon expired",
            ));
            if op.attempts > 0 {
                self.poisoned_terminals.insert(op.terminal_id.clone());
                reports.extend(self.strand_terminal(
                    &op.terminal_id,
                    "an earlier acknowledged input has unknown delivery; read the pane before retrying",
                ));
            }
        }
        reports
    }

    pub(super) fn strand_terminal(
        &mut self,
        terminal_id: &ResourceId,
        why: &str,
    ) -> Vec<ReplayReport> {
        let mut reports = Vec::new();
        let mut index = 0;
        while index < self.ops.len() {
            if &self.ops[index].terminal_id != terminal_id {
                index += 1;
                continue;
            }
            let Some(op) = self.remove_at(index) else {
                break;
            };
            reports.push(strand_report(&op, why));
        }
        reports
    }

    pub(super) fn remove_at(&mut self, index: usize) -> Option<PendingOp> {
        let op = self.ops.remove(index)?;
        if let Some(ctx) = self.connection.as_mut() {
            ctx.in_flight
                .retain(|_, attempt| attempt.operation_id != op.operation_id);
        }
        self.release_accounting(&op);
        Some(op)
    }

    pub(super) const fn release_accounting(&mut self, op: &PendingOp) {
        self.retained_events = self.retained_events.saturating_sub(op.events.len());
        self.retained_bytes = self.retained_bytes.saturating_sub(op.retained_bytes);
    }

    pub(super) fn defer_report(&mut self, report: ReplayReport) {
        if self.deferred_reports.len() < MAX_DEFERRED_REPORTS {
            self.deferred_reports.push_back(report);
        } else {
            let count = &mut self.suppressed_reports[disposition_index(report.disposition)];
            *count = count.saturating_add(1);
        }
    }
}

pub(super) const fn disposition_index(disposition: ReplayDisposition) -> usize {
    match disposition {
        ReplayDisposition::Delivered => 0,
        ReplayDisposition::Unknown => 1,
        ReplayDisposition::Refused => 2,
    }
}

/// The stranding verdict: an attempted operation is *unknown* (its bytes may
/// be in the pane), a never-sent one is a deterministic *refusal*.
pub(super) fn strand_report(op: &PendingOp, message: &str) -> ReplayReport {
    ReplayReport {
        disposition: if op.attempts > 0 {
            ReplayDisposition::Unknown
        } else {
            ReplayDisposition::Refused
        },
        operation_id: operation_id_hex(&op.operation_id),
        message: message.to_owned(),
    }
}

pub(super) const fn error_disposition(code: ErrorCode, attempts: u32) -> ReplayDisposition {
    if attempts > 1 {
        return ReplayDisposition::Unknown;
    }
    match code {
        ErrorCode::NotAttached
        | ErrorCode::TerminalNotFound
        | ErrorCode::UnsupportedSatelliteRoute
        | ErrorCode::InvalidCommand
        | ErrorCode::PermissionDenied
        | ErrorCode::ResourceExhausted
        | ErrorCode::UnsafePaste
        | ErrorCode::InputLeaseHeld
        | ErrorCode::CanonicalLimitExceeded
        | ErrorCode::InputNotWritten
        | ErrorCode::WrongResourceKind => ReplayDisposition::Refused,
        _ => ReplayDisposition::Unknown,
    }
}

pub(super) fn retained_event_bytes(events: &[InputEvent], allocation_slots: usize) -> usize {
    events.iter().fold(
        allocation_slots.saturating_mul(std::mem::size_of::<InputEvent>()),
        |total, event| {
            let payload = match event {
                InputEvent::Key(key) => key.text.as_ref().map_or(0, String::capacity),
                InputEvent::Paste(paste) => paste.data.capacity(),
                _ => 0,
            };
            total.saturating_add(payload)
        },
    )
}

pub(super) const fn operation_expired(op: &PendingOp, now_ms: u64) -> bool {
    now_ms.saturating_sub(op.created_at_ms) >= INPUT_RETRY_HORIZON_MS
}

/// Hex of an operation id, for correlation in diagnostics and `--json`.
#[must_use]
pub fn operation_id_hex(operation_id: &InputOperationId) -> String {
    use std::fmt::Write as _;
    operation_id
        .as_bytes()
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}
