//! Authority over the `phux.agent/v1` record (ADR-0046 §E).
//!
//! An explicit `SET_METADATA` that supplies a `state` outranks the detector
//! until `DELETE`; one that supplies only identity is preserved field for
//! field while the detector fills `state` around it. The detector deletes
//! only records it wrote. The one exception (`docs/program-sequences.md#the-agent-record`): on
//! positive evidence that the declared occupant is gone, the server may
//! withdraw the declaration ([`withdraw_state`]: `state` becomes
//! `"unknown"`, identity preserved, never a `DELETE`).
//!
//! **I1.** Every detector write reasserts `kind`, `name`, and `state`
//! together, composed against the bytes read under the same lock, except
//! fields an explicit writer owns. The correction event is best-effort, so
//! only this reassertion self-heals.
//!
//! **I2.** `state: "unknown"` is the only state that may pair with a
//! possibly stale `kind`. Where an explicit writer owns `kind`, the server
//! cannot correct it, so [`explicit_kind_is_contradicted`] withholds state
//! instead.
//!
//! Declaration cannot be read back from the bytes (absent and `unknown`
//! decode alike), so it is tracked explicitly, fed only by the
//! `SET_METADATA` entry point, which the detector's drain never passes.

#![allow(
    clippy::redundant_pub_crate,
    reason = "items shared inside the crate; pub would widen its API"
)]

use std::collections::HashMap;

use phux_protocol::ids::ResourceId as WireResourceId;

use crate::agent_record::AgentRecordJson;

/// Where a claim about a Terminal's agent `state` came from.
///
/// Ordered by authority (ADR-0103 §5): closeness to the agent's own knowledge,
/// not freshness. A declaration ([`AgentRecordArbiter`]) outranks all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceSource {
    /// Derived from the grid by the rules (ADR-0046): the weakest source.
    Screen,
    /// `REPORT_AGENT_STATE` from an integration (ADR-0085): the agent
    /// speaking, but lossy and edge-triggered.
    Hook,
    /// A live `AgentSession` child's record stream (ADR-0103): the agent
    /// describing itself over a sequenced, gap-detecting channel.
    Stream,
}

impl EvidenceSource {
    /// Rank within the ladder: higher wins. Separate from the ask ladder
    /// ([`crate::asked::AskedSource::priority`]).
    #[must_use]
    pub(crate) const fn priority(self) -> u8 {
        match self {
            Self::Screen => 0,
            Self::Hook => 1,
            Self::Stream => 2,
        }
    }

    /// Whether `self` may publish over `incumbent`. Reflexive: re-asserting
    /// one's own claim is a refresh.
    #[must_use]
    pub const fn outranks(self, incumbent: Self) -> bool {
        self.priority() >= incumbent.priority()
    }
}

/// Who currently owns each Terminal's `phux.agent/v1` record.
#[derive(Debug, Default)]
pub struct AgentRecordArbiter {
    /// Each Terminal's marks; a Terminal with none set has no entry.
    marks: HashMap<WireResourceId, Marks>,
}

/// What the arbiter knows about one Terminal's stored record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "four independent marks on one record, each with its own writer"
)]
struct Marks {
    /// An explicit `SET_METADATA` wrote the record with a `state`; the
    /// detector stands down until `DELETE`.
    declared: bool,
    /// The detector wrote the current record (and may retract it).
    detector_owned: bool,
    /// The stored record carries human-authored identity (`name` /
    /// `session` / `attention`). The detector may own the `state` of such a
    /// record but must never `DELETE` it.
    explicit_identity: bool,
    /// An explicit writer set the stored `kind`, which the detector must
    /// preserve. Separate from `explicit_identity`, since a bare kind
    /// does not protect a record from deletion.
    explicit_kind: bool,
}

/// Which identity fields of a stored record an explicit writer owns; every
/// other field is reasserted on each detector write (I1).
#[derive(Debug, Clone, Copy)]
pub struct IdentityOwnership {
    /// An explicit writer supplied `name`; keep theirs.
    pub name: bool,
    /// An explicit writer supplied `kind`; keep theirs.
    pub kind: bool,
}

impl IdentityOwnership {
    /// Nothing owned: every field is the detector's.
    #[cfg(test)]
    pub(crate) const DETECTOR: Self = Self {
        name: false,
        kind: false,
    };
}

impl AgentRecordArbiter {
    /// This Terminal's marks; all clear when it has none.
    fn marks(&self, terminal: &WireResourceId) -> Marks {
        self.marks.get(terminal).copied().unwrap_or_default()
    }

    /// Change this Terminal's marks, dropping its entry once none is set.
    fn update(&mut self, terminal: &WireResourceId, change: impl FnOnce(&mut Marks)) {
        let mut marks = self.marks(terminal);
        change(&mut marks);
        if marks == Marks::default() {
            self.marks.remove(terminal);
        } else {
            self.marks.insert(terminal.clone(), marks);
        }
    }

    /// Note an explicit `SET_METADATA` on this Terminal's agent record.
    ///
    /// Supplying a real `state` makes it declared; identity-only writes
    /// leave the detector to fill in `state`. Either way the detector loses
    /// ownership. Marks track what is in the store (writes replace wholesale).
    pub fn note_explicit_set(&mut self, terminal: &WireResourceId, value: &[u8]) {
        let record = AgentRecordJson::decode(value);
        let record = record.as_ref();
        let marks = Marks {
            declared: record.is_some_and(|r| !r.state.is_empty() && r.state != "unknown"),
            detector_owned: false,
            // A kind alone is not identity a human would miss...
            explicit_identity: record.is_some_and(|r| {
                !r.name.is_empty() || r.session.is_some() || r.attention.is_some()
            }),
            // ...but an explicitly supplied kind is still preserved.
            explicit_kind: record.is_some_and(|r| r.kind.as_ref().is_some_and(|k| !k.is_empty())),
        };
        self.update(terminal, |stored| *stored = marks);
    }

    /// Note an explicit `DELETE_METADATA`: everything clears and the detector
    /// resumes full ownership.
    pub fn note_explicit_delete(&mut self, terminal: &WireResourceId) {
        self.marks.remove(terminal);
    }

    /// Withdraw a declaration whose subject is provably gone.
    ///
    /// Clears `declared` only: identity bookkeeping stays, and the detector
    /// does not gain ownership (its next write acquires it normally).
    pub fn note_declaration_withdrawn(&mut self, terminal: &WireResourceId) {
        self.update(terminal, |marks| marks.declared = false);
    }

    /// Whether the stored record carries human identity (withdraw, never
    /// `DELETE`).
    #[must_use]
    pub fn has_explicit_identity(&self, terminal: &WireResourceId) -> bool {
        self.marks(terminal).explicit_identity
    }

    /// Whether an explicit writer supplied the stored `kind` (preserve it).
    #[must_use]
    pub fn has_explicit_kind(&self, terminal: &WireResourceId) -> bool {
        self.marks(terminal).explicit_kind
    }

    /// The ownership bits for one Terminal, read together.
    #[must_use]
    pub fn identity_ownership(&self, terminal: &WireResourceId) -> IdentityOwnership {
        let marks = self.marks(terminal);
        IdentityOwnership {
            name: marks.explicit_identity,
            kind: marks.explicit_kind,
        }
    }

    /// Whether a human declared this Terminal's state (detector must not
    /// write).
    #[must_use]
    pub fn is_declared(&self, terminal: &WireResourceId) -> bool {
        self.marks(terminal).declared
    }

    /// Note that the detector authored this Terminal's current record.
    pub fn note_detector_write(&mut self, terminal: &WireResourceId) {
        self.update(terminal, |marks| marks.detector_owned = true);
    }

    /// Note that the detector retracted this Terminal's record.
    pub fn note_detector_retract(&mut self, terminal: &WireResourceId) {
        self.update(terminal, |marks| marks.detector_owned = false);
    }

    /// Whether the detector wrote the stored record (and may delete it).
    #[must_use]
    pub fn detector_owns(&self, terminal: &WireResourceId) -> bool {
        self.marks(terminal).detector_owned
    }

    /// Drop all bookkeeping for a reaped Terminal.
    pub fn forget(&mut self, terminal: &WireResourceId) {
        self.marks.remove(terminal);
    }
}

/// Compose the detector's record, preserving fields an explicit writer
/// owns.
///
/// `name` and `kind` come from the manifest unless `owned` (or an
/// identity-only stored record that beat the arbiter bit) says otherwise;
/// a blank stored value is always filled. `session` passes through; a
/// declared `attention` is preserved. Everything else is reasserted every
/// write (I1).
pub fn compose(
    existing: Option<&[u8]>,
    kind: &str,
    name: &str,
    state: &str,
    owned: IdentityOwnership,
) -> Vec<u8> {
    let prior = existing.and_then(AgentRecordJson::decode);
    let record = match prior {
        Some(mut prior) => {
            // An empty `state` marks an identity-only SET; the detector
            // always writes a real word.
            let identity_only = prior.state.is_empty();
            if prior.name.is_empty() || !(owned.name || identity_only) {
                prior.name.clear();
                prior.name.push_str(name);
            }
            let kind_present = prior.kind.as_ref().is_some_and(|stored| !stored.is_empty());
            if !kind_present || !(owned.kind || identity_only) {
                prior.kind = Some(kind.to_owned());
            }
            prior.state.clear();
            prior.state.push_str(state);
            prior
        }
        None => AgentRecordJson {
            name: name.to_owned(),
            kind: Some(kind.to_owned()),
            state: state.to_owned(),
            attention: None,
            session: None,
        },
    };
    record.encode()
}

/// Whether the explicit `kind` stored for a pane is contradicted by what
/// the detector sees (I2).
///
/// A shim declares `kind: claude`; after a switch to codex the preserved
/// kind would sit beside codex's derived state. The record contract forbids correcting the
/// kind but permits withdrawing state on positive evidence the occupant is
/// gone, so the server withholds state (the shape `%name` writes refuse).
/// Only a kind with a loaded manifest can be contradicted: an open slug
/// like `my-agent` asserts nothing checkable. Stateless, and the frozen
/// write dedups to zero broadcasts.
pub fn explicit_kind_is_contradicted(
    existing: Option<&[u8]>,
    detected: &str,
    rules: &phux_agent_rules::rules::RuleSet,
) -> bool {
    let Some(stored) = existing
        .and_then(AgentRecordJson::decode)
        .and_then(|record| record.kind)
        .filter(|kind| !kind.is_empty())
    else {
        return false;
    };
    if stored.eq_ignore_ascii_case(detected) {
        return false;
    }
    // Only a kind with a manifest is falsifiable; a case mismatch errs
    // toward leaving the writer alone.
    rules.manifest(&stored.to_lowercase()).is_some()
}

/// The stored `state`, if any, so the `agent-state-changed` hook can report
/// the edge crossed; `None` for absent or undecodable.
#[must_use]
pub fn stored_state(existing: Option<&[u8]>) -> Option<String> {
    let record = AgentRecordJson::decode(existing?)?;
    if record.state.is_empty() {
        return None;
    }
    Some(record.state)
}

/// Withdraw `state` from a stored record, preserving what a human authored.
///
/// Used to retract a record carrying human identity (a `DELETE` would lose
/// it for good) and to withdraw a declaration whose subject is gone. State
/// becomes `unknown` (safe beside any `kind`, I2); `attention` is cleared
/// with it, since the server derives it from state. Byte-idempotent. `None` when
/// nothing is stored.
#[must_use]
pub fn withdraw_state(existing: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut record = AgentRecordJson::decode(existing?)?;
    record.state.clear();
    record.state.push_str("unknown");
    record.attention = None;
    Some(record.encode())
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
