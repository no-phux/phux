//! The operation journal: every mutating organization command gets one
//! `OperationId`, is sent once a ready `ORGANIZATION` connection exists,
//! resent with the same id after a reconnect inside the horizon, and resolved
//! into [`Event::OrganizationOperation`] reports.

use std::time::Instant;

use phux_client_core::org_ops::{
    ORG_JOURNAL_MAX_OPS, OrgOutcome, OrgReport, OrganizationCommand, SubmitError,
};
use phux_protocol::ids::OperationId;
use phux_protocol::wire::frame::{Command, CommandResult};

use super::super::{ControlPlane, Event};
use super::bulk::BulkPhase;

impl ControlPlane {
    /// Submit a mutating organization command. It is sent now, or once a
    /// connection to an `ORGANIZATION` server has named its authority, and
    /// resent with the same operation id after a reconnect inside the retry
    /// horizon. The outcome is [`Event::OrganizationOperation`].
    ///
    /// # Errors
    ///
    /// [`SubmitError`] — a read, a reused id with another payload, or a full
    /// journal.
    pub fn submit_organization(&mut self, command: Command) -> Result<OperationId, SubmitError> {
        if matches!(command, Command::ResourceRelaunch { .. })
            && !self.server.as_ref().is_some_and(|server| {
                server
                    .features_ext2
                    .contains(phux_protocol::caps::ServerFeatureExt2::ArchivedLaunchAdmission)
            })
        {
            return Err(SubmitError::Unsupported);
        }
        let now_ms = self.now_ms();
        let operation = self.journal_org_operation(command)?;
        if self.handshake_ready && !self.organization_supported() {
            // This server cannot take it; a later connection to one that can
            // must not run it unasked.
            let reports = self.org.journal.begin_connection(false, now_ms);
            self.handle_org_reports(reports);
            return Ok(operation);
        }
        self.flush_org_journal();
        Ok(operation)
    }

    /// All producers use this gate: deferred chunks reserve real journal
    /// slots, and an exact retry of an already journaled operation needs none.
    pub(in crate::control) fn journal_org_operation(
        &mut self,
        command: Command,
    ) -> Result<OperationId, SubmitError> {
        let operation = command
            .organization_operation()
            .ok_or(SubmitError::NotAnOperation)?;
        if self.org.journal.command(operation).is_none()
            && self.org.journal.len() + self.org.deferred >= ORG_JOURNAL_MAX_OPS
        {
            return Err(SubmitError::Full);
        }
        if self
            .org
            .moves
            .iter()
            .any(|run| run.remaining.iter().any(|(id, _)| id == operation))
        {
            return Err(SubmitError::DuplicateOperation);
        }
        self.org.journal.submit(command, self.now_ms())
    }

    pub(in crate::control) fn flush_org_journal(&mut self) {
        if !self.org.ready || !self.handshake_ready {
            return;
        }
        if !self.server.as_ref().is_some_and(|server| {
            server
                .features_ext2
                .contains(phux_protocol::caps::ServerFeatureExt2::ArchivedLaunchAdmission)
        }) {
            // Reports re-enter this flush through the bulk-move advance; only
            // an actual rejection may do so, or every flush recurses.
            let reports = self.org.journal.reject_unenforced_relaunches();
            if !reports.is_empty() {
                self.handle_org_reports(reports);
            }
        }
        let mut seq = self.request_seq;
        let frames = self.org.journal.next_frames(&mut || {
            let id = seq;
            seq = seq.wrapping_add(1).max(1);
            id
        });
        self.request_seq = seq;
        for frame in &frames {
            self.queue_frame(frame);
        }
    }

    /// Whether `request_id` is an organization operation's attempt.
    pub(in crate::control) fn owns_org_request(&self, request_id: u32) -> bool {
        self.org.journal.owns(request_id)
    }

    pub(in crate::control) fn resolve_org_request(
        &mut self,
        request_id: u32,
        result: &CommandResult,
    ) {
        if let Some(report) = self.org.journal.resolve(request_id, result) {
            self.handle_org_reports(vec![report]);
        }
    }

    /// Resolve every attempted operation past the retry horizon. Returns
    /// whether anything resolved.
    pub(in crate::control) fn expire_org(&mut self) -> bool {
        let before = self.events.len();
        let now_ms = self.now_ms();
        let reports = self.org.journal.expire(now_ms);
        self.handle_org_reports(reports);
        let now = self.observed_time();
        self.expire_bulk_reads(now);
        self.events.len() != before
    }

    /// The earliest bulk-inventory deadline.
    pub(in crate::control) fn next_org_deadline(&self) -> Option<Instant> {
        self.org
            .moves
            .iter()
            .filter(|run| !matches!(run.phase, BulkPhase::Submitted(_)))
            .map(|run| run.deadline)
            .min()
    }

    pub(super) fn handle_org_reports(&mut self, reports: Vec<OrgReport>) {
        for report in reports {
            if let Some(position) = self
                .org
                .closes
                .iter()
                .position(|batch| batch.operations.contains_key(&report.operation))
            {
                let batch = &mut self.org.closes[position];
                if let Some(id) = batch.operations.remove(&report.operation)
                    && !report.outcome.succeeded()
                {
                    batch
                        .errors
                        .push(format!("{id}: {}", outcome_sentence(&report.outcome)));
                }
                if batch.operations.is_empty() {
                    let batch = self.org.closes.remove(position);
                    self.push_event(Event::TerminalsClosed {
                        request_id: batch.request_id,
                        error: (!batch.errors.is_empty()).then(|| batch.errors.join("; ")),
                    });
                }
                continue;
            }
            self.resolve_session_spawn(&report);
            let cancelled = self.resolve_bulk_move(&report);
            self.push_event(Event::OrganizationOperation {
                operation: report.operation,
                outcome: report.outcome,
            });
            if let Some(run) = cancelled {
                self.cancel_bulk_tail(run, "a prior bulk move chunk did not commit");
            }
        }
        self.advance_bulk_moves();
    }
}

/// A fresh random operation id.
#[must_use]
pub fn mint_operation() -> OperationId {
    loop {
        if let Some(id) = OperationId::new(*uuid::Uuid::new_v4().as_bytes()) {
            return id;
        }
    }
}

pub(in crate::control) fn outcome_sentence(outcome: &OrgOutcome) -> String {
    match outcome {
        OrgOutcome::Refused(refusal) => phux_client_core::org_ops::refusal_sentence(refusal),
        OrgOutcome::Error { message, .. } => message.clone(),
        OrgOutcome::Unknown { reason } => format!("outcome unknown: {reason}"),
        OrgOutcome::NotSent { reason } => format!("not sent: {reason}"),
        other => format!("{other:?}"),
    }
}
