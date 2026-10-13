//! The control plane's organization lane: the replica's seed and event
//! routing, the operation journal (one `OperationId` per action, resent with
//! the same id after a reconnect inside the horizon, reported unknown past
//! it), the kill-session workflow, and the typed commands a consumer submits.
//!
//! Everything here is inert against a server that does not advertise
//! `ORGANIZATION`: no seed is read, submissions resolve
//! [`OrgOutcome::NotSent`], and `close_terminals` keeps its legacy
//! `CLOSE_TAB_RESOURCES` path.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use phux_client_core::layout::ArrangementDoc as HandleDoc;
use phux_client_core::layout::wire::{WireError, to_protocol};
use phux_client_core::org_ops::{
    KillWorkflow, OrgJournal, OrgOutcome, OrgReport, SubmitError, live_target,
};
use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{ArrangementKey, OperationId, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, Committed, FrameKind, MAX_ORG_BATCH,
    MoveItem, OrgFilter, SessionLifetime, TerminateMode,
};

use super::organization::{RecordChange, ReplicaAction};
use super::{ControlPlane, Event, Pending, Topology};

/// How long a kill workflow waits for terminated members to end before it
/// reports them timed out; a graceful terminate is forced halfway.
pub const KILL_WAIT: Duration = Duration::from_secs(4);

/// One running kill-session workflow.
#[derive(Debug)]
pub(super) struct KillRun {
    id: u64,
    workflow: KillWorkflow,
    operations: HashSet<OperationId>,
    escalate_at: Instant,
    deadline: Instant,
    escalated: bool,
}

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
    pub(super) kills: Vec<KillRun>,
    pub(super) closes: Vec<CloseBatch>,
    pub(super) next_run: u64,
}

impl ControlPlane {
    // ----- observation ----------------------------------------------------

    /// The organization replica: sessions, resources, arrangements, and the
    /// Inbox, current through [`super::organization::OrganizationReplica::journal_seq`].
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

    /// Unresolved organization operations, in submission order.
    pub fn pending_organization_operations(&self) -> impl Iterator<Item = OperationId> + '_ {
        self.org.journal.pending()
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
        self.queue_org_read(command, Pending::OrgPage(filter))
    }

    fn queue_org_read(&mut self, command: Command, pending: Pending) -> u32 {
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
        for mut run in std::mem::take(&mut self.org.kills) {
            run.workflow.time_out();
            let id = run.id;
            self.push_event(Event::KillFinished {
                workflow: id,
                report: Box::new(run.workflow.report()),
            });
        }
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
            (Pending::OrgPage(OrgFilter::All), CommandResult::Refused(_)) => {
                // An expired cursor: start over from the first page.
                self.organization.abandon_seed();
                self.queue_org_seed(OrgFilter::All);
            }
            (Pending::OrgPage(_), other) => {
                self.organization.abandon_seed();
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
            ReplicaAction::Seeded => {
                if !self.org.ready
                    && let Some(authority) = self.organization.authority()
                {
                    self.org.ready = true;
                    let reports = self.org.journal.set_authority(authority);
                    self.handle_org_reports(reports);
                    self.flush_org_journal();
                }
                self.refresh_kill_runs();
                self.organization_changed();
            }
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
    }

    /// Route one journal event to the replica and the kill workflows.
    pub(super) fn organization_event(&mut self, seq: Option<u64>, event: &AgentEvent) {
        let (action, change) = self.organization.apply_event(seq, event);
        match change {
            RecordChange::Activation {
                resource,
                activation,
            } => {
                for run in &mut self.org.kills {
                    run.workflow.on_activation(&resource, activation.state);
                }
            }
            RecordChange::ResourceRemoved(resource) => {
                for run in &mut self.org.kills {
                    run.workflow.on_removed(&resource);
                }
            }
            RecordChange::SessionDeleted(session) => {
                for run in self
                    .org
                    .kills
                    .iter_mut()
                    .filter(|run| run.workflow.session() == session)
                {
                    run.workflow.on_records(None, &[]);
                }
            }
            RecordChange::Other => {}
        }
        self.act_on_replica(action);
        self.advance_kill_runs();
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
        let operation = self.org.journal.submit(command, now_ms)?;
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

    fn flush_org_journal(&mut self) {
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

    /// Resolve every attempted operation past the retry horizon, and time
    /// out kill workflows. Returns whether anything resolved.
    pub(super) fn expire_org(&mut self) -> bool {
        let before = self.events.len();
        let now_ms = self.now_ms();
        let reports = self.org.journal.expire(now_ms);
        self.handle_org_reports(reports);
        let now = Instant::now();
        let mut escalate = Vec::new();
        for run in &mut self.org.kills {
            if now >= run.deadline {
                run.workflow.time_out();
            } else if !run.escalated && now >= run.escalate_at {
                run.escalated = true;
                escalate.push(run.id);
            }
        }
        let resources = self.organization.resources().to_vec();
        for run in self
            .org
            .kills
            .iter_mut()
            .filter(|run| escalate.contains(&run.id))
        {
            run.workflow.escalate(&resources);
        }
        self.advance_kill_runs();
        self.events.len() != before
    }

    /// The earliest kill-workflow deadline.
    pub(super) fn next_org_deadline(&self) -> Option<Instant> {
        self.org
            .kills
            .iter()
            .map(|run| {
                if run.escalated {
                    run.deadline
                } else {
                    run.escalate_at
                }
            })
            .min()
    }

    fn handle_org_reports(&mut self, reports: Vec<OrgReport>) {
        for report in reports {
            if let Some(run) = self
                .org
                .kills
                .iter_mut()
                .find(|run| run.operations.contains(&report.operation))
            {
                run.operations.remove(&report.operation);
                run.workflow.on_result(&report.operation, &report.outcome);
                continue;
            }
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
            self.push_event(Event::OrganizationOperation {
                operation: report.operation,
                outcome: report.outcome,
            });
        }
        self.advance_kill_runs();
    }

    // ----- kill workflows --------------------------------------------------

    /// Kill a whole session by the CONTRACTS §04 workflow: terminate each
    /// executing member, wait until each is inactive, remove each, then
    /// delete the session with the revision the workflow last committed
    /// against. The member set is fixed now. The result is
    /// [`Event::KillFinished`] with a per-target report. `None` when the
    /// replica is not live or does not know the session.
    pub fn kill_session(&mut self, session: SessionKey) -> Option<u64> {
        if !self.organization.is_live() {
            return None;
        }
        let record = self.organization.session(&session)?.clone();
        let members: Vec<_> = self.organization.members(&session).cloned().collect();
        let workflow = KillWorkflow::plan(&record, &members);
        let now = Instant::now();
        self.org.next_run += 1;
        let id = self.org.next_run;
        self.org.kills.push(KillRun {
            id,
            workflow,
            operations: HashSet::new(),
            escalate_at: now + KILL_WAIT / 2,
            deadline: now + KILL_WAIT,
            escalated: false,
        });
        self.advance_kill_runs();
        Some(id)
    }

    fn refresh_kill_runs(&mut self) {
        for run in &mut self.org.kills {
            let session = run.workflow.session();
            let resources: Vec<_> = self.organization.members(&session).cloned().collect();
            run.workflow
                .on_records(self.organization.session(&session), &resources);
        }
    }

    fn advance_kill_runs(&mut self) {
        let mut submissions = Vec::new();
        for run in &mut self.org.kills {
            let mut mint = mint_operation;
            for (operation, command) in run.workflow.next_commands(&mut mint) {
                run.operations.insert(operation);
                submissions.push((run.id, operation, command));
            }
        }
        for (id, operation, command) in submissions {
            let now_ms = self.now_ms();
            if let Err(error) = self.org.journal.submit(command, now_ms)
                && let Some(run) = self.org.kills.iter_mut().find(|run| run.id == id)
            {
                run.operations.remove(&operation);
                run.workflow.on_result(
                    &operation,
                    &OrgOutcome::NotSent {
                        reason: error.to_string(),
                    },
                );
            }
        }
        self.flush_org_journal();
        let (done, running): (Vec<KillRun>, Vec<KillRun>) = std::mem::take(&mut self.org.kills)
            .into_iter()
            .partition(|run| run.workflow.is_done());
        self.org.kills = running;
        for run in done {
            self.push_event(Event::KillFinished {
                workflow: run.id,
                report: Box::new(run.workflow.report()),
            });
        }
    }

    // ----- typed commands --------------------------------------------------

    fn session_record(
        &self,
        session: &SessionKey,
    ) -> Result<&phux_protocol::wire::frame::SessionRecord, SubmitError> {
        self.organization
            .session(session)
            .ok_or(SubmitError::NotAnOperation)
    }

    /// Rename a session (`SESSION_RENAME` at the replica's revision).
    ///
    /// # Errors
    ///
    /// [`SubmitError::NotAnOperation`] when the replica does not hold the
    /// session; [`Self::submit_organization`]'s errors.
    pub fn rename_session(
        &mut self,
        session: SessionKey,
        name: &str,
    ) -> Result<OperationId, SubmitError> {
        let expected = self.session_record(&session)?.revision;
        self.submit_organization(Command::SessionRename {
            operation: mint_operation(),
            session,
            expected,
            name: name.to_owned(),
        })
    }

    /// Make a session persistent or ephemeral.
    ///
    /// # Errors
    ///
    /// As [`Self::rename_session`].
    pub fn set_session_lifetime(
        &mut self,
        session: SessionKey,
        lifetime: SessionLifetime,
    ) -> Result<OperationId, SubmitError> {
        let expected = self.session_record(&session)?.revision;
        self.submit_organization(Command::SessionSetLifetime {
            operation: mint_operation(),
            session,
            expected,
            lifetime,
        })
    }

    /// Delete an empty session.
    ///
    /// # Errors
    ///
    /// As [`Self::rename_session`].
    pub fn delete_session(&mut self, session: SessionKey) -> Result<OperationId, SubmitError> {
        let expected = self.session_record(&session)?.revision;
        self.submit_organization(Command::SessionDelete {
            operation: mint_operation(),
            session,
            expected,
        })
    }

    /// Clear a session's Inbox intake mark.
    ///
    /// # Errors
    ///
    /// As [`Self::rename_session`].
    pub fn inbox_resolve(&mut self, session: SessionKey) -> Result<OperationId, SubmitError> {
        let expected = self.session_record(&session)?.revision;
        self.submit_organization(Command::InboxResolve {
            operation: mint_operation(),
            session,
            expected,
        })
    }

    /// Move one resource into `to` (membership only; it keeps running).
    ///
    /// # Errors
    ///
    /// As [`Self::rename_session`], for the resource or either session.
    pub fn move_resource(
        &mut self,
        resource: ResourceKey,
        to: SessionKey,
    ) -> Result<OperationId, SubmitError> {
        let from = self
            .organization
            .resource(&resource)
            .ok_or(SubmitError::NotAnOperation)?
            .session;
        let from_expected = self.session_record(&from)?.revision;
        let to_expected = self.session_record(&to)?.revision;
        self.submit_organization(Command::ResourceMove {
            operation: mint_operation(),
            resource,
            from,
            from_expected,
            to,
            to_expected,
        })
    }

    /// Move many resources into `to`, chunked at the server's batch bound:
    /// one atomic `RESOURCE_MOVE_MANY` per chunk, each with its own
    /// operation and outcome.
    ///
    /// # Errors
    ///
    /// As [`Self::rename_session`]; nothing is submitted when any resource
    /// or session is unknown.
    pub fn move_resources(
        &mut self,
        resources: &[ResourceKey],
        to: SessionKey,
    ) -> Result<Vec<OperationId>, SubmitError> {
        let to_expected = self.session_record(&to)?.revision;
        let mut chunks = Vec::new();
        for chunk in resources.chunks(MAX_ORG_BATCH) {
            let mut items = Vec::with_capacity(chunk.len());
            for resource in chunk {
                let from = self
                    .organization
                    .resource(resource)
                    .ok_or(SubmitError::NotAnOperation)?
                    .session;
                items.push(MoveItem {
                    resource: *resource,
                    from,
                    from_expected: self.session_record(&from)?.revision,
                });
            }
            chunks.push(items);
        }
        chunks
            .into_iter()
            .map(|items| {
                self.submit_organization(Command::ResourceMoveMany {
                    operation: mint_operation(),
                    items,
                    to,
                    to_expected,
                })
            })
            .collect()
    }

    /// Undo a committed organization edit by its receipt.
    ///
    /// # Errors
    ///
    /// [`Self::submit_organization`]'s errors.
    pub fn undo(&mut self, receipt: &Committed) -> Result<OperationId, SubmitError> {
        self.submit_organization(Command::OrganizationUndo {
            operation: mint_operation(),
            undo: receipt.operation,
            expected: receipt.revisions.clone(),
        })
    }

    /// Remove an inactive resource record.
    ///
    /// # Errors
    ///
    /// As [`Self::rename_session`].
    pub fn remove_resource(&mut self, resource: ResourceKey) -> Result<OperationId, SubmitError> {
        let expected = self
            .organization
            .resource(&resource)
            .ok_or(SubmitError::NotAnOperation)?
            .revision;
        self.submit_organization(Command::ResourceRemove {
            operation: mint_operation(),
            resource,
            expected,
        })
    }

    /// Start a new activation of an inactive resource.
    ///
    /// # Errors
    ///
    /// As [`Self::rename_session`].
    pub fn relaunch_resource(&mut self, resource: ResourceKey) -> Result<OperationId, SubmitError> {
        let expected = self
            .organization
            .resource(&resource)
            .ok_or(SubmitError::NotAnOperation)?
            .revision;
        self.submit_organization(Command::ResourceRelaunch {
            operation: mint_operation(),
            resource,
            expected,
        })
    }

    /// Terminate a resource's current activation here.
    ///
    /// # Errors
    ///
    /// [`SubmitError::NotAnOperation`] when nothing executes here.
    pub fn terminate_resource(
        &mut self,
        resource: ResourceKey,
        mode: TerminateMode,
    ) -> Result<OperationId, SubmitError> {
        let target = self
            .organization
            .resource(&resource)
            .and_then(live_target)
            .ok_or(SubmitError::NotAnOperation)?;
        self.submit_organization(Command::ResourceTerminate {
            operation: mint_operation(),
            target,
            mode,
        })
    }

    /// Replace `session`'s arrangement with a handle-space `document`, at the
    /// revision the replica holds (compare-and-swap: a `CONFLICT` outcome
    /// means re-read and redo).
    ///
    /// # Errors
    ///
    /// [`ArrangementSubmitError`] — the replica does not hold the
    /// arrangement, a leaf has no key, or the submission failed.
    pub fn replace_arrangement(
        &mut self,
        session: SessionKey,
        document: &HandleDoc,
    ) -> Result<OperationId, ArrangementSubmitError> {
        let (key, revision, _, binding) = self
            .organization
            .arrangement_document(&session)
            .ok_or(ArrangementSubmitError::NotHeld)?;
        let wire = to_protocol(document, self.organization.handles(), &binding)?;
        Ok(self.submit_organization(Command::ArrangementReplace {
            operation: mint_operation(),
            arrangement: key,
            expected: revision,
            document: wire,
        })?)
    }

    /// Close terminals. Against a server that advertises `ORGANIZATION`
    /// each is terminated (`RESOURCE_TERMINATE`, which leaves membership and
    /// arrangement to the server); otherwise one atomic `CLOSE_TAB_RESOURCES`
    /// when the server advertises it. The reply is
    /// [`Event::TerminalsClosed`] under the returned id; `None` when neither
    /// path is available.
    pub(super) fn close_terminals_by_record(&mut self, ids: &[ResourceId]) -> Option<u32> {
        if !self.organization_supported() || !self.organization.is_live() {
            return None;
        }
        let request_id = self.next_request_id();
        let mut batch = CloseBatch {
            request_id,
            operations: HashSet::new(),
            errors: Vec::new(),
        };
        for id in ids {
            let target = self.organization.resource_by_live(id).and_then(live_target);
            let Some(target) = target else {
                batch.errors.push(format!("{id:?} has no live record here"));
                continue;
            };
            let now_ms = self.now_ms();
            match self.org.journal.submit(
                Command::ResourceTerminate {
                    operation: mint_operation(),
                    target,
                    mode: TerminateMode::Graceful,
                },
                now_ms,
            ) {
                Ok(operation) => {
                    batch.operations.insert(operation);
                }
                Err(error) => batch.errors.push(error.to_string()),
            }
        }
        if batch.operations.is_empty() {
            self.push_event(Event::TerminalsClosed {
                request_id,
                error: (!batch.errors.is_empty()).then(|| batch.errors.join("; ")),
            });
        } else {
            self.org.closes.push(batch);
            self.flush_org_journal();
        }
        Some(request_id)
    }
}

/// Why [`ControlPlane::replace_arrangement`] submitted nothing.
#[derive(Debug, thiserror::Error)]
pub enum ArrangementSubmitError {
    /// The replica does not hold the session's arrangement; fetch it first.
    #[error("the session's arrangement is not loaded")]
    NotHeld,
    /// A leaf names a handle with no resource key.
    #[error(transparent)]
    Wire(#[from] WireError),
    /// The journal refused the submission.
    #[error(transparent)]
    Submit(#[from] SubmitError),
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

fn outcome_sentence(outcome: &OrgOutcome) -> String {
    match outcome {
        OrgOutcome::Refused(refusal) => phux_client_core::org_ops::refusal_sentence(refusal),
        OrgOutcome::Error { message, .. } => message.clone(),
        OrgOutcome::Unknown { reason } => format!("outcome unknown: {reason}"),
        OrgOutcome::NotSent { reason } => format!("not sent: {reason}"),
        other => format!("{other:?}"),
    }
}
