use phux_protocol::ids::{ArrangementKey, ResourceKey, Revision, SessionKey};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ExecState, ExitPolicy, LiveTarget, OrgFilter,
    OrganizationPage, Outcome, RecordRevision, Refusal, ResourceRecord, SessionRecord,
};

use super::{OrgFixture, Receipt};

impl OrgFixture {
    /// Answer one organization command.
    pub(in crate::testkit) fn answer(&mut self, command: &Command) -> CommandResult {
        if let Some(operation) = command.idempotency_key().copied() {
            if let Some(receipt) = self.receipts.iter().find(|r| r.operation == operation) {
                return if receipt.command == *command {
                    receipt.result.clone()
                } else {
                    CommandResult::Refused(Refusal::DuplicateOperation)
                };
            }
            let name = command_name(command);
            if let Some(position) = self.refusals.iter().position(|(n, _)| *n == name) {
                let (_, refusal) = self.refusals.remove(position).expect("present");
                return CommandResult::Refused(refusal);
            }
            let (result, inverse) = match self.mutate(command) {
                Ok((committed, inverse)) => (committed, inverse),
                Err(refusal) => (CommandResult::Refused(refusal), None),
            };
            self.receipts.push(Receipt {
                operation,
                command: command.clone(),
                result: result.clone(),
                inverse,
            });
            return result;
        }
        match command {
            Command::GetOrganization {
                filter,
                page_size,
                cursor,
            } => self.page(*filter, *page_size, cursor.as_deref()),
            Command::ArrangementGet { arrangement } => self
                .arrangement_ref(arrangement)
                .map_or(CommandResult::Refused(Refusal::NotFound), |value| {
                    CommandResult::OkWith(CommandValue::Arrangement(Box::new(value.clone())))
                }),
            _ => CommandResult::Refused(Refusal::Unsupported {
                capability: "scripted".to_owned(),
            }),
        }
    }

    fn page(&self, filter: OrgFilter, page_size: u32, cursor: Option<&[u8]>) -> CommandResult {
        let sessions: Vec<&SessionRecord> = self
            .sessions
            .iter()
            .filter(|s| match filter {
                OrgFilter::Session(key) => s.key == key,
                OrgFilter::Intake => s.awaiting_organization || Some(s.key) == self.inbox,
                _ => true,
            })
            .collect();
        let resources: Vec<&ResourceRecord> = self
            .resources
            .iter()
            .filter(|r| sessions.iter().any(|s| s.key == r.session))
            .collect();
        let total = sessions.len() + resources.len();
        let start = cursor
            .and_then(|c| <[u8; 4]>::try_from(c).ok())
            .map_or(0, |c| u32::from_be_bytes(c) as usize);
        if start > total {
            return CommandResult::Refused(Refusal::InvalidArgument {
                field: "cursor".to_owned(),
                reason: "expired".to_owned(),
            });
        }
        let size = self.page_size.unwrap_or(if page_size == 0 {
            usize::MAX
        } else {
            page_size as usize
        });
        let end = start.saturating_add(size.max(1)).min(total);
        let mut page_sessions = Vec::new();
        let mut page_resources = Vec::new();
        for index in start..end {
            if index < sessions.len() {
                page_sessions.push(sessions[index].clone());
            } else {
                page_resources.push(resources[index - sessions.len()].clone());
            }
        }
        let intake = u32::try_from(sessions.iter().filter(|s| s.awaiting_organization).count())
            .unwrap_or(u32::MAX);
        let page = OrganizationPage::new(self.authority, Revision::new(self.revision), 0)
            .with_inbox(self.inbox)
            .with_sessions(page_sessions)
            .with_resources(page_resources)
            .with_totals(
                u32::try_from(sessions.len()).unwrap_or(u32::MAX),
                u32::try_from(resources.len()).unwrap_or(u32::MAX),
                intake,
            )
            .with_next_cursor(
                (end < total).then(|| u32::try_from(end).unwrap_or(0).to_be_bytes().to_vec()),
            );
        CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(page)))
    }

    /// An injected conflict: the record's revision moves and the caller's
    /// expectation no longer holds.
    pub(super) fn conflict(&mut self, record: RecordKey) -> Result<(), Refusal> {
        if self.conflicts == 0 {
            return Ok(());
        }
        self.conflicts -= 1;
        let revision = self.bump();
        match record {
            RecordKey::Session(key) => {
                if let Some(s) = self.session_mut(&key) {
                    s.revision = revision;
                }
            }
            RecordKey::Resource(key) => {
                if let Some(r) = self.resource_mut(&key) {
                    r.revision = revision;
                }
            }
            RecordKey::Arrangement(key) => {
                if let Some(a) = self.arrangements.iter_mut().find(|a| a.key == key) {
                    a.revision = revision;
                }
            }
        }
        Err(Refusal::Conflict { current: revision })
    }

    pub(super) fn check_session(
        &mut self,
        key: SessionKey,
        expected: Revision,
    ) -> Result<(), Refusal> {
        let current = self.session_ref(&key).ok_or(Refusal::NotFound)?.revision;
        self.conflict(RecordKey::Session(key))?;
        if current == expected {
            Ok(())
        } else {
            Err(Refusal::Conflict { current })
        }
    }

    pub(super) fn check_resource(
        &mut self,
        key: ResourceKey,
        expected: Revision,
    ) -> Result<(), Refusal> {
        let current = self.resource_ref(&key).ok_or(Refusal::NotFound)?.revision;
        self.conflict(RecordKey::Resource(key))?;
        if current == expected {
            Ok(())
        } else {
            Err(Refusal::Conflict { current })
        }
    }

    pub(super) fn require_inactive(&self, key: &ResourceKey) -> Result<(), Refusal> {
        let state = self
            .resource_ref(key)
            .and_then(|r| r.activation.map(|a| a.state));
        match state {
            Some(ExecState::Lost) => Err(Refusal::ExecutionUncertain),
            s if !phux_client_core::org_ops::is_inactive(s) => Err(Refusal::InvalidArgument {
                field: "resource".to_owned(),
                reason: "execution is current; terminate it first".to_owned(),
            }),
            _ => Ok(()),
        }
    }

    pub(super) fn pins(&self, sessions: &[SessionKey]) -> Vec<RecordRevision> {
        let mut pins: Vec<RecordRevision> = Vec::new();
        for session in sessions {
            if let Some(pin) = self.pin_session(session)
                && !pins.iter().any(|p| p.key == pin.key)
            {
                pins.push(pin);
            }
        }
        pins
    }

    pub(super) fn move_one(&mut self, resource: &ResourceKey, to: SessionKey) -> Revision {
        let from = self.resource_ref(resource).map(|r| r.session);
        if let Some(from) = from {
            self.prune_arrangement(&from, resource);
        }
        let revision = self.bump();
        if let Some(record) = self.resource_mut(resource) {
            record.session = to;
            record.revision = revision;
        }
        if let Some(from) = from {
            self.touch_session(&from);
        }
        self.touch_session(&to);
        revision
    }

    pub(super) fn terminate_target(&self, target: &LiveTarget) -> Result<ResourceKey, Refusal> {
        let record = self
            .resources
            .iter()
            .find(|r| r.live.as_ref() == Some(&target.resource))
            .ok_or(Refusal::NotFound)?;
        let current = record.activation.map(|a| a.id);
        if current != Some(target.activation) {
            return Err(Refusal::Stale { current });
        }
        Ok(record.key)
    }

    /// End `resource`'s activation with `outcome`, as its process exiting
    /// would: an exit policy of Remove drops the record.
    pub fn end_activation(&mut self, resource: &ResourceKey, outcome: Outcome) {
        let Some(record) = self.resource_ref(resource) else {
            return;
        };
        if record.on_exit == ExitPolicy::Remove {
            self.drop_resource(resource);
            return;
        }
        let revision = self.bump();
        if let Some(record) = self.resource_mut(resource) {
            if let Some(activation) = record.activation.as_mut() {
                activation.state = ExecState::Completed;
                activation.outcome = Some(outcome);
            }
            record.live = None;
            record.revision = revision;
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum RecordKey {
    Session(SessionKey),
    Resource(ResourceKey),
    Arrangement(ArrangementKey),
}

/// The lower-snake-case name [`OrgFixture::refuse_next`] keys on.
const fn command_name(command: &Command) -> &'static str {
    match command {
        Command::SessionCreate { .. } => "session_create",
        Command::SessionRename { .. } => "session_rename",
        Command::SessionSetProject { .. } => "session_set_project",
        Command::SessionSetLifetime { .. } => "session_set_lifetime",
        Command::SessionDelete { .. } => "session_delete",
        Command::SessionKill { .. } => "session_kill",
        Command::ResourceCreate { .. } => "resource_create",
        Command::ResourceMove { .. } => "resource_move",
        Command::ResourceMoveMany { .. } => "resource_move_many",
        Command::ResourceRename { .. } => "resource_rename",
        Command::ResourceTerminate { .. } => "resource_terminate",
        Command::ResourceRemove { .. } => "resource_remove",
        Command::ResourceRelaunch { .. } => "resource_relaunch",
        Command::InboxResolve { .. } => "inbox_resolve",
        Command::OrganizationUndo { .. } => "organization_undo",
        Command::ArrangementReplace { .. } => "arrangement_replace",
        _ => "other",
    }
}
