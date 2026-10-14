//! [`Org`]: one authority's organization records, and the [`Transition`]
//! that [`crate::apply::apply`] produces against it.
//!
//! The server owns persistence: it commits a transition to its store, then
//! calls [`Org::commit`] (through [`crate::Registry::commit`]) to mirror it
//! in memory, then publishes the transition's events.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::activation::{ExecState, Outcome, SessionLifetime, Stamp};
use crate::actor::Actor;
use crate::arrangement::{Arrangement, ArrangementOwner, Group};
use crate::command::{Op, Reply};
use crate::keys::{
    ActivationId, ArrangementKey, AuthorityId, FreshKeys, InstanceId, OperationId, ResourceKey,
    Revision, SessionKey,
};
use crate::resource::{Resource, ResourceKind};
use crate::session::{Attrs, Session};

/// Name the designated Intake session gets when [`Org::new`] creates it.
pub const INTAKE_NAME: &str = "intake";

/// Legacy public spelling of [`INTAKE_NAME`].
pub const INBOX_NAME: &str = INTAKE_NAME;

/// The persistent identity of this server and its designated Intake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authority {
    /// Never changes.
    pub id: AuthorityId,
    /// The persistent session unassigned work enters. It cannot be deleted
    /// or made ephemeral while designated.
    /// The legacy field name is retained for serialized/storage compatibility.
    pub inbox_session: SessionKey,
}

impl Authority {
    /// The persistent identity of the designated Intake session.
    #[must_use]
    pub const fn intake_session(&self) -> SessionKey {
        self.inbox_session
    }
}

/// A record a revision belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordRef {
    /// A session record.
    Session(SessionKey),
    /// A resource record.
    Resource(ResourceKey),
    /// An arrangement record.
    Arrangement(ArrangementKey),
}

/// A record and the revision it must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RevisionPin {
    /// Which record.
    pub record: RecordRef,
    /// Its revision.
    pub revision: Revision,
}

/// One reversible organization edit, as stored in a receipt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InverseEdit {
    /// Put `resource` back into session `to`.
    Membership {
        /// The resource.
        resource: ResourceKey,
        /// The session it returns to.
        to: SessionKey,
    },
    /// Restore a session's intake marker.
    Intake {
        /// The session.
        session: SessionKey,
        /// The marker's value to restore.
        awaiting_organization: bool,
    },
    /// Put an arrangement's document back as it was before a move pruned
    /// it. Applied after the memberships, so the restored entries name
    /// members again.
    Arrangement {
        /// The arrangement.
        key: ArrangementKey,
        /// Its groups before the move.
        groups: Vec<Group>,
    },
}

/// What undoing an operation does, and the state it requires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Undo {
    /// Edits that reverse the operation, applied in order.
    pub inverse: Vec<InverseEdit>,
    /// Revisions the operation left; undo applies only while every one is
    /// still current.
    pub pins: Vec<RevisionPin>,
}

/// The stored result of a committed operation, for retries and undo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    /// The operation.
    pub operation: OperationId,
    /// Who issued it.
    pub actor: Actor,
    /// Its payload, compared on retry.
    pub op: Op,
    /// When it committed.
    pub at: Stamp,
    /// The reply a retry gets.
    pub reply: Reply,
    /// The inverse, for supported organization edits.
    #[serde(default)]
    pub undo: Option<Undo>,
}

/// A record write, as the store applies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delta {
    /// Insert or replace a session.
    PutSession(Session),
    /// Delete a session.
    DeleteSession(SessionKey),
    /// Insert or replace a resource.
    PutResource(Resource),
    /// Remove a resource; the store writes a tombstone.
    RemoveResource(ResourceKey),
    /// Insert or replace an arrangement.
    PutArrangement(Arrangement),
    /// Delete an arrangement.
    DeleteArrangement(ArrangementKey),
}

/// An organization event for the journal (CONTRACTS §07).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// A session was created.
    SessionCreated {
        /// Key.
        key: SessionKey,
        /// Name.
        name: String,
        /// New revision.
        revision: Revision,
    },
    /// A session was renamed.
    SessionRenamed {
        /// Key.
        key: SessionKey,
        /// New name.
        name: String,
        /// New revision.
        revision: Revision,
    },
    /// A session's lifetime changed.
    SessionLifetimeChanged {
        /// Key.
        key: SessionKey,
        /// New lifetime.
        lifetime: SessionLifetime,
        /// New revision.
        revision: Revision,
    },
    /// A session was deleted.
    SessionDeleted {
        /// Key.
        key: SessionKey,
        /// Its last name.
        name: String,
        /// Its last revision.
        revision: Revision,
    },
    /// A session's intake marker changed.
    SessionIntakeChanged {
        /// Key.
        session: SessionKey,
        /// New value.
        awaiting_organization: bool,
        /// New revision.
        revision: Revision,
    },
    /// A resource moved between sessions.
    MembershipChanged {
        /// The resource.
        resource: ResourceKey,
        /// Source session.
        from: SessionKey,
        /// Destination session.
        to: SessionKey,
        /// The resource's new revision.
        resource_revision: Revision,
        /// The source's new revision; `None` when the move deleted it.
        from_revision: Option<Revision>,
        /// The destination's new revision.
        to_revision: Revision,
    },
    /// A resource was created.
    ResourceCreated {
        /// Key.
        key: ResourceKey,
        /// Its session.
        session: SessionKey,
        /// Kind.
        kind: ResourceKind,
    },
    /// A resource was renamed.
    ResourceRenamed {
        /// Key.
        key: ResourceKey,
        /// New label.
        name: Option<String>,
        /// New revision.
        revision: Revision,
    },
    /// A resource was removed.
    ResourceRemoved {
        /// Key.
        key: ResourceKey,
        /// The session it left.
        session: SessionKey,
        /// Kind.
        kind: ResourceKind,
    },
    /// An activation changed state.
    ActivationChanged {
        /// The resource.
        resource: ResourceKey,
        /// The activation.
        activation: ActivationId,
        /// New state.
        state: ExecState,
        /// Outcome, once ended.
        outcome: Option<Outcome>,
    },
    /// An arrangement was created, replaced, pruned, or deleted.
    ArrangementChanged {
        /// Key.
        key: ArrangementKey,
        /// Owner.
        owner: ArrangementOwner,
        /// New revision; `None` when deleted.
        revision: Option<Revision>,
    },
}

/// Everything one command commits, in one owner transaction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transition {
    /// Record writes, in order.
    pub deltas: Vec<Delta>,
    /// Events to publish after the commit, in order.
    pub events: Vec<Event>,
    /// The receipt to store; `None` for executor reports and replays.
    pub receipt: Option<Receipt>,
}

impl Transition {
    /// Whether nothing is written.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.deltas.is_empty() && self.receipt.is_none()
    }
}

/// A broken organization invariant.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("organization invariant violated: {0}")]
pub struct InvariantViolation(pub String);

/// One authority's organization records: an IO-free snapshot that
/// [`crate::apply::apply`] reads and [`Org::commit`] updates.
#[derive(Debug, Clone, PartialEq)]
pub struct Org {
    authority: Authority,
    instance: InstanceId,
    sessions: BTreeMap<SessionKey, Session>,
    resources: BTreeMap<ResourceKey, Resource>,
    arrangements: BTreeMap<ArrangementKey, Arrangement>,
    receipts: BTreeMap<OperationId, Receipt>,
    /// `receipts` ordered oldest first, so pruning touches only what it
    /// drops.
    receipts_by_age: BTreeSet<(Stamp, OperationId)>,
}

impl Org {
    /// A fresh organization: the authority and its empty, persistent Intake
    /// session named [`INTAKE_NAME`]. Existing stored names are preserved:
    /// records do not distinguish an old default `inbox` from a user rename.
    #[must_use]
    pub fn new(id: AuthorityId, intake: SessionKey, instance: InstanceId, now: Stamp) -> Self {
        let mut sessions = BTreeMap::new();
        sessions.insert(
            intake,
            Session {
                key: intake,
                name: INTAKE_NAME.to_owned(),
                lifetime: SessionLifetime::Persistent,
                awaiting_organization: false,
                members: Vec::new(),
                arrangement: None,
                attrs: Attrs::default(),
                created: now,
                revision: Revision::FIRST,
                recent: None,
            },
        );
        Self {
            authority: Authority {
                id,
                inbox_session: intake,
            },
            instance,
            sessions,
            resources: BTreeMap::new(),
            arrangements: BTreeMap::new(),
            receipts: BTreeMap::new(),
            receipts_by_age: BTreeSet::new(),
        }
    }

    /// Rebuild from stored records, checking every invariant.
    ///
    /// # Errors
    ///
    /// [`InvariantViolation`] when the records are inconsistent.
    pub fn restore(
        authority: Authority,
        instance: InstanceId,
        sessions: impl IntoIterator<Item = Session>,
        resources: impl IntoIterator<Item = Resource>,
        arrangements: impl IntoIterator<Item = Arrangement>,
        receipts: impl IntoIterator<Item = Receipt>,
    ) -> Result<Self, InvariantViolation> {
        let receipts: BTreeMap<OperationId, Receipt> =
            receipts.into_iter().map(|r| (r.operation, r)).collect();
        let org = Self {
            authority,
            instance,
            sessions: sessions.into_iter().map(|s| (s.key, s)).collect(),
            resources: resources.into_iter().map(|r| (r.key, r)).collect(),
            arrangements: arrangements.into_iter().map(|a| (a.key, a)).collect(),
            receipts_by_age: receipts.values().map(|r| (r.at, r.operation)).collect(),
            receipts,
        };
        org.check_invariants()?;
        Ok(org)
    }

    /// The authority record.
    #[must_use]
    pub const fn authority(&self) -> &Authority {
        &self.authority
    }

    /// The server run applying commands; stamped on new activations.
    #[must_use]
    pub const fn instance(&self) -> InstanceId {
        self.instance
    }

    /// Set the current server run, at startup.
    pub const fn set_instance(&mut self, instance: InstanceId) {
        self.instance = instance;
    }

    /// A session by key.
    #[must_use]
    pub fn session(&self, key: &SessionKey) -> Option<&Session> {
        self.sessions.get(key)
    }

    /// Every session, in key order.
    pub fn sessions(&self) -> impl Iterator<Item = &Session> + '_ {
        self.sessions.values()
    }

    /// The live session named `name`.
    #[must_use]
    pub fn session_by_name(&self, name: &str) -> Option<&Session> {
        self.sessions.values().find(|s| s.name == name)
    }

    /// A resource by key.
    #[must_use]
    pub fn resource(&self, key: &ResourceKey) -> Option<&Resource> {
        self.resources.get(key)
    }

    /// Every resource, in key order.
    pub fn resources(&self) -> impl Iterator<Item = &Resource> + '_ {
        self.resources.values()
    }

    /// An arrangement by key.
    #[must_use]
    pub fn arrangement(&self, key: &ArrangementKey) -> Option<&Arrangement> {
        self.arrangements.get(key)
    }

    /// Every arrangement, in key order.
    pub fn arrangements(&self) -> impl Iterator<Item = &Arrangement> + '_ {
        self.arrangements.values()
    }

    /// A stored receipt.
    #[must_use]
    pub fn receipt(&self, operation: &OperationId) -> Option<&Receipt> {
        self.receipts.get(operation)
    }

    /// Every stored receipt, in operation-id order.
    pub fn receipts(&self) -> impl Iterator<Item = &Receipt> + '_ {
        self.receipts.values()
    }

    /// The current revision of `record`, or `None` when it does not exist.
    #[must_use]
    pub fn revision_of(&self, record: RecordRef) -> Option<Revision> {
        match record {
            RecordRef::Session(k) => self.sessions.get(&k).map(|s| s.revision),
            RecordRef::Resource(k) => self.resources.get(&k).map(|r| r.revision),
            RecordRef::Arrangement(k) => self.arrangements.get(&k).map(|a| a.revision),
        }
    }

    /// Give every session that has none an empty arrangement at revision 1,
    /// with keys from `fresh`: a first start's Intake and an imported tree,
    /// before the records reach a store. A session [`crate::apply()`] creates
    /// already has one.
    pub fn ensure_arrangements(&mut self, fresh: &mut dyn FreshKeys) {
        let unarranged: Vec<SessionKey> = self
            .sessions
            .values()
            .filter(|s| s.arrangement.is_none())
            .map(|s| s.key)
            .collect();
        for session in unarranged {
            let key = ArrangementKey::from_bytes(fresh.fresh());
            self.arrangements.insert(
                key,
                Arrangement {
                    key,
                    owner: ArrangementOwner::Session(session),
                    groups: Vec::new(),
                    revision: Revision::FIRST,
                },
            );
            if let Some(s) = self.sessions.get_mut(&session) {
                s.arrangement = Some(key);
            }
        }
    }

    /// Mirror a committed transition. Deltas apply in order; the receipt is
    /// stored.
    pub fn commit(&mut self, transition: &Transition) {
        for delta in &transition.deltas {
            match delta {
                Delta::PutSession(s) => {
                    self.sessions.insert(s.key, s.clone());
                }
                Delta::DeleteSession(k) => {
                    self.sessions.remove(k);
                }
                Delta::PutResource(r) => {
                    self.resources.insert(r.key, r.clone());
                }
                Delta::RemoveResource(k) => {
                    self.resources.remove(k);
                }
                Delta::PutArrangement(a) => {
                    self.arrangements.insert(a.key, a.clone());
                }
                Delta::DeleteArrangement(k) => {
                    self.arrangements.remove(k);
                }
            }
        }
        if let Some(receipt) = &transition.receipt {
            if let Some(old) = self.receipts.insert(receipt.operation, receipt.clone()) {
                self.receipts_by_age.remove(&(old.at, old.operation));
            }
            self.receipts_by_age.insert((receipt.at, receipt.operation));
        }
    }

    /// Set a session's advisory [`Session::recent`]. Unversioned. Returns
    /// `false` when the session is unknown or `resource` is not a member.
    pub fn set_recent(&mut self, session: &SessionKey, resource: Option<ResourceKey>) -> bool {
        let Some(s) = self.sessions.get_mut(session) else {
            return false;
        };
        if resource.is_some_and(|r| !s.members.contains(&r)) {
            return false;
        }
        s.recent = resource;
        true
    }

    /// Drop receipts older than `before`, then the oldest beyond
    /// `keep_at_most`. Returns the dropped operation ids.
    pub fn prune_receipts(&mut self, before: Stamp, keep_at_most: usize) -> Vec<OperationId> {
        let mut dropped = Vec::new();
        while let Some(&(at, op)) = self.receipts_by_age.first() {
            if at >= before && self.receipts_by_age.len() <= keep_at_most {
                break;
            }
            self.receipts_by_age.pop_first();
            self.receipts.remove(&op);
            dropped.push(op);
        }
        dropped
    }

    /// Check every organization invariant.
    ///
    /// # Errors
    ///
    /// The first [`InvariantViolation`] found.
    pub fn check_invariants(&self) -> Result<(), InvariantViolation> {
        let fail = |msg: String| Err(InvariantViolation(msg));
        let intake = self.authority.intake_session();
        match self.sessions.get(&intake) {
            None => return fail(format!("intake session {intake} missing")),
            Some(s) if s.lifetime != SessionLifetime::Persistent => {
                return fail("intake session is ephemeral".to_owned());
            }
            Some(_) => {}
        }
        let mut names = HashSet::new();
        for (key, s) in &self.sessions {
            self.check_session(*key, s)?;
            if !names.insert(s.name.as_str()) {
                return fail(format!("session name {:?} not unique", s.name));
            }
        }
        for (key, r) in &self.resources {
            if r.key != *key {
                return fail(format!("resource {key} indexed under the wrong key"));
            }
            let Some(s) = self.sessions.get(&r.session) else {
                return fail(format!(
                    "resource {key} names missing session {}",
                    r.session
                ));
            };
            if !s.members.contains(key) {
                return fail(format!("resource {key} missing from its session's members"));
            }
            if r.revision == Revision::ZERO {
                return fail(format!("resource {key} has revision zero"));
            }
        }
        for (key, a) in &self.arrangements {
            let ArrangementOwner::Session(owner) = a.owner else {
                return fail(format!("arrangement {key} has a workspace owner"));
            };
            if self.sessions.get(&owner).and_then(|s| s.arrangement) != Some(*key) {
                return fail(format!("arrangement {key} not linked from its owner"));
            }
        }
        Ok(())
    }

    fn check_session(&self, key: SessionKey, s: &Session) -> Result<(), InvariantViolation> {
        let fail = |msg: String| Err(InvariantViolation(msg));
        if s.key != key {
            return fail(format!("session {key} indexed under the wrong key"));
        }
        if s.revision == Revision::ZERO {
            return fail(format!("session {key} has revision zero"));
        }
        if s.lifetime == SessionLifetime::Ephemeral && s.members.is_empty() {
            return fail(format!("ephemeral session {key} is empty"));
        }
        let mut seen = HashSet::new();
        for m in &s.members {
            if !seen.insert(*m) {
                return fail(format!("session {key} lists {m} twice"));
            }
            if self.resources.get(m).map(|r| r.session) != Some(key) {
                return fail(format!("session {key} member {m} does not point back"));
            }
        }
        if let Some(r) = s.recent
            && !s.members.contains(&r)
        {
            return fail(format!("session {key} recent {r} is not a member"));
        }
        if let Some(ak) = s.arrangement {
            let Some(a) = self.arrangements.get(&ak) else {
                return fail(format!("session {key} arrangement {ak} missing"));
            };
            if a.owner != ArrangementOwner::Session(key) {
                return fail(format!("arrangement {ak} owned by another record"));
            }
            if let Err(e) = a.validate(&s.members) {
                return fail(format!("arrangement {ak} invalid: {e}"));
            }
        }
        Ok(())
    }
}
