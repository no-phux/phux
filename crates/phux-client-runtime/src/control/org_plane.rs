//! The control plane's organization lane: the replica's seed and event
//! routing, the operation journal (one `OperationId` per action, resent with
//! the same id after a reconnect inside the horizon, reported unknown past
//! it), and the typed commands a consumer submits.
//!
//! Everything here is inert against a server that does not advertise
//! `ORGANIZATION`: no seed is read, submissions resolve
//! [`OrgOutcome::NotSent`], and `close_terminals` can use the advertised
//! legacy `CLOSE_TAB_RESOURCES` path.
//!
//! [`OrgOutcome::NotSent`]: phux_client_core::org_ops::OrgOutcome::NotSent

use std::collections::{HashMap, HashSet};

use phux_client_core::org_ops::OrgJournal;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{ArrangementKey, OperationId, ResourceId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, FrameKind, OrgFilter, Refusal,
};

mod bulk;
mod commands;
mod journal;
#[cfg(test)]
mod tests;

use bulk::{BulkMove, BulkPhase};
pub use commands::ArrangementSubmitError;
pub use journal::mint_operation;
pub(super) use journal::outcome_sentence;

use super::organization::ReplicaAction;
use super::{ControlPlane, Event, Pending, Topology};

/// One `close_terminals` batch answered by per-terminal terminates.
#[derive(Debug)]
pub(super) struct CloseBatch {
    request_id: u32,
    operations: HashMap<OperationId, ResourceId>,
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
    /// Lookup-only recovery across incarnation change, fenced to authority/key.
    pub(super) pending_attach: Option<(
        phux_protocol::ids::AuthorityId,
        phux_protocol::ids::SessionKey,
    )>,
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

    /// Whether the connected server advertises `SLOT_TABS`, so an
    /// arrangement write carries slot tabs instead of the server keeping its
    /// own (`docs/organization.md#slot-tabs`).
    #[must_use]
    pub fn slot_tabs_supported(&self) -> bool {
        self.server
            .as_ref()
            .is_some_and(|server| server.has_ext(ServerFeatureExt::SlotTabs))
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
        Some(self.queue_arrangement_read(arrangement))
    }

    fn queue_arrangement_read(&mut self, arrangement: ArrangementKey) -> u32 {
        self.queue_org_read(
            Command::ArrangementGet { arrangement },
            Pending::OrgArrangement(arrangement),
        )
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
            if self.org.pending_attach.take().is_some() {
                self.attach_target = None;
                self.error = Some(
                    "cannot recover an exact session key on a peer without ORGANIZATION".into(),
                );
                self.set_status(super::Status::Failed);
            }
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
        self.org.pending_attach = None;
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

    fn refresh_seeded_arrangements(
        &mut self,
        filter: OrgFilter,
        mut keys: HashSet<ArrangementKey>,
    ) {
        // The seed's journal cut can cover an ArrangementChanged in the same
        // transaction as a ResourceCreated, but pages carry only the key, not
        // the arrangement itself. Re-read cached documents in the seeded scope.
        let seeded = self
            .organization
            .sessions()
            .iter()
            .filter(|session| match filter {
                OrgFilter::All => true,
                OrgFilter::Session(key) => session.key == key,
                _ => false,
            })
            .filter_map(|session| session.arrangement);
        keys.extend(seeded);
        keys.retain(|key| self.organization.arrangement(key).is_some());
        for key in keys {
            self.queue_arrangement_read(key);
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
            ReplicaAction::Seeded {
                filter,
                arrangements,
            } => {
                self.refresh_seeded_arrangements(filter, arrangements);
                self.org_seeded();
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
                self.queue_arrangement_read(arrangement);
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
        self.resume_keyed_attach();
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
}
