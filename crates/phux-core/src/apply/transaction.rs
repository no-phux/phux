//! Transaction: extracted from the shared module.
use super::{
    Activation, ActivationId, Actor, Arrangement, ArrangementKey, ArrangementOwner, BTreeMap,
    BTreeSet, Delta, Effect, Event, ExecState, FreshKeys, HashSet, LaunchMode, NewResource,
    OperationId, Org, Provenance, RecordRef, Refusal, Resource, ResourceKey, ResourceKind,
    Revision, RevisionPin, Session, SessionKey, SessionLifetime, Stamp, check_name, first_group,
};

// ---- the working transaction ---------------------------------------------

/// Copy-on-write view over an [`Org`]. The first mutable touch of an
/// existing record clones it and bumps its revision; later touches in the
/// same transaction do not bump again. `None` in a map marks a deletion.
pub(super) struct Tx<'a> {
    pub(super) org: &'a Org,
    pub(super) actor: &'a Actor,
    pub(super) operation: OperationId,
    pub(super) now: Stamp,
    pub(super) sessions: BTreeMap<SessionKey, Option<Session>>,
    pub(super) resources: BTreeMap<ResourceKey, Option<Resource>>,
    pub(super) arrangements: BTreeMap<ArrangementKey, Option<Arrangement>>,
    pub(super) events: Vec<Event>,
    pub(super) effects: Vec<Effect>,
}

impl<'a> Tx<'a> {
    pub(super) const fn new(
        org: &'a Org,
        actor: &'a Actor,
        operation: OperationId,
        now: Stamp,
    ) -> Self {
        Self {
            org,
            actor,
            operation,
            now,
            sessions: BTreeMap::new(),
            resources: BTreeMap::new(),
            arrangements: BTreeMap::new(),
            events: Vec::new(),
            effects: Vec::new(),
        }
    }

    pub(super) fn session(&self, key: SessionKey) -> Option<&Session> {
        self.sessions
            .get(&key)
            .map_or_else(|| self.org.session(&key), Option::as_ref)
    }

    pub(super) fn session_mut(&mut self, key: SessionKey) -> Option<&mut Session> {
        if !self.sessions.contains_key(&key) {
            let mut s = self.org.session(&key)?.clone();
            s.revision = s.revision.next();
            self.sessions.insert(key, Some(s));
        }
        self.sessions.get_mut(&key).and_then(Option::as_mut)
    }

    pub(super) fn resource(&self, key: ResourceKey) -> Option<&Resource> {
        self.resources
            .get(&key)
            .map_or_else(|| self.org.resource(&key), Option::as_ref)
    }

    pub(super) fn resource_mut(&mut self, key: ResourceKey) -> Option<&mut Resource> {
        if !self.resources.contains_key(&key) {
            let mut r = self.org.resource(&key)?.clone();
            r.revision = r.revision.next();
            self.resources.insert(key, Some(r));
        }
        self.resources.get_mut(&key).and_then(Option::as_mut)
    }

    pub(super) fn arrangement(&self, key: ArrangementKey) -> Option<&Arrangement> {
        self.arrangements
            .get(&key)
            .map_or_else(|| self.org.arrangement(&key), Option::as_ref)
    }

    pub(super) fn arrangement_mut(&mut self, key: ArrangementKey) -> Option<&mut Arrangement> {
        if !self.arrangements.contains_key(&key) {
            let mut a = self.org.arrangement(&key)?.clone();
            a.revision = a.revision.next();
            self.arrangements.insert(key, Some(a));
        }
        self.arrangements.get_mut(&key).and_then(Option::as_mut)
    }

    pub(super) fn revision_of(&self, record: RecordRef) -> Option<Revision> {
        match record {
            RecordRef::Session(k) => self.session(k).map(|s| s.revision),
            RecordRef::Resource(k) => self.resource(k).map(|r| r.revision),
            RecordRef::Arrangement(k) => self.arrangement(k).map(|a| a.revision),
        }
    }

    /// Pins for every record in `records` that still exists, deduplicated.
    pub(super) fn pins(&self, records: impl IntoIterator<Item = RecordRef>) -> Vec<RevisionPin> {
        let mut seen = HashSet::new();
        records
            .into_iter()
            .filter(|r| seen.insert(*r))
            .filter_map(|record| {
                self.revision_of(record)
                    .map(|revision| RevisionPin { record, revision })
            })
            .collect()
    }

    pub(super) fn fresh_key(&self, fresh: &mut dyn FreshKeys) -> Result<[u8; 16], Refusal> {
        let bytes = fresh.fresh();
        let taken = self.session(SessionKey::from_bytes(bytes)).is_some()
            || self.resource(ResourceKey::from_bytes(bytes)).is_some()
            || self
                .arrangement(ArrangementKey::from_bytes(bytes))
                .is_some();
        if taken {
            return Err(Refusal::invalid("operation", "fresh key collides"));
        }
        Ok(bytes)
    }

    /// Every live session key in this transaction's view.
    pub(super) fn session_keys(&self) -> BTreeSet<SessionKey> {
        let mut keys: BTreeSet<SessionKey> = self.org.sessions().map(|s| s.key).collect();
        for (k, slot) in &self.sessions {
            if slot.is_some() {
                keys.insert(*k);
            } else {
                keys.remove(k);
            }
        }
        keys
    }

    pub(super) fn name_taken(&self, name: &str, except: Option<SessionKey>) -> bool {
        self.session_keys()
            .into_iter()
            .filter(|k| Some(*k) != except)
            .any(|k| self.session(k).is_some_and(|s| s.name == name))
    }

    /// A session that exists and is at `expected`.
    pub(super) fn checked_session(
        &self,
        key: SessionKey,
        expected: Revision,
    ) -> Result<&Session, Refusal> {
        let s = self.session(key).ok_or(Refusal::NotFound)?;
        if s.revision != expected {
            return Err(Refusal::Conflict {
                current: s.revision,
            });
        }
        Ok(s)
    }

    /// A resource that exists and is at `expected`.
    pub(super) fn checked_resource(
        &self,
        key: ResourceKey,
        expected: Revision,
    ) -> Result<&Resource, Refusal> {
        let r = self.resource(key).ok_or(Refusal::NotFound)?;
        if r.revision != expected {
            return Err(Refusal::Conflict {
                current: r.revision,
            });
        }
        Ok(r)
    }

    pub(super) fn insert_session(&mut self, session: Session) {
        self.events.push(Event::SessionCreated {
            key: session.key,
            name: session.name.clone(),
            revision: session.revision,
        });
        self.sessions.insert(session.key, Some(session));
    }

    pub(super) fn delete_session(&mut self, key: SessionKey) {
        let Some(session) = self.session(key).cloned() else {
            return;
        };
        if let Some(ak) = session.arrangement {
            self.arrangements.insert(ak, None);
            self.events.push(Event::ArrangementChanged {
                key: ak,
                owner: ArrangementOwner::Session(key),
                revision: None,
            });
        }
        self.sessions.insert(key, None);
        self.events.push(Event::SessionDeleted {
            key,
            name: session.name,
            revision: session.revision,
        });
    }

    /// Drop `resource` from `session`'s members, recent marker, and
    /// arrangement. Deletes an ephemeral session left empty; returns
    /// whether it did.
    pub(super) fn drop_member(&mut self, session: SessionKey, resource: ResourceKey) -> bool {
        let arrangement = self.session(session).and_then(|s| s.arrangement);
        if let Some(s) = self.session_mut(session) {
            s.members.retain(|m| *m != resource);
            if s.recent == Some(resource) {
                s.recent = None;
            }
        }
        if let Some(ak) = arrangement {
            let names = self
                .arrangement(ak)
                .is_some_and(|a| a.resources().contains(&resource));
            if names && let Some(a) = self.arrangement_mut(ak) {
                a.prune(resource);
                let (owner, revision) = (a.owner, a.revision);
                self.events.push(Event::ArrangementChanged {
                    key: ak,
                    owner,
                    revision: Some(revision),
                });
            }
        }
        let empty_ephemeral = self
            .session(session)
            .is_some_and(|s| s.lifetime == SessionLifetime::Ephemeral && s.members.is_empty());
        if empty_ephemeral {
            self.delete_session(session);
        }
        empty_ephemeral
    }

    /// Move `resource` into `to`. Membership only; a no-op when it is
    /// already there. Returns the session it left.
    pub(super) fn move_resource(
        &mut self,
        resource: ResourceKey,
        to: SessionKey,
    ) -> Result<SessionKey, Refusal> {
        let from = self.resource(resource).ok_or(Refusal::NotFound)?.session;
        if self.session(to).is_none() {
            return Err(Refusal::NotFound);
        }
        if from == to {
            return Ok(from);
        }
        let from_deleted = self.drop_member(from, resource);
        let to_revision = match self.session_mut(to) {
            Some(s) => {
                s.members.push(resource);
                s.revision
            }
            None => return Err(Refusal::NotFound),
        };
        let resource_revision = match self.resource_mut(resource) {
            Some(r) => {
                r.session = to;
                r.revision
            }
            None => return Err(Refusal::NotFound),
        };
        let from_revision = if from_deleted {
            None
        } else {
            self.session(from).map(|s| s.revision)
        };
        self.events.push(Event::MembershipChanged {
            resource,
            from,
            to,
            resource_revision,
            from_revision,
            to_revision,
        });
        Ok(from)
    }

    /// Remove a resource record and its membership.
    pub(super) fn remove_resource(&mut self, key: ResourceKey) {
        let Some(r) = self.resource(key).cloned() else {
            return;
        };
        self.resources.insert(key, None);
        self.events.push(Event::ResourceRemoved {
            key,
            session: r.session,
            kind: r.kind,
        });
        self.drop_member(r.session, key);
    }

    /// Create a resource in `session` and, when it has a launch spec that
    /// is not deferred, its first activation and launch effect. A deferred
    /// one stays dormant: no activation until [`Op::ResourceRelaunch`].
    pub(super) fn create_resource(
        &mut self,
        fresh: &mut dyn FreshKeys,
        session: SessionKey,
        spec: &NewResource,
    ) -> Result<ResourceKey, Refusal> {
        self.validate_new_resource(spec)?;
        let key = ResourceKey::from_bytes(self.fresh_key(fresh)?);
        let launch = match spec.launch_mode {
            LaunchMode::Immediate => spec.launch.as_ref(),
            LaunchMode::Deferred => None,
        };
        let activation = launch
            .map(|_| Activation::starting(ActivationId::FIRST, self.org.instance(), self.now));
        let record = Resource {
            key,
            session,
            name: spec.name.clone(),
            kind: spec.kind,
            provenance: Provenance {
                created_by: self.actor.clone(),
                operation: self.operation,
                parent: spec.parent,
            },
            launch: spec.launch.clone(),
            on_exit: spec.on_exit,
            durable_result: false,
            activation,
            revision: Revision::FIRST,
        };
        self.resources.insert(key, Some(record));
        self.session_mut(session)
            .ok_or(Refusal::NotFound)?
            .members
            .push(key);
        self.events.push(Event::ResourceCreated {
            key,
            session,
            kind: spec.kind,
        });
        if let Some(launch) = launch {
            self.events.push(Event::ActivationChanged {
                resource: key,
                activation: ActivationId::FIRST,
                state: ExecState::Starting,
                outcome: None,
            });
            self.effects.push(Effect::Launch {
                resource: key,
                activation: ActivationId::FIRST,
                spec: launch.clone(),
            });
        }
        Ok(key)
    }

    pub(super) fn validate_new_resource(&self, spec: &NewResource) -> Result<(), Refusal> {
        if let Some(name) = &spec.name {
            check_name("name", name)?;
        }
        match &spec.launch {
            Some(launch) if launch.kind() != spec.kind => {
                return Err(Refusal::invalid(
                    "launch",
                    "launch spec is for another kind",
                ));
            }
            None if spec.kind == ResourceKind::Terminal => {
                return Err(Refusal::invalid("launch", "a terminal needs a launch spec"));
            }
            None if spec.launch_mode == LaunchMode::Deferred => {
                return Err(Refusal::invalid(
                    "launch_mode",
                    "a deferred launch needs a launch spec",
                ));
            }
            _ => {}
        }
        let parent_kind = match spec.parent {
            Some(p) => Some(
                self.resource(p)
                    .ok_or_else(|| Refusal::invalid("parent", "unknown resource"))?
                    .kind,
            ),
            None => None,
        };
        if spec.kind == ResourceKind::AgentSession && parent_kind != Some(ResourceKind::Terminal) {
            return Err(Refusal::invalid(
                "parent",
                "an agent conversation binds to a terminal",
            ));
        }
        Ok(())
    }

    /// Set the current activation's state and emit the change.
    pub(super) fn set_activation(
        &mut self,
        resource: ResourceKey,
        update: impl FnOnce(&mut Activation),
    ) -> Option<Revision> {
        let r = self.resource_mut(resource)?;
        let a = r.activation.as_mut()?;
        update(a);
        let event = Event::ActivationChanged {
            resource,
            activation: a.id,
            state: a.state,
            outcome: a.outcome,
        };
        let revision = r.revision;
        self.events.push(event);
        Some(revision)
    }

    /// Give the session `session` this transaction creates its arrangement:
    /// one unnamed group showing `members` ([`first_group`]), or no groups.
    pub(super) fn arrange_new_session(
        &mut self,
        fresh: &mut dyn FreshKeys,
        session: SessionKey,
        members: &[ResourceKey],
    ) -> Result<ArrangementKey, Refusal> {
        let key = ArrangementKey::from_bytes(self.fresh_key(fresh)?);
        let owner = ArrangementOwner::Session(session);
        self.arrangements.insert(
            key,
            Some(Arrangement {
                key,
                owner,
                groups: first_group(members, fresh).into_iter().collect(),
                revision: Revision::FIRST,
            }),
        );
        self.session_mut(session)
            .ok_or(Refusal::NotFound)?
            .arrangement = Some(key);
        self.events.push(Event::ArrangementChanged {
            key,
            owner,
            revision: Some(Revision::FIRST),
        });
        Ok(key)
    }

    /// Records as deltas (puts, then deletions), events, and effects.
    pub(super) fn finish(self) -> (Vec<Delta>, Vec<Event>, Vec<Effect>) {
        let org = self.org;
        let mut puts = Vec::new();
        let mut deletes = Vec::new();
        for (k, slot) in self.sessions {
            match slot {
                Some(s) => puts.push(Delta::PutSession(s)),
                None if org.session(&k).is_some() => deletes.push(Delta::DeleteSession(k)),
                None => {}
            }
        }
        let mut resource_deletes = Vec::new();
        for (k, slot) in self.resources {
            match slot {
                Some(r) => puts.push(Delta::PutResource(r)),
                None if org.resource(&k).is_some() => {
                    resource_deletes.push(Delta::RemoveResource(k));
                }
                None => {}
            }
        }
        let mut arrangement_deletes = Vec::new();
        for (k, slot) in self.arrangements {
            match slot {
                Some(a) => puts.push(Delta::PutArrangement(a)),
                None if org.arrangement(&k).is_some() => {
                    arrangement_deletes.push(Delta::DeleteArrangement(k));
                }
                None => {}
            }
        }
        puts.extend(arrangement_deletes);
        puts.extend(resource_deletes);
        puts.extend(deletes);
        (puts, self.events, self.effects)
    }
}
