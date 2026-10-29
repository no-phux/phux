//! Applying organization journal events to the replica.

use super::{
    ActivationInfo, AgentEvent, ArrangementOwner, MAX_BUFFERED_EVENTS, OrganizationReplica,
    ReplicaAction, ReplicaState, ResourceRecord, Revision, SessionKey, SessionRecord, is_inactive,
    is_organization_event,
};

impl OrganizationReplica {
    /// Apply one journal event, stamped `seq` when the server journals
    /// events. Returns what the control plane must do.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per organization event, each a few record updates"
    )]
    pub fn apply_event(&mut self, seq: Option<u64>, event: &AgentEvent) -> ReplicaAction {
        if matches!(event, AgentEvent::JournalGap { .. }) {
            if self.state != ReplicaState::Empty {
                self.state = ReplicaState::Stale;
                return ReplicaAction::Reseed;
            }
            return ReplicaAction::None;
        }
        if !is_organization_event(event) {
            return ReplicaAction::None;
        }
        if self.seed.is_some() {
            if self.buffered.len() >= MAX_BUFFERED_EVENTS {
                self.buffered.clear();
                return ReplicaAction::Reseed;
            }
            self.buffered.push((seq, event.clone()));
            return ReplicaAction::None;
        }
        if self.state != ReplicaState::Live {
            return ReplicaAction::None;
        }
        if let Some(seq) = seq {
            if seq <= self.journal_seq {
                // Already part of the seed's cut.
                return ReplicaAction::None;
            }
            self.journal_seq = seq;
        }
        match event {
            AgentEvent::SessionCreated {
                session,
                name,
                lifetime,
                revision,
            } => {
                if self.session(session).is_none() {
                    self.sessions.push(
                        SessionRecord::new(*session, name.clone(), *revision)
                            .with_lifetime(*lifetime),
                    );
                }
                // The live handle arrives with the records.
                ReplicaAction::RefetchSession(*session)
            }
            AgentEvent::SessionRenamed {
                session,
                name,
                revision,
            } => {
                self.with_session(session, |s| {
                    s.name.clone_from(name);
                    s.revision = *revision;
                });
                ReplicaAction::Changed
            }
            AgentEvent::SessionProjectSet {
                session,
                project,
                revision,
            } => {
                self.with_session(session, |s| {
                    crate::project_identity::set_link_attr(&mut s.attrs, project.as_deref());
                    s.revision = *revision;
                });
                ReplicaAction::Changed
            }
            AgentEvent::SessionDeleted { session, .. } => {
                if let Some(arrangement) = self.session(session).and_then(|s| s.arrangement) {
                    self.arrangements.remove(&arrangement);
                }
                self.sessions.retain(|s| s.key != *session);
                self.forget_members(*session);
                ReplicaAction::Changed
            }
            AgentEvent::SessionLifetimeChanged {
                session,
                lifetime,
                revision,
            } => {
                self.with_session(session, |s| {
                    s.lifetime = *lifetime;
                    s.revision = *revision;
                });
                ReplicaAction::Changed
            }
            AgentEvent::IntakeChanged {
                session,
                awaiting_organization,
                revision,
            } => {
                self.with_session(session, |s| {
                    s.awaiting_organization = *awaiting_organization;
                    s.revision = *revision;
                });
                ReplicaAction::Changed
            }
            AgentEvent::MembershipChanged {
                resource,
                from,
                to,
                from_revision,
                to_revision,
            } => {
                if let Some(record) = self.resources.iter_mut().find(|r| r.key == *resource) {
                    record.session = *to;
                }
                self.with_session(from, |s| {
                    s.revision = *from_revision;
                    s.members = s.members.saturating_sub(1);
                });
                self.with_session(to, |s| {
                    s.revision = *to_revision;
                    s.members = s.members.saturating_add(1);
                });
                if self.resource(resource).is_none() {
                    return ReplicaAction::RefetchSession(*to);
                }
                ReplicaAction::Changed
            }
            AgentEvent::ResourceCreated {
                resource,
                session,
                kind,
            } => {
                if self.resource(resource).is_none() {
                    self.resources.push(
                        ResourceRecord::new(*resource, *session, Revision::default())
                            .with_kind(*kind),
                    );
                    self.with_session(session, |s| s.members = s.members.saturating_add(1));
                }
                // Its revision, launch, and live handle arrive with the
                // records.
                ReplicaAction::RefetchSession(*session)
            }
            AgentEvent::ResourceRemoved {
                resource, session, ..
            } => {
                let existed = self.resource(resource).is_some();
                self.resources.retain(|r| r.key != *resource);
                self.handles.remove_key(resource);
                if existed {
                    self.with_session(session, |s| s.members = s.members.saturating_sub(1));
                }
                ReplicaAction::Changed
            }
            AgentEvent::ActivationChanged {
                resource,
                activation,
                state,
                outcome,
            } => {
                let Some(record) = self.resources.iter_mut().find(|r| r.key == *resource) else {
                    return ReplicaAction::None;
                };
                let instance = record.activation.map(|a| a.instance);
                let fresh = record.activation.is_none_or(|a| a.id != *activation);
                let info = ActivationInfo::new(
                    *activation,
                    instance.unwrap_or_else(|| phux_protocol::ids::ServerInstance::new([0; 16])),
                    *state,
                )
                .with_outcome(*outcome);
                record.activation = Some(info);
                let session = record.session;
                // A new activation runs under a new live handle and maybe a
                // new server instance: read them.
                if (fresh && !is_inactive(Some(*state))) || instance.is_none() {
                    return ReplicaAction::RefetchSession(session);
                }
                ReplicaAction::Changed
            }
            AgentEvent::ResourceRenamed {
                resource,
                name,
                revision,
            } => {
                if let Some(record) = self.resources.iter_mut().find(|r| r.key == *resource) {
                    record.name.clone_from(name);
                    record.revision = *revision;
                }
                ReplicaAction::Changed
            }
            AgentEvent::ArrangementChanged {
                arrangement,
                owner,
                revision,
            } => {
                let ArrangementOwner::Session(owner) = *owner else {
                    return ReplicaAction::None;
                };
                match revision {
                    None => {
                        self.arrangements.remove(arrangement);
                        self.with_session(&owner, |s| {
                            if s.arrangement == Some(*arrangement) {
                                s.arrangement = None;
                            }
                        });
                        ReplicaAction::Changed
                    }
                    Some(revision) => {
                        self.with_session(&owner, |s| s.arrangement = Some(*arrangement));
                        let stale = self
                            .arrangements
                            .get(arrangement)
                            .is_some_and(|held| held.revision < *revision);
                        if stale {
                            ReplicaAction::RefetchArrangement(*arrangement)
                        } else {
                            ReplicaAction::Changed
                        }
                    }
                }
            }
            _ => ReplicaAction::None,
        }
    }

    /// Drop `session`'s resource records and their handles.
    pub(super) fn forget_members(&mut self, session: SessionKey) {
        for gone in self.resources.iter().filter(|r| r.session == session) {
            self.handles.remove_key(&gone.key);
        }
        self.resources.retain(|r| r.session != session);
    }

    fn with_session(&mut self, key: &SessionKey, edit: impl FnOnce(&mut SessionRecord)) {
        if let Some(session) = self.sessions.iter_mut().find(|s| s.key == *key) {
            edit(session);
        }
    }
}
