//! The typed organization commands a consumer submits.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

use phux_client_core::layout::ArrangementDoc as HandleDoc;
use phux_client_core::layout::wire::{WireError, to_protocol};
use phux_client_core::org_ops::{
    ORG_JOURNAL_MAX_OPS, ORG_RETRY_HORIZON_MS, SubmitError, live_target,
};
use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{OperationId, ResourceKey, SessionKey};
use phux_protocol::wire::frame::{
    Command, Committed, MAX_ORG_BATCH, SessionLifetime, TerminateMode,
};

use super::super::{ControlPlane, Event};
use super::{BulkMove, BulkPhase, CloseBatch, mint_operation};

impl ControlPlane {
    pub(super) fn session_record(
        &self,
        session: &SessionKey,
    ) -> Result<&phux_protocol::wire::frame::SessionRecord, SubmitError> {
        self.organization
            .session(session)
            .ok_or(SubmitError::NotAnOperation)
    }

    /// Rename a session (`SESSION_RENAME`; the last writer wins).
    ///
    /// # Errors
    ///
    /// [`Self::submit_organization`]'s errors.
    pub fn rename_session(
        &mut self,
        session: SessionKey,
        name: &str,
    ) -> Result<OperationId, SubmitError> {
        self.submit_organization(Command::SessionRename {
            operation: mint_operation(),
            session,
            name: name.to_owned(),
        })
    }

    /// Link a session into a named project, or clear its link
    /// (`SESSION_SET_PROJECT`; the last writer wins). The link is the
    /// session's `"project.link"` attribute, separate from the free-text
    /// `"project"` label; clients group sessions across hosts by it ahead of
    /// the repository identity.
    ///
    /// # Errors
    ///
    /// [`SubmitError::Unsupported`] unless the connected server advertises
    /// `SESSION_PROJECT`; [`Self::submit_organization`]'s errors.
    pub fn set_session_project(
        &mut self,
        session: SessionKey,
        project: Option<&str>,
    ) -> Result<OperationId, SubmitError> {
        if !self.handshake_ready || !self.session_project_supported() {
            return Err(SubmitError::Unsupported);
        }
        self.submit_organization(Command::SessionSetProject {
            operation: mint_operation(),
            session,
            project: project.map(str::to_owned),
        })
    }

    /// Make a session persistent or ephemeral (the last writer wins).
    ///
    /// # Errors
    ///
    /// As [`Self::rename_session`].
    pub fn set_session_lifetime(
        &mut self,
        session: SessionKey,
        lifetime: SessionLifetime,
    ) -> Result<OperationId, SubmitError> {
        self.submit_organization(Command::SessionSetLifetime {
            operation: mint_operation(),
            session,
            lifetime,
        })
    }

    /// Delete an empty session; refused `NOT_EMPTY` while it has members.
    ///
    /// # Errors
    ///
    /// As [`Self::rename_session`].
    pub fn delete_session(&mut self, session: SessionKey) -> Result<OperationId, SubmitError> {
        self.submit_organization(Command::SessionDelete {
            operation: mint_operation(),
            session,
        })
    }

    /// Kill a whole session (`SESSION_KILL`): the server ends every member,
    /// work that joined meanwhile included, and deletes the session. The
    /// outcome is [`Event::OrganizationOperation`]; its receipt counts the
    /// members ended.
    ///
    /// # Errors
    ///
    /// [`SubmitError::Unsupported`] unless the connected server advertises
    /// `SESSION_KILL`; [`Self::submit_organization`]'s errors.
    pub fn kill_session(&mut self, session: SessionKey) -> Result<OperationId, SubmitError> {
        let supported = self.handshake_ready
            && self
                .server
                .as_ref()
                .is_some_and(|server| server.has_ext(ServerFeatureExt::SessionKill));
        if !supported {
            return Err(SubmitError::Unsupported);
        }
        self.submit_organization(Command::SessionKill {
            operation: mint_operation(),
            session,
        })
    }

    /// Clear a session's Intake mark using the legacy-compatible wire command.
    ///
    /// # Errors
    ///
    /// Session absent from the replica or submission refused.
    pub fn intake_resolve(&mut self, session: SessionKey) -> Result<OperationId, SubmitError> {
        let expected = self.session_record(&session)?.revision;
        self.submit_organization(Command::intake_resolve(mint_operation(), session, expected))
    }

    /// Move one resource into `to` (membership only; it keeps running).
    ///
    /// # Errors
    ///
    /// As [`Self::intake_resolve`], for the resource or either session.
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
    /// operation and outcome. Chunks run sequentially, refreshing the replica
    /// after each receipt; the whole selection is not atomic. A failed or
    /// uncertain chunk cancels every remaining chunk as `NotSent`.
    ///
    /// # Errors
    ///
    /// As [`Self::intake_resolve`]; nothing is submitted when any resource
    /// or session is unknown, a resource is duplicated, or capacity is full.
    pub fn move_resources(
        &mut self,
        resources: &[ResourceKey],
        to: SessionKey,
    ) -> Result<Vec<OperationId>, SubmitError> {
        let mut chunks = self.plan_bulk_move(resources, to)?;
        let operations = chunks.iter().map(|(id, _)| *id).collect();
        let Some((operation, first)) = chunks.pop_front() else {
            return Ok(operations);
        };
        let command = self.bulk_move_command(operation, &first, to)?;
        let authority = self
            .organization
            .authority()
            .ok_or(SubmitError::NotAnOperation)?;
        self.journal_org_operation(command)?;
        if !chunks.is_empty() {
            self.org.deferred += chunks.len();
            self.org.moves.push(BulkMove {
                authority,
                to,
                remaining: chunks,
                phase: BulkPhase::Submitted(operation),
                deadline: Instant::now() + Duration::from_millis(ORG_RETRY_HORIZON_MS),
            });
        }
        if self.handshake_ready && !self.organization_supported() {
            let reports = self.org.journal.begin_connection(false, self.now_ms());
            self.handle_org_reports(reports);
        }
        self.flush_org_journal();
        Ok(operations)
    }

    fn plan_bulk_move(
        &self,
        resources: &[ResourceKey],
        to: SessionKey,
    ) -> Result<VecDeque<(OperationId, Vec<ResourceKey>)>, SubmitError> {
        self.session_record(&to)?;
        if resources.iter().collect::<HashSet<_>>().len() != resources.len() {
            return Err(SubmitError::NotAnOperation);
        }
        let chunks = resources
            .chunks(MAX_ORG_BATCH)
            .map(|chunk| {
                let operation = mint_operation();
                self.bulk_move_command(operation, chunk, to)?;
                Ok((operation, chunk.to_vec()))
            })
            .collect::<Result<VecDeque<_>, SubmitError>>()?;
        if self.org.journal.len() + self.org.deferred + chunks.len() > ORG_JOURNAL_MAX_OPS {
            return Err(SubmitError::Full);
        }
        Ok(chunks)
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

    /// Remove an inactive resource record; refused while it runs.
    ///
    /// # Errors
    ///
    /// As [`Self::rename_session`].
    pub fn remove_resource(&mut self, resource: ResourceKey) -> Result<OperationId, SubmitError> {
        self.submit_organization(Command::ResourceRemove {
            operation: mint_operation(),
            resource,
        })
    }

    /// Start a new activation of an inactive resource.
    ///
    /// # Errors
    ///
    /// As [`Self::intake_resolve`].
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
            slot_tabs: false,
        })?)
    }

    /// Close terminals. Against a server that advertises `ORGANIZATION`
    /// each is terminated (`RESOURCE_TERMINATE`, which leaves membership and
    /// arrangement to the server); otherwise one atomic `CLOSE_TAB_RESOURCES`
    /// when the server advertises it. The reply is
    /// [`Event::TerminalsClosed`] under the returned id; `None` when neither
    /// path is available.
    pub(in crate::control) fn close_terminals_by_record(
        &mut self,
        ids: &[ResourceId],
    ) -> Option<u32> {
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
            match self.journal_org_operation(Command::ResourceTerminate {
                operation: mint_operation(),
                target,
                mode: TerminateMode::Graceful,
            }) {
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
