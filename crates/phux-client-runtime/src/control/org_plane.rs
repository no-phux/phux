//! The control plane's organization lane: the replica's seed and event
//! routing, the operation journal (one `OperationId` per action, resent with
//! the same id after a reconnect inside the horizon, reported unknown past
//! it), and the typed commands a consumer submits.
//!
//! Everything here is inert against a server that does not advertise
//! `ORGANIZATION`: no seed is read, submissions resolve
//! [`OrgOutcome::NotSent`], and `close_terminals` keeps its legacy
//! `CLOSE_TAB_RESOURCES` path.

use std::collections::HashSet;
use std::time::Instant;

use phux_client_core::org_ops::{
    ORG_JOURNAL_MAX_OPS, OrgJournal, OrgOutcome, OrgReport, OrganizationCommand, SubmitError,
};
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{ArrangementKey, OperationId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, FrameKind, OrgFilter, Refusal,
};

mod bulk;
mod commands;

use bulk::{BulkMove, BulkPhase};
pub use commands::ArrangementSubmitError;

use super::organization::ReplicaAction;
use super::{ControlPlane, Event, Pending, Topology};

/// One `close_terminals` batch answered by per-terminal terminates.
#[derive(Debug)]
pub(super) struct CloseBatch {
    request_id: u32,
    operations: HashSet<OperationId>,
    errors: Vec<String>,
}

/// The lane's per-plane state.
#[derive(Debug, Default)]
pub(super) struct OrgLane {
    pub(super) journal: OrgJournal,
    /// The first full page of this connection named the authority: resends
    /// may go out.
    pub(super) ready: bool,
    pub(super) closes: Vec<CloseBatch>,
    /// Terminals created by session key for a spawn request whose session
    /// had no live member to address: operation to spawn request id.
    pub(super) spawns: Vec<(OperationId, u32)>,
    moves: Vec<BulkMove>,
    deferred: usize,
    seed_has_cursor: bool,
    seed_restarts: u8,
}

impl ControlPlane {
    // ----- observation ----------------------------------------------------

    /// The organization replica: sessions, resources, arrangements, and the
    /// Intake, current through [`super::organization::OrganizationReplica::journal_seq`].
    #[must_use]
    pub const fn organization(&self) -> &super::organization::OrganizationReplica {
        &self.organization
    }

    /// Whether the connected server advertises `ORGANIZATION`.
    #[must_use]
    pub fn organization_supported(&self) -> bool {
        self.server
            .as_ref()
            .is_some_and(|server| server.has_ext(ServerFeatureExt::Organization))
    }

    /// Whether the connected server advertises `SESSION_PROJECT`, so
    /// [`Self::set_session_project`] can be sent.
    #[must_use]
    pub fn session_project_supported(&self) -> bool {
        self.server
            .as_ref()
            .is_some_and(|server| server.has_ext(ServerFeatureExt::SessionProject))
    }

    /// Unresolved organization operations, including deferred bulk chunks.
    pub fn pending_organization_operations(&self) -> impl Iterator<Item = OperationId> + '_ {
        self.org.journal.pending().chain(
            self.org
                .moves
                .iter()
                .flat_map(|run| run.remaining.iter().map(|(id, _)| *id)),
        )
    }

    // ----- seeding and events --------------------------------------------

    /// Reseed the organization replica from the first page; `None` before
    /// `HELLO_OK` or against a server without `ORGANIZATION`.
    pub fn refresh_organization(&mut self) -> Option<u32> {
        if !self.handshake_ready || !self.organization_supported() {
            return None;
        }
        Some(self.queue_org_seed(OrgFilter::All))
    }

    pub(super) fn queue_org_seed(&mut self, filter: OrgFilter) -> u32 {
        if self.organization.seeding()
            && let Some((request_id, _)) = self
                .pending
                .iter()
                .find(|(_, pending)| matches!(pending, Pending::OrgPage(_)))
        {
            return *request_id;
        }
        let command = self.organization.begin_seed(filter);
        self.org.seed_restarts = 0;
        self.queue_org_read(command, Pending::OrgPage(filter))
    }

    fn queue_org_read(&mut self, command: Command, pending: Pending) -> u32 {
        if let Command::GetOrganization { cursor, .. } = &command {
            self.org.seed_has_cursor = cursor.is_some();
        }
        let request_id = self.next_request_id();
        self.pending.insert(request_id, pending);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command,
        });
        request_id
    }

    /// Read `arrangement` into the replica; the result is
    /// [`Event::OrganizationChanged`]. `None` without `ORGANIZATION`.
    pub fn fetch_arrangement(&mut self, arrangement: ArrangementKey) -> Option<u32> {
        if !self.handshake_ready || !self.organization_supported() {
            return None;
        }
        Some(self.queue_org_read(
            Command::ArrangementGet { arrangement },
            Pending::OrgArrangement(arrangement),
        ))
    }

    /// A connection negotiated: resolve what can no longer be resent, then
    /// seed the replica (resends wait for the seed to name the authority).
    pub(super) fn org_connection_negotiated(&mut self, new_incarnation: bool) {
        let supported = self.organization_supported();
        if new_incarnation || !supported {
            self.organization.reset();
        }
        self.org.ready = false;
        let now_ms = self.now_ms();
        let reports = self.org.journal.begin_connection(supported, now_ms);
        self.handle_org_reports(reports);
        if !supported {
            self.cancel_bulk_moves("the server does not advertise ORGANIZATION");
        }
        if supported && self.options.automatic_lifecycle {
            self.queue_org_seed(OrgFilter::All);
        }
    }

    pub(super) fn org_connection_lost(&mut self) {
        self.org.journal.connection_lost();
        self.org.ready = false;
        if self.organization.seeding() {
            self.organization.abandon_seed();
        }
    }

    /// The session ends: every unresolved operation resolves now.
    pub(super) fn org_strand(&mut self, why: &str) {
        let reports = self.org.journal.drain(why);
        self.handle_org_reports(reports);
        self.cancel_bulk_moves(why);
    }

    /// A `GET_ORGANIZATION` page or `ARRANGEMENT_GET` reply.
    pub(super) fn resolve_org_read(&mut self, pending: &Pending, result: CommandResult) {
        match (pending, result) {
            (Pending::OrgPage(_), CommandResult::OkWith(CommandValue::OrganizationPage(page))) => {
                let action = self.organization.apply_page(*page);
                self.act_on_replica(action);
            }
            (
                Pending::OrgArrangement(_),
                CommandResult::OkWith(CommandValue::Arrangement(value)),
            ) => {
                self.organization.apply_arrangement(*value);
                self.organization_changed();
            }
            (Pending::OrgPage(filter), CommandResult::Refused(refusal)) => {
                self.resolve_org_read_refusal(*filter, &refusal);
            }
            (Pending::OrgPage(_), other) => {
                self.organization.abandon_seed();
                self.cancel_bulk_moves("the organization read failed");
                self.push_event(Event::OrganizationUnavailable {
                    message: format!("GET_ORGANIZATION answered {other:?}"),
                });
            }
            (Pending::OrgArrangement(key), other) => {
                self.push_event(Event::OrganizationUnavailable {
                    message: format!("ARRANGEMENT_GET of {key} answered {other:?}"),
                });
            }
            _ => {}
        }
    }

    fn resolve_org_read_refusal(&mut self, filter: OrgFilter, refusal: &Refusal) {
        // A rejected opaque continuation can be replaced once. Authorization,
        // support and store failures are not evidence of cursor invalidation.
        let restart = self.org.seed_has_cursor
            && self.org.seed_restarts == 0
            && matches!(refusal, Refusal::InvalidArgument { field, .. } if field == "cursor");
        self.organization.abandon_seed();
        if restart {
            self.queue_org_seed(filter);
            self.org.seed_restarts = 1;
            return;
        }
        self.cancel_bulk_moves("the organization read was refused");
        self.push_event(Event::OrganizationUnavailable {
            message: format!("GET_ORGANIZATION answered {refusal:?}"),
        });
    }

    fn act_on_replica(&mut self, action: ReplicaAction) {
        match action {
            ReplicaAction::None => {}
            ReplicaAction::Changed => self.organization_changed(),
            ReplicaAction::NextPage(command) => {
                let filter = match &command {
                    Command::GetOrganization { filter, .. } => *filter,
                    _ => OrgFilter::All,
                };
                self.queue_org_read(command, Pending::OrgPage(filter));
            }
            ReplicaAction::Seeded => self.org_seeded(),
            ReplicaAction::Reseed => {
                self.queue_org_seed(OrgFilter::All);
            }
            ReplicaAction::RefetchSession(session) => {
                if !self.organization.seeding() {
                    self.queue_org_seed(OrgFilter::Session(session));
                }
                self.organization_changed();
            }
            ReplicaAction::RefetchArrangement(arrangement) => {
                self.queue_org_read(
                    Command::ArrangementGet { arrangement },
                    Pending::OrgArrangement(arrangement),
                );
            }
        }
        self.advance_bulk_moves();
    }

    fn org_seeded(&mut self) {
        self.bulk_moves_seeded();
        if !self.org.ready
            && let Some(authority) = self.organization.authority()
        {
            self.org.ready = true;
            let reports = self.org.journal.set_authority(authority);
            self.handle_org_reports(reports);
            self.flush_org_journal();
        }
        self.organization_changed();
    }

    /// Route one journal event to the replica.
    pub(super) fn organization_event(&mut self, seq: Option<u64>, event: &AgentEvent) {
        let action = self.organization.apply_event(seq, event);
        self.act_on_replica(action);
    }

    /// Re-project the topology with the replica and tell the consumer.
    fn organization_changed(&mut self) {
        if let Some(topology) = self.topology.take() {
            self.topology = Some(topology.with_organization(&self.organization));
        }
        self.push_event(Event::OrganizationChanged);
    }

    /// Project a snapshot with what the replica knows.
    pub(super) fn project_topology(
        &mut self,
        snapshot: &phux_protocol::wire::info::SessionSnapshot,
    ) -> Topology {
        self.organization.observe_snapshot_keys(
            snapshot
                .sessions
                .iter()
                .filter_map(|session| session.key.map(|key| (key, session.id))),
        );
        Topology::from_snapshot(snapshot).with_organization(&self.organization)
    }

    // ----- the journal -----------------------------------------------------

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
    pub(super) fn journal_org_operation(
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

    pub(super) fn flush_org_journal(&mut self) {
        if !self.org.ready || !self.handshake_ready {
            return;
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
    pub(super) fn owns_org_request(&self, request_id: u32) -> bool {
        self.org.journal.owns(request_id)
    }

    pub(super) fn resolve_org_request(&mut self, request_id: u32, result: &CommandResult) {
        if let Some(report) = self.org.journal.resolve(request_id, result) {
            self.handle_org_reports(vec![report]);
        }
    }

    /// Resolve every attempted operation past the retry horizon. Returns
    /// whether anything resolved.
    pub(super) fn expire_org(&mut self) -> bool {
        let before = self.events.len();
        let now_ms = self.now_ms();
        let reports = self.org.journal.expire(now_ms);
        self.handle_org_reports(reports);
        let now = Instant::now();
        self.expire_bulk_reads(now);
        self.events.len() != before
    }

    /// The earliest bulk-inventory deadline.
    pub(super) fn next_org_deadline(&self) -> Option<Instant> {
        self.org
            .moves
            .iter()
            .filter(|run| !matches!(run.phase, BulkPhase::Submitted(_)))
            .map(|run| run.deadline)
            .min()
    }

    fn handle_org_reports(&mut self, reports: Vec<OrgReport>) {
        for report in reports {
            if let Some(position) = self
                .org
                .closes
                .iter()
                .position(|batch| batch.operations.contains(&report.operation))
            {
                let batch = &mut self.org.closes[position];
                batch.operations.remove(&report.operation);
                if !report.outcome.succeeded() {
                    batch.errors.push(outcome_sentence(&report.outcome));
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
            if self.resolve_session_spawn(&report) {
                continue;
            }
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

pub(super) fn outcome_sentence(outcome: &OrgOutcome) -> String {
    match outcome {
        OrgOutcome::Refused(refusal) => phux_client_core::org_ops::refusal_sentence(refusal),
        OrgOutcome::Error { message, .. } => message.clone(),
        OrgOutcome::Unknown { reason } => format!("outcome unknown: {reason}"),
        OrgOutcome::NotSent { reason } => format!("not sent: {reason}"),
        other => format!("{other:?}"),
    }
}
