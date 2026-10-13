//! [`apply()`]: the one pure function that turns a [`Command`] against an
//! [`Org`] into a [`Transition`] plus [`Effect`]s, or a [`Refusal`].
//!
//! Invariants every successful transition preserves:
//!
//! * every resource is a member of exactly one session, and every member
//!   points back at its session;
//! * no ephemeral session is empty after commit: removing or moving out its
//!   last member deletes it in the same transition;
//! * the designated Inbox session is never ephemeral and never deleted;
//! * a session arrangement names only that session's members: entries for
//!   a resource that leaves are pruned in the same transition;
//! * every record touched by a transition bumps its revision exactly once.
//!
//! A refusal changes nothing. Retries are deduplicated by operation id
//! before any other check.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::activation::{
    Activation, ExecState, ExitPolicy, Outcome, ProcIdentity, SessionLifetime, Stamp,
};
use crate::actor::{Actor, PrincipalId, Verb};
use crate::arrangement::{Arrangement, ArrangementOwner, Group, first_group, validate_groups};
use crate::command::{
    Applied, Command, Created, Effect, LaunchMode, MAX_MOVE_BATCH, NewResource, Op, Refusal, Reply,
    SourcePin, TerminateMode,
};
use crate::keys::{
    ActivationId, ArrangementKey, FreshKeys, OperationId, ResourceKey, Revision, SessionKey,
};
use crate::org::{
    Delta, Event, InverseEdit, Org, Receipt, RecordRef, RevisionPin, Transition, Undo,
};
use crate::resource::{Provenance, Resource, ResourceKind};
use crate::session::{Attrs, MAX_NAME_BYTES, Session};

/// Apply `cmd`, issued by `actor` at `now`, to `org`.
///
/// Pure: `org` is not modified. On success the caller commits
/// [`Applied::transition`] durably, mirrors it with [`Org::commit`], then
/// runs [`Applied::effects`] and publishes the events. New keys come from
/// `fresh`.
///
/// # Errors
///
/// A [`Refusal`]; nothing is to be committed.
pub fn apply(
    org: &Org,
    cmd: &Command,
    actor: &Actor,
    now: Stamp,
    fresh: &mut dyn FreshKeys,
) -> Result<Applied, Refusal> {
    if let Some(receipt) = org.receipt(&cmd.operation) {
        return if receipt.op == cmd.op {
            Ok(Applied {
                operation: cmd.operation,
                transition: Transition::default(),
                effects: Vec::new(),
                reply: receipt.reply,
                replayed: true,
            })
        } else {
            Err(Refusal::DuplicateOperation)
        };
    }
    if cmd.op.is_report() && actor.principal != PrincipalId::System {
        return Err(Refusal::Unauthorized { verb: Verb::Create });
    }
    let mut tx = Tx::new(org, actor, cmd.operation, now);
    let Out { reply, undo } = dispatch(&mut tx, &cmd.op, fresh)?;
    let (deltas, events, effects) = tx.finish();
    let receipt = (!cmd.op.is_report()).then(|| Receipt {
        operation: cmd.operation,
        actor: actor.clone(),
        op: cmd.op.clone(),
        at: now,
        reply,
        undo,
    });
    Ok(Applied {
        operation: cmd.operation,
        transition: Transition {
            deltas,
            events,
            receipt,
        },
        effects,
        reply,
        replayed: false,
    })
}

struct Out {
    reply: Reply,
    undo: Option<Undo>,
}

impl Out {
    const fn ok(revision: Option<Revision>) -> Self {
        Self {
            reply: Reply::Ok {
                revision,
                created: Created {
                    session: None,
                    resource: None,
                    arrangement: None,
                },
            },
            undo: None,
        }
    }

    const fn created(revision: Option<Revision>, created: Created) -> Self {
        Self {
            reply: Reply::Ok { revision, created },
            undo: None,
        }
    }

    const fn accepted() -> Self {
        Self {
            reply: Reply::Accepted {
                created: Created {
                    session: None,
                    resource: None,
                    arrangement: None,
                },
            },
            undo: None,
        }
    }

    fn with_undo(mut self, undo: Undo) -> Self {
        self.undo = Some(undo);
        self
    }
}

fn dispatch(tx: &mut Tx<'_>, op: &Op, fresh: &mut dyn FreshKeys) -> Result<Out, Refusal> {
    match op {
        Op::SessionCreate {
            name,
            lifetime,
            attrs,
            resource,
        } => session_create(tx, fresh, name, *lifetime, attrs, resource.as_ref()),
        Op::SessionRename {
            session,
            expected,
            name,
        } => session_rename(tx, *session, *expected, name),
        Op::SessionSetLifetime {
            session,
            expected,
            lifetime,
        } => session_set_lifetime(tx, *session, *expected, *lifetime),
        Op::SessionDelete { session } => session_delete(tx, *session),
        Op::ResourceCreate { session, resource } => resource_create(tx, fresh, *session, resource),
        Op::ResourceMove {
            resource,
            from,
            from_expected,
            to,
            to_expected,
        } => resource_move(tx, *resource, (*from, *from_expected), (*to, *to_expected)),
        Op::ResourceMoveMany {
            resources,
            sources,
            to,
            to_expected,
        } => resource_move_many(tx, resources, sources, *to, *to_expected),
        Op::InboxResolve { session, expected } => inbox_resolve(tx, *session, *expected),
        Op::OrganizationUndo { original, expected } => organization_undo(tx, *original, expected),
        Op::ResourceRename {
            resource,
            expected,
            name,
        } => resource_rename(tx, *resource, *expected, name.as_deref()),
        Op::ResourceTerminate {
            resource,
            activation,
            mode,
        } => resource_terminate(tx, *resource, *activation, *mode),
        Op::ResourceRemove { resource } => resource_remove(tx, *resource),
        Op::ResourceRelaunch { resource, expected } => resource_relaunch(tx, *resource, *expected),
        Op::ArrangementReplace {
            owner,
            expected,
            groups,
        } => arrangement_replace(tx, fresh, *owner, *expected, groups),
        Op::ActivationStarted {
            resource,
            activation,
            proc,
        } => activation_started(tx, *resource, *activation, *proc),
        Op::ActivationEnded {
            resource,
            activation,
            state,
            outcome,
        } => activation_ended(tx, *resource, *activation, *state, *outcome),
        Op::ActivationLost {
            resource,
            activation,
        } => activation_lost(tx, *resource, *activation),
    }
}

// ---- the working transaction ---------------------------------------------

/// Copy-on-write view over an [`Org`]. The first mutable touch of an
/// existing record clones it and bumps its revision; later touches in the
/// same transaction do not bump again. `None` in a map marks a deletion.
struct Tx<'a> {
    org: &'a Org,
    actor: &'a Actor,
    operation: OperationId,
    now: Stamp,
    sessions: BTreeMap<SessionKey, Option<Session>>,
    resources: BTreeMap<ResourceKey, Option<Resource>>,
    arrangements: BTreeMap<ArrangementKey, Option<Arrangement>>,
    events: Vec<Event>,
    effects: Vec<Effect>,
}

impl<'a> Tx<'a> {
    const fn new(org: &'a Org, actor: &'a Actor, operation: OperationId, now: Stamp) -> Self {
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

    fn session(&self, key: SessionKey) -> Option<&Session> {
        self.sessions
            .get(&key)
            .map_or_else(|| self.org.session(&key), Option::as_ref)
    }

    fn session_mut(&mut self, key: SessionKey) -> Option<&mut Session> {
        if !self.sessions.contains_key(&key) {
            let mut s = self.org.session(&key)?.clone();
            s.revision = s.revision.next();
            self.sessions.insert(key, Some(s));
        }
        self.sessions.get_mut(&key).and_then(Option::as_mut)
    }

    fn resource(&self, key: ResourceKey) -> Option<&Resource> {
        self.resources
            .get(&key)
            .map_or_else(|| self.org.resource(&key), Option::as_ref)
    }

    fn resource_mut(&mut self, key: ResourceKey) -> Option<&mut Resource> {
        if !self.resources.contains_key(&key) {
            let mut r = self.org.resource(&key)?.clone();
            r.revision = r.revision.next();
            self.resources.insert(key, Some(r));
        }
        self.resources.get_mut(&key).and_then(Option::as_mut)
    }

    fn arrangement(&self, key: ArrangementKey) -> Option<&Arrangement> {
        self.arrangements
            .get(&key)
            .map_or_else(|| self.org.arrangement(&key), Option::as_ref)
    }

    fn arrangement_mut(&mut self, key: ArrangementKey) -> Option<&mut Arrangement> {
        if !self.arrangements.contains_key(&key) {
            let mut a = self.org.arrangement(&key)?.clone();
            a.revision = a.revision.next();
            self.arrangements.insert(key, Some(a));
        }
        self.arrangements.get_mut(&key).and_then(Option::as_mut)
    }

    fn revision_of(&self, record: RecordRef) -> Option<Revision> {
        match record {
            RecordRef::Session(k) => self.session(k).map(|s| s.revision),
            RecordRef::Resource(k) => self.resource(k).map(|r| r.revision),
            RecordRef::Arrangement(k) => self.arrangement(k).map(|a| a.revision),
        }
    }

    /// Pins for every record in `records` that still exists, deduplicated.
    fn pins(&self, records: impl IntoIterator<Item = RecordRef>) -> Vec<RevisionPin> {
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

    fn fresh_key(&self, fresh: &mut dyn FreshKeys) -> Result<[u8; 16], Refusal> {
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
    fn session_keys(&self) -> BTreeSet<SessionKey> {
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

    fn name_taken(&self, name: &str, except: Option<SessionKey>) -> bool {
        self.session_keys()
            .into_iter()
            .filter(|k| Some(*k) != except)
            .any(|k| self.session(k).is_some_and(|s| s.name == name))
    }

    /// A session that exists and is at `expected`.
    fn checked_session(&self, key: SessionKey, expected: Revision) -> Result<&Session, Refusal> {
        let s = self.session(key).ok_or(Refusal::NotFound)?;
        if s.revision != expected {
            return Err(Refusal::Conflict {
                current: s.revision,
            });
        }
        Ok(s)
    }

    /// A resource that exists and is at `expected`.
    fn checked_resource(&self, key: ResourceKey, expected: Revision) -> Result<&Resource, Refusal> {
        let r = self.resource(key).ok_or(Refusal::NotFound)?;
        if r.revision != expected {
            return Err(Refusal::Conflict {
                current: r.revision,
            });
        }
        Ok(r)
    }

    fn insert_session(&mut self, session: Session) {
        self.events.push(Event::SessionCreated {
            key: session.key,
            name: session.name.clone(),
            revision: session.revision,
        });
        self.sessions.insert(session.key, Some(session));
    }

    fn delete_session(&mut self, key: SessionKey) {
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
    fn drop_member(&mut self, session: SessionKey, resource: ResourceKey) -> bool {
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
    fn move_resource(
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
    fn remove_resource(&mut self, key: ResourceKey) {
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
    fn create_resource(
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

    fn validate_new_resource(&self, spec: &NewResource) -> Result<(), Refusal> {
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
    fn set_activation(
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
    fn arrange_new_session(
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
    fn finish(self) -> (Vec<Delta>, Vec<Event>, Vec<Effect>) {
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

fn check_name(field: &'static str, name: &str) -> Result<(), Refusal> {
    if name.trim().is_empty() {
        return Err(Refusal::invalid(field, "must not be empty"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(Refusal::invalid(
            field,
            format!("longer than {MAX_NAME_BYTES} bytes"),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(Refusal::invalid(field, "contains a control character"));
    }
    Ok(())
}

// ---- sessions --------------------------------------------------------------

fn session_create(
    tx: &mut Tx<'_>,
    fresh: &mut dyn FreshKeys,
    name: &str,
    lifetime: SessionLifetime,
    attrs: &Attrs,
    resource: Option<&NewResource>,
) -> Result<Out, Refusal> {
    check_name("name", name)?;
    if tx.name_taken(name, None) {
        return Err(Refusal::NameTaken);
    }
    if lifetime == SessionLifetime::Ephemeral && resource.is_none() {
        return Err(Refusal::invalid(
            "resource",
            "an ephemeral session needs a first resource",
        ));
    }
    let key = SessionKey::from_bytes(tx.fresh_key(fresh)?);
    tx.insert_session(Session {
        key,
        name: name.to_owned(),
        lifetime,
        awaiting_organization: false,
        members: Vec::new(),
        arrangement: None,
        attrs: attrs.clone(),
        created: tx.now,
        revision: Revision::FIRST,
        recent: None,
    });
    let resource = match resource {
        Some(spec) => Some(tx.create_resource(fresh, key, spec)?),
        None => None,
    };
    let arrangement = tx.arrange_new_session(fresh, key, resource.as_slice())?;
    Ok(Out::created(
        Some(Revision::FIRST),
        Created {
            session: Some(key),
            resource,
            arrangement: Some(arrangement),
        },
    ))
}

fn session_rename(
    tx: &mut Tx<'_>,
    key: SessionKey,
    expected: Revision,
    name: &str,
) -> Result<Out, Refusal> {
    let current = tx.checked_session(key, expected)?;
    check_name("name", name)?;
    if current.name == name {
        return Ok(Out::ok(Some(current.revision)));
    }
    if tx.name_taken(name, Some(key)) {
        return Err(Refusal::NameTaken);
    }
    let s = tx.session_mut(key).ok_or(Refusal::NotFound)?;
    name.clone_into(&mut s.name);
    let revision = s.revision;
    tx.events.push(Event::SessionRenamed {
        key,
        name: name.to_owned(),
        revision,
    });
    Ok(Out::ok(Some(revision)))
}

fn session_set_lifetime(
    tx: &mut Tx<'_>,
    key: SessionKey,
    expected: Revision,
    lifetime: SessionLifetime,
) -> Result<Out, Refusal> {
    let current = tx.checked_session(key, expected)?;
    if current.lifetime == lifetime {
        return Ok(Out::ok(Some(current.revision)));
    }
    if key == tx.org.authority().inbox_session && lifetime == SessionLifetime::Ephemeral {
        return Err(Refusal::invalid(
            "lifetime",
            "the designated inbox session is persistent",
        ));
    }
    let s = tx.session_mut(key).ok_or(Refusal::NotFound)?;
    s.lifetime = lifetime;
    let (revision, empty) = (s.revision, s.members.is_empty());
    tx.events.push(Event::SessionLifetimeChanged {
        key,
        lifetime,
        revision,
    });
    if lifetime == SessionLifetime::Ephemeral && empty {
        tx.delete_session(key);
        return Ok(Out::ok(None));
    }
    Ok(Out::ok(Some(revision)))
}

/// No revision check: emptiness is the guard, so a member added after the
/// caller looked refuses [`Refusal::NotEmpty`].
fn session_delete(tx: &mut Tx<'_>, key: SessionKey) -> Result<Out, Refusal> {
    let s = tx.session(key).ok_or(Refusal::NotFound)?;
    if key == tx.org.authority().inbox_session {
        return Err(Refusal::invalid(
            "session",
            "the designated inbox session cannot be deleted",
        ));
    }
    if !s.members.is_empty() {
        return Err(Refusal::NotEmpty {
            members: u32::try_from(s.members.len()).unwrap_or(u32::MAX),
        });
    }
    tx.delete_session(key);
    Ok(Out::ok(None))
}

fn inbox_resolve(tx: &mut Tx<'_>, key: SessionKey, expected: Revision) -> Result<Out, Refusal> {
    let current = tx.checked_session(key, expected)?;
    if !current.awaiting_organization {
        return Ok(Out::ok(Some(current.revision)));
    }
    let revision = set_intake(tx, key, false)?;
    let undo = Undo {
        inverse: vec![InverseEdit::Intake {
            session: key,
            awaiting_organization: true,
        }],
        pins: tx.pins([RecordRef::Session(key)]),
    };
    Ok(Out::ok(Some(revision)).with_undo(undo))
}

fn set_intake(tx: &mut Tx<'_>, key: SessionKey, awaiting: bool) -> Result<Revision, Refusal> {
    let s = tx.session_mut(key).ok_or(Refusal::NotFound)?;
    s.awaiting_organization = awaiting;
    let revision = s.revision;
    tx.events.push(Event::SessionIntakeChanged {
        session: key,
        awaiting_organization: awaiting,
        revision,
    });
    Ok(revision)
}

// ---- membership ------------------------------------------------------------

fn resource_create(
    tx: &mut Tx<'_>,
    fresh: &mut dyn FreshKeys,
    session: Option<SessionKey>,
    spec: &NewResource,
) -> Result<Out, Refusal> {
    let session = session.unwrap_or_else(|| tx.org.authority().inbox_session);
    if tx.session(session).is_none() {
        return Err(Refusal::NotFound);
    }
    let key = tx.create_resource(fresh, session, spec)?;
    Ok(Out::created(
        Some(Revision::FIRST),
        Created {
            session: None,
            resource: Some(key),
            arrangement: None,
        },
    ))
}

/// Move `moves` (resource, destination) and build the undo that puts each
/// back where it was, including the source arrangement a move prunes.
fn move_with_undo(
    tx: &mut Tx<'_>,
    moves: &[(ResourceKey, SessionKey)],
) -> Result<Option<Undo>, Refusal> {
    let mut inverse = Vec::new();
    let mut layouts = Vec::new();
    let mut touched = Vec::new();
    for &(resource, to) in moves {
        let from = tx.resource(resource).ok_or(Refusal::NotFound)?.session;
        if let Some(a) = tx
            .session(from)
            .and_then(|s| s.arrangement)
            .and_then(|key| tx.arrangement(key))
            .filter(|a| a.resources().contains(&resource))
            && !layouts
                .iter()
                .any(|e| matches!(e, InverseEdit::Arrangement { key, .. } if *key == a.key))
        {
            touched.push(RecordRef::Arrangement(a.key));
            layouts.push(InverseEdit::Arrangement {
                key: a.key,
                groups: a.groups.clone(),
            });
        }
        if tx.move_resource(resource, to)? != to {
            inverse.push(InverseEdit::Membership { resource, to: from });
            touched.extend([
                RecordRef::Resource(resource),
                RecordRef::Session(from),
                RecordRef::Session(to),
            ]);
        }
    }
    inverse.extend(layouts);
    Ok((!inverse.is_empty()).then(|| Undo {
        inverse,
        pins: tx.pins(touched),
    }))
}

fn resource_move(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    (from, from_expected): (SessionKey, Revision),
    (to, to_expected): (SessionKey, Revision),
) -> Result<Out, Refusal> {
    let current = tx.resource(resource).ok_or(Refusal::NotFound)?;
    if current.session != from {
        return Err(Refusal::Conflict {
            current: current.revision,
        });
    }
    tx.checked_session(from, from_expected)?;
    tx.checked_session(to, to_expected)?;
    let undo = move_with_undo(tx, &[(resource, to)])?;
    let revision = tx.resource(resource).map(|r| r.revision);
    let out = Out::ok(revision);
    Ok(match undo {
        Some(u) => out.with_undo(u),
        None => out,
    })
}

fn resource_move_many(
    tx: &mut Tx<'_>,
    resources: &[ResourceKey],
    sources: &[SourcePin],
    to: SessionKey,
    to_expected: Revision,
) -> Result<Out, Refusal> {
    if resources.is_empty() {
        return Err(Refusal::invalid("resources", "empty selection"));
    }
    if resources.len() > MAX_MOVE_BATCH {
        return Err(Refusal::invalid(
            "resources",
            format!("more than {MAX_MOVE_BATCH} resources"),
        ));
    }
    if resources.iter().collect::<HashSet<_>>().len() != resources.len() {
        return Err(Refusal::invalid("resources", "duplicate resource"));
    }
    tx.checked_session(to, to_expected)?;
    let mut pinned = HashSet::new();
    for pin in sources {
        tx.checked_session(pin.session, pin.expected)?;
        pinned.insert(pin.session);
    }
    for key in resources {
        let r = tx.resource(*key).ok_or(Refusal::NotFound)?;
        if r.session != to && !pinned.contains(&r.session) {
            return Err(Refusal::invalid(
                "sources",
                format!("source session {} of {key} is not pinned", r.session),
            ));
        }
    }
    let moves: Vec<(ResourceKey, SessionKey)> = resources.iter().map(|r| (*r, to)).collect();
    let undo = move_with_undo(tx, &moves)?;
    let revision = tx.session(to).map(|s| s.revision);
    let out = Out::ok(revision);
    Ok(match undo {
        Some(u) => out.with_undo(u),
        None => out,
    })
}

fn organization_undo(
    tx: &mut Tx<'_>,
    original: OperationId,
    expected: &[RevisionPin],
) -> Result<Out, Refusal> {
    let receipt = tx.org.receipt(&original).ok_or(Refusal::NotFound)?;
    let undo = receipt.undo.clone().ok_or_else(|| Refusal::Unsupported {
        capability: "undo".to_owned(),
    })?;
    for pin in undo.pins.iter().chain(expected) {
        let current = tx.revision_of(pin.record);
        if current != Some(pin.revision) {
            return Err(Refusal::Conflict {
                current: current.unwrap_or(Revision::ZERO),
            });
        }
    }
    let mut moves = Vec::new();
    let mut intake = Vec::new();
    let mut layouts = Vec::new();
    for edit in &undo.inverse {
        match edit {
            InverseEdit::Membership { resource, to } => {
                if tx.session(*to).is_none() {
                    return Err(Refusal::invalid(
                        "undo",
                        "the original session no longer exists",
                    ));
                }
                moves.push((*resource, *to));
            }
            InverseEdit::Intake {
                session,
                awaiting_organization,
            } => intake.push((*session, *awaiting_organization)),
            InverseEdit::Arrangement { key, groups } => layouts.push((*key, groups)),
        }
    }
    let mut redo = move_with_undo(tx, &moves)?.unwrap_or(Undo {
        inverse: Vec::new(),
        pins: Vec::new(),
    });
    let mut touched = Vec::new();
    for (session, awaiting) in intake {
        let prior = tx
            .session(session)
            .ok_or(Refusal::NotFound)?
            .awaiting_organization;
        set_intake(tx, session, awaiting)?;
        redo.inverse.push(InverseEdit::Intake {
            session,
            awaiting_organization: prior,
        });
        touched.push(RecordRef::Session(session));
    }
    for (key, groups) in layouts {
        let a = tx.arrangement_mut(key).ok_or(Refusal::NotFound)?;
        a.groups.clone_from(groups);
        let (owner, revision) = (a.owner, a.revision);
        tx.events.push(Event::ArrangementChanged {
            key,
            owner,
            revision: Some(revision),
        });
        touched.push(RecordRef::Arrangement(key));
    }
    let records: Vec<RecordRef> = redo.pins.iter().map(|p| p.record).chain(touched).collect();
    redo.pins = tx.pins(records);
    Ok(Out::ok(None).with_undo(redo))
}

// ---- resources -------------------------------------------------------------

fn resource_rename(
    tx: &mut Tx<'_>,
    key: ResourceKey,
    expected: Revision,
    name: Option<&str>,
) -> Result<Out, Refusal> {
    let current = tx.checked_resource(key, expected)?;
    if let Some(n) = name {
        check_name("name", n)?;
    }
    if current.name.as_deref() == name {
        return Ok(Out::ok(Some(current.revision)));
    }
    let r = tx.resource_mut(key).ok_or(Refusal::NotFound)?;
    r.name = name.map(str::to_owned);
    let revision = r.revision;
    tx.events.push(Event::ResourceRenamed {
        key,
        name: name.map(str::to_owned),
        revision,
    });
    Ok(Out::ok(Some(revision)))
}

/// The resource's activation when its id is `activation`; otherwise
/// [`Refusal::Stale`] naming the current one.
fn matching_activation(
    tx: &Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
) -> Result<Activation, Refusal> {
    let r = tx.resource(resource).ok_or(Refusal::NotFound)?;
    match &r.activation {
        Some(a) if a.id == activation => Ok(a.clone()),
        other => Err(Refusal::Stale {
            current: other.as_ref().map(|a| a.id),
        }),
    }
}

fn resource_terminate(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
    mode: TerminateMode,
) -> Result<Out, Refusal> {
    let a = matching_activation(tx, resource, activation)?;
    if !(a.state.is_current() || a.state == ExecState::Lost) {
        return Err(Refusal::Stale {
            current: Some(a.id),
        });
    }
    if matches!(a.state, ExecState::Starting | ExecState::Running) {
        tx.set_activation(resource, |a| a.state = ExecState::Stopping);
    }
    tx.effects.push(Effect::Terminate {
        resource,
        activation,
        mode,
        proc: a.host_proc,
    });
    Ok(Out::accepted())
}

/// Refuse while `resource`'s activation is current or lost.
fn require_inactive(r: &Resource) -> Result<(), Refusal> {
    match r.activation.as_ref().map(|a| a.state) {
        Some(ExecState::Lost) => Err(Refusal::ExecutionUncertain),
        Some(s) if s.is_current() => Err(Refusal::invalid(
            "resource",
            "execution is current; terminate it first",
        )),
        _ => Ok(()),
    }
}

/// No revision check: inactivity is the guard.
fn resource_remove(tx: &mut Tx<'_>, key: ResourceKey) -> Result<Out, Refusal> {
    require_inactive(tx.resource(key).ok_or(Refusal::NotFound)?)?;
    tx.remove_resource(key);
    Ok(Out::ok(None))
}

fn resource_relaunch(
    tx: &mut Tx<'_>,
    key: ResourceKey,
    expected: Revision,
) -> Result<Out, Refusal> {
    let r = tx.checked_resource(key, expected)?;
    require_inactive(r)?;
    let spec = r
        .launch
        .clone()
        .ok_or_else(|| Refusal::invalid("resource", "no launch spec to relaunch"))?;
    let id = r
        .activation
        .as_ref()
        .map_or(ActivationId::FIRST, |a| a.id.next());
    let (instance, now) = (tx.org.instance(), tx.now);
    let record = tx.resource_mut(key).ok_or(Refusal::NotFound)?;
    record.activation = Some(Activation::starting(id, instance, now));
    tx.events.push(Event::ActivationChanged {
        resource: key,
        activation: id,
        state: ExecState::Starting,
        outcome: None,
    });
    tx.effects.push(Effect::Launch {
        resource: key,
        activation: id,
        spec,
    });
    Ok(Out::accepted())
}

fn arrangement_replace(
    tx: &mut Tx<'_>,
    fresh: &mut dyn FreshKeys,
    owner: ArrangementOwner,
    expected: Revision,
    groups: &[Group],
) -> Result<Out, Refusal> {
    let ArrangementOwner::Session(session) = owner else {
        return Err(Refusal::Unsupported {
            capability: "workspace".to_owned(),
        });
    };
    let s = tx.session(session).ok_or(Refusal::NotFound)?;
    let existing = s.arrangement;
    let current = existing
        .and_then(|k| tx.arrangement(k))
        .map_or(Revision::ZERO, |a| a.revision);
    if current != expected {
        return Err(Refusal::Conflict { current });
    }
    validate_groups(groups, &s.members).map_err(|e| Refusal::invalid("groups", e.to_string()))?;
    let (key, revision, created) = if let Some(key) = existing {
        let a = tx.arrangement_mut(key).ok_or(Refusal::NotFound)?;
        a.groups = groups.to_vec();
        (key, a.revision, None)
    } else {
        let key = ArrangementKey::from_bytes(tx.fresh_key(fresh)?);
        tx.arrangements.insert(
            key,
            Some(Arrangement {
                key,
                owner,
                groups: groups.to_vec(),
                revision: Revision::FIRST,
            }),
        );
        tx.session_mut(session)
            .ok_or(Refusal::NotFound)?
            .arrangement = Some(key);
        (key, Revision::FIRST, Some(key))
    };
    tx.events.push(Event::ArrangementChanged {
        key,
        owner,
        revision: Some(revision),
    });
    Ok(Out::created(
        Some(revision),
        Created {
            session: None,
            resource: None,
            arrangement: created,
        },
    ))
}

// ---- executor reports ------------------------------------------------------

fn activation_started(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
    proc: Option<ProcIdentity>,
) -> Result<Out, Refusal> {
    let a = matching_activation(tx, resource, activation)?;
    if a.state != ExecState::Starting {
        return Err(Refusal::Stale {
            current: Some(a.id),
        });
    }
    let revision = tx.set_activation(resource, |a| {
        a.state = ExecState::Running;
        a.host_proc = proc;
    });
    Ok(Out::ok(revision))
}

/// An observed completion of a [`ExitPolicy::Remove`] resource removes the
/// record in the same transition. An unobserved one (a crash) keeps it.
fn activation_ended(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
    state: ExecState,
    outcome: Outcome,
) -> Result<Out, Refusal> {
    if !matches!(state, ExecState::Completed | ExecState::Failed) {
        return Err(Refusal::invalid(
            "state",
            "an ending is completed or failed",
        ));
    }
    let a = matching_activation(tx, resource, activation)?;
    if !(a.state.is_current() || a.state == ExecState::Lost) {
        return Err(Refusal::Stale {
            current: Some(a.id),
        });
    }
    let now = tx.now;
    let revision = tx.set_activation(resource, |a| {
        a.state = state;
        a.ended = Some(now);
        a.outcome = Some(outcome);
    });
    let remove = state == ExecState::Completed
        && outcome.is_observed()
        && tx
            .resource(resource)
            .is_some_and(|r| r.on_exit == ExitPolicy::Remove);
    if remove {
        tx.remove_resource(resource);
        return Ok(Out::ok(None));
    }
    Ok(Out::ok(revision))
}

fn activation_lost(
    tx: &mut Tx<'_>,
    resource: ResourceKey,
    activation: ActivationId,
) -> Result<Out, Refusal> {
    let a = matching_activation(tx, resource, activation)?;
    if !a.state.is_current() {
        return Err(Refusal::Stale {
            current: Some(a.id),
        });
    }
    let revision = tx.set_activation(resource, |a| a.state = ExecState::Lost);
    Ok(Out::ok(revision))
}
