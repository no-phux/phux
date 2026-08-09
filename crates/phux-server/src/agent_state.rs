//! Authority over the `phux.agent/v1` record (ADR-0046 §E).
//!
//! Two writers can reach one Terminal's record: a human/agent/plugin issuing
//! an explicit `SET_METADATA`, and the server-side detector. They must not
//! fight over it. The arbitration rule, normatively:
//!
//! > An explicit `SET_METADATA` on `phux.agent/v1` that supplies a `state`
//! > outranks the detector; the detector makes no further writes to that
//! > Terminal until the record is `DELETE`d. An explicit write that supplies
//! > only identity (`name` / `kind` / `session`) is preserved field-for-field
//! > and the detector fills `state` around it. The detector deletes only
//! > records it itself wrote.
//!
//! One narrow exception, and it is the whole of ADR-0046's "why" (see
//! `docs/spec/L3.md` §3.7, "Server as a producer"): a declaration outranks
//! the derivation *for as long as the pane is occupied by the agent it
//! describes*. On positive evidence that the declared occupant is gone the
//! server may **withdraw** the declaration — set `state` to `"unknown"`,
//! never substitute a derived value and never `DELETE` — preserving `name`,
//! `kind` and `session`. A `kill -9` runs no `EXIT` trap and issues no
//! `agent clear`, so without this a declared pane is wedged in a lie with no
//! path back to truth, which is precisely the failure mode level-triggering
//! exists to prevent. [`withdraw_state`] is that write, and it is the only
//! one the server makes over a declaration.
//!
//! # Two invariants the drain must hold
//!
//! **I1.** Every `metadata_set` on `phux.agent/v1` writes `kind`, `name` and
//! `state` in the SAME write, from the SAME source tuple, composed against
//! the SAME `existing` bytes read under the SAME state-lock. There is no path
//! that updates `state` without reasserting the `kind` the detector currently
//! believes — except where an explicit writer owns `kind`. This is why
//! [`compose`] no longer preserves a `kind` (or `name`) the detector itself
//! authored: the correction event is best-effort (`try_send`, dropped on a
//! full sink), so level-triggered reassertion on every write is the only
//! thing that self-heals.
//!
//! **I2.** `state: "unknown"` is the ONLY value that may pair with a possibly
//! stale `kind`, because `"unknown"` describes no process and therefore
//! cannot describe a *different* one. Every correction and withdrawal path
//! lands there. A subscriber must never observe one record whose `kind` and
//! `state` come from two different processes, not even for a tick.
//!
//! I2 is what forces [`explicit_kind_is_contradicted`]. I1 reasserts a `kind`
//! the DETECTOR authored, which repairs the occupant-change case on its own —
//! but only where the detector owns the `kind`. Where an explicit writer owns
//! it, `docs/spec/L3.md` §3.7 requires the server to preserve it, so the
//! reassertion cannot run and a derived `state` would land beside a `kind`
//! describing a process that is gone. The server may not correct that `kind`;
//! what it can do, and what §3.7's withdrawal bullet names in as many words,
//! is decline to assert a state about it. See that function for the rule.
//!
//! The declaration cannot be inferred from the stored bytes. The client's
//! `AgentMetaState` decodes an absent or unrecognized `state` to `Unknown`,
//! so `unknown` and "absent" are indistinguishable on the way back out — and
//! the detector's own writes carry a `state` too. So declaration is tracked
//! explicitly, populated ONLY from the `SET_METADATA` entry point. The
//! detector's drain writes through `ServerState::metadata_set` directly and
//! therefore never passes through it, which is exactly what makes the
//! bookkeeping honest.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private server module shared by the sibling runtime / state modules"
)]

use std::collections::HashSet;

use phux_protocol::ids::TerminalId as WireTerminalId;

use crate::agent_detect::record::AgentRecordJson;

/// Who currently owns each Terminal's `phux.agent/v1` record.
#[derive(Debug, Default)]
pub(crate) struct AgentRecordArbiter {
    /// Terminals whose record was written by an explicit `SET_METADATA` that
    /// SUPPLIED a `state`. The detector stands down for these until `DELETE`.
    declared: HashSet<WireTerminalId>,
    /// Terminals whose current record the detector authored — so it may
    /// rewrite or retract it, and only it.
    detector_owned: HashSet<WireTerminalId>,
    /// Terminals whose STORED record carries identity a human authored
    /// (`name` / `session` / `attention`).
    ///
    /// Distinct from [`Self::detector_owned`], and it has to be: after an
    /// identity-only `SET_METADATA` the detector is deliberately left running
    /// to fill `state` in, and its very next write re-acquires ownership. So
    /// "the detector wrote the record currently stored" is TRUE of a record
    /// whose name the human chose — and using that alone to authorize a
    /// `DELETE` on retract destroys their label. The detector owns the `state`
    /// field; it never owns the identity.
    explicit_identity: HashSet<WireTerminalId>,
    /// Terminals whose STORED record carries a `kind` an explicit writer set.
    ///
    /// Deliberately separate from [`Self::explicit_identity`], which excludes
    /// `kind` on the grounds that "kind is not identity" — true of the
    /// *retract* question that set was built for (a bare kind is not something
    /// a human would miss), and false of the *correction* question:
    /// `docs/spec/L3.md` §3.7 requires a server to preserve the `kind` of an
    /// identity-only declaration, and now that the detector reasserts the
    /// `kind` it authored on every write (I1), an explicitly-set one needs a
    /// bucket of its own or it would be overwritten with the rest.
    explicit_kind: HashSet<WireTerminalId>,
}

/// Which identity fields of a stored record belong to an explicit writer and
/// must therefore survive a detector write.
///
/// Every field NOT owned here is reasserted from the detector's report on
/// every single write — that is invariant I1, and it is what makes a dropped
/// [`crate::agent_detect::AgentDetectEvent::Reidentified`] harmless rather
/// than a permanent lie.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IdentityOwnership {
    /// An explicit writer supplied `name`; keep theirs.
    pub(crate) name: bool,
    /// An explicit writer supplied `kind`; keep theirs.
    pub(crate) kind: bool,
}

impl IdentityOwnership {
    /// Nothing is owned: every field is the detector's to write. The shape of
    /// a pane no human has ever labelled.
    #[cfg(test)]
    pub(crate) const DETECTOR: Self = Self {
        name: false,
        kind: false,
    };
}

impl AgentRecordArbiter {
    /// Note an explicit `SET_METADATA` on this Terminal's agent record.
    ///
    /// The Terminal becomes `declared` **iff** the write supplied a real
    /// `state` — a bare identity declaration (`name`/`kind`/`session` only,
    /// or an explicit `"unknown"`) leaves the detector free to fill `state`
    /// in around it, which is the useful half of the feature: a human names
    /// the agent, the detector tracks its lifecycle.
    ///
    /// Either way the detector no longer owns the record, so it must not
    /// delete it. A write that supplies `name`, `session` or `attention` also
    /// marks the record as carrying human-authored identity, which the
    /// detector must never retract even once it owns the `state` again.
    ///
    /// `SET_METADATA` replaces the stored value wholesale, so a later write
    /// that drops those fields drops the mark with them: this tracks what is
    /// IN THE STORE, not what was ever written.
    pub(crate) fn note_explicit_set(&mut self, terminal: &WireTerminalId, value: &[u8]) {
        self.detector_owned.remove(terminal);
        let record = AgentRecordJson::decode(value);
        let declares_state = record
            .as_ref()
            .is_some_and(|r| !r.state.is_empty() && r.state != "unknown");
        if declares_state {
            self.declared.insert(terminal.clone());
        } else {
            self.declared.remove(terminal);
        }
        // `kind` is not identity: the detector derives it itself, and a record
        // holding nothing but a kind is not something a human would miss.
        let supplies_identity = record
            .as_ref()
            .is_some_and(|r| !r.name.is_empty() || r.session.is_some() || r.attention.is_some());
        if supplies_identity {
            self.explicit_identity.insert(terminal.clone());
        } else {
            self.explicit_identity.remove(terminal);
        }
        // ... but a kind an explicit writer DID supply is still theirs, and
        // L3.md §3.7 says to preserve it. Tracked separately from
        // `explicit_identity` precisely because it must not, on its own,
        // protect a record from the retract `DELETE`.
        let supplies_kind = record
            .as_ref()
            .is_some_and(|r| r.kind.as_ref().is_some_and(|k| !k.is_empty()));
        if supplies_kind {
            self.explicit_kind.insert(terminal.clone());
        } else {
            self.explicit_kind.remove(terminal);
        }
    }

    /// Note an explicit `DELETE_METADATA`. The declaration is withdrawn, the
    /// human's identity is gone from the store with the rest of the record,
    /// and the detector resumes full ownership.
    pub(crate) fn note_explicit_delete(&mut self, terminal: &WireTerminalId) {
        self.declared.remove(terminal);
        self.detector_owned.remove(terminal);
        self.explicit_identity.remove(terminal);
        self.explicit_kind.remove(terminal);
    }

    /// Withdraw a declaration whose subject is provably gone (`docs/spec/L3.md`
    /// §3.7, "Server as a producer"; ADR-0046 point 8).
    ///
    /// Clears `declared` and NOTHING else. The human's `name`, `session` and
    /// `kind` are still theirs — the withdrawal preserves them in the record
    /// and this preserves the bookkeeping that protects them. `detector_owned`
    /// is deliberately NOT set: the detector has not authored this record, and
    /// must not gain the right to `DELETE` it by having withdrawn a state it
    /// never wrote. Its next `State` write acquires ownership the normal way.
    ///
    /// Not a deletion, and not a substitution of a derived value — the two
    /// things §3.7 forbids. Losing information only in the honest direction.
    pub(crate) fn note_declaration_withdrawn(&mut self, terminal: &WireTerminalId) {
        self.declared.remove(terminal);
    }

    /// Whether the stored record carries identity a human authored, in which
    /// case the detector may withdraw its `state` but must not `DELETE` the
    /// key.
    pub(crate) fn has_explicit_identity(&self, terminal: &WireTerminalId) -> bool {
        self.explicit_identity.contains(terminal)
    }

    /// Whether the stored record's `kind` was supplied by an explicit writer,
    /// in which case the detector must preserve it rather than reassert its
    /// own (`docs/spec/L3.md` §3.7).
    pub(crate) fn has_explicit_kind(&self, terminal: &WireTerminalId) -> bool {
        self.explicit_kind.contains(terminal)
    }

    /// The ownership bits for one Terminal, read together so a caller cannot
    /// take them from two different states of the world.
    pub(crate) fn identity_ownership(&self, terminal: &WireTerminalId) -> IdentityOwnership {
        IdentityOwnership {
            name: self.has_explicit_identity(terminal),
            kind: self.has_explicit_kind(terminal),
        }
    }

    /// Whether a human has declared this Terminal's state, in which case the
    /// detector must not write.
    pub(crate) fn is_declared(&self, terminal: &WireTerminalId) -> bool {
        self.declared.contains(terminal)
    }

    /// Note that the detector authored this Terminal's current record.
    pub(crate) fn note_detector_write(&mut self, terminal: &WireTerminalId) {
        self.detector_owned.insert(terminal.clone());
    }

    /// Note that the detector retracted this Terminal's record.
    pub(crate) fn note_detector_retract(&mut self, terminal: &WireTerminalId) {
        self.detector_owned.remove(terminal);
    }

    /// Whether the detector authored the record currently stored, and may
    /// therefore delete it.
    pub(crate) fn detector_owns(&self, terminal: &WireTerminalId) -> bool {
        self.detector_owned.contains(terminal)
    }

    /// Drop all bookkeeping for a reaped Terminal.
    pub(crate) fn forget(&mut self, terminal: &WireTerminalId) {
        self.declared.remove(terminal);
        self.detector_owned.remove(terminal);
        self.explicit_identity.remove(terminal);
        self.explicit_kind.remove(terminal);
    }
}

/// Compose the record the detector should write, preserving every field an
/// explicit (state-less) declaration supplied.
///
/// `existing` is the currently-stored value, if any. `name` and `kind` come
/// from the detector's manifest and yield ONLY to a field `owned` says an
/// explicit writer supplied: if someone called their pane "reviewer", it stays
/// "reviewer" while the detector tracks its state. `session` is carried
/// through untouched.
///
/// Everything else is **reasserted on every write**. That is invariant I1, and
/// it is not an optimization to skip: the previous rule ("keep whatever kind is
/// already there") meant that when a pane's occupant changed, the record kept
/// the OLD kind and gained a state derived from the NEW one — nothing looked
/// stale, and the kind was a lie. The corrective event is best-effort
/// (`try_send`), so only level-triggered reassertion actually self-heals.
///
/// An empty stored `name` or `kind` is not an owned one: `name` is REQUIRED and
/// non-empty per `docs/spec/L3.md` §3.7, so a blank is filled whoever "owns" it.
///
/// `attention` is deliberately NOT set by the detector — `docs/spec/L3.md`
/// §3.7 already derives it from `state` when absent — but a declared one is
/// preserved.
pub(crate) fn compose(
    existing: Option<&[u8]>,
    kind: &str,
    name: &str,
    state: &str,
    owned: IdentityOwnership,
) -> Vec<u8> {
    let prior = existing.and_then(AgentRecordJson::decode);
    let record = match prior {
        Some(mut prior) => {
            if !owned.name || prior.name.is_empty() {
                prior.name.clear();
                prior.name.push_str(name);
            }
            if !owned.kind || prior.kind.as_ref().is_none_or(String::is_empty) {
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

/// Whether the detector has positive evidence that the `kind` an explicit
/// writer stored describes a process that is NOT in the pane (phux-w7z2.45).
///
/// # The interaction this exists for
///
/// The Claude hook shim declares `--name claude --kind claude`, so a shim pane
/// is `explicit_kind` for its whole life. On a `claude` -> `codex` handover in
/// that pane the record therefore keeps `kind: claude` — `docs/spec/L3.md` §3.7
/// requires a server to preserve the `kind` of an identity-only declaration —
/// and then takes codex's DERIVED state beside it. Nothing looks stale: the
/// state is fresh, the name is present, the kind is a lie. That is precisely
/// the failure phux-w7z2.27 was filed to fix, surviving on the largest
/// population of panes, and it is why I1's reassertion is not enough on its own.
///
/// # What the server is allowed to do about it
///
/// Not correct the `kind`: §3.7 lists it on two MUST-preserve lists and the
/// server does not get to overrule an explicit writer's field. What §3.7 does
/// permit, in the withdrawal bullet, is to set `state` to `"unknown"` "when it
/// has positive evidence that the declared occupant of the pane is gone: for
/// example, the PTY's foreground process group ... resolves to a different
/// one" — our exact evidence, named in the spec's own example. So the server
/// keeps every field the writer owns and stops asserting a state it cannot
/// attribute honestly. Losing information only in the honest direction.
///
/// The landing shape (`kind` present, `state: unknown`) is the WITHDRAWN shape
/// ADR-0075 point 6's `%name` write gate already refuses, so a fleet driver
/// declines to deliver input into the pane rather than delivering it to the
/// wrong agent. That is the outcome .27 was protecting.
///
/// # Why "contradicted" and not merely "different"
///
/// `phux agent set --name reviewer --kind my-agent` on a pane running claude
/// is the documented useful half of the feature, and its `kind` differs from
/// the detector's on every single tick. An open-vocabulary slug the detector
/// has no manifest for asserts nothing the detector can check, so it cannot be
/// contradicted and the detector keeps filling `state` in as before. Only a
/// `kind` the detector could ITSELF have produced — one with a loaded manifest
/// — is falsifiable, and only then by a different such kind.
///
/// # Self-healing, and free
///
/// Level-triggered, with no memory: the moment the pane's occupant matches the
/// stored `kind` again, or an explicit writer refreshes it (the shim's next
/// `SessionStart`), or `phux agent clear` removes it, the detector resumes.
/// And the frozen write is byte-identical to the record already stored, so
/// `metadata_set` suppresses it: a contradicted pane costs ZERO writes and
/// ZERO broadcasts per tick, not one (ADR-0046 decision 7).
pub(crate) fn explicit_kind_is_contradicted(
    existing: Option<&[u8]>,
    detected: &str,
    rules: &crate::agent_detect::rules::RuleSet,
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
    // Only a kind the detector could have derived is a claim the detector is
    // in a position to falsify. Kinds are lowercase slugs; a writer who typed
    // one in another case is given the benefit of the doubt by the lookup
    // missing, which errs toward leaving them alone.
    rules.manifest(&stored.to_lowercase()).is_some()
}

/// The `state` word currently stored for a pane, if any.
///
/// Read before a detector write so the `agent-state-changed` hook can report
/// the edge it crossed rather than only where it landed. An absent or
/// undecodable record yields `None`, which the hook renders as "no prior
/// state" — honestly distinct from a transition out of `idle`.
pub(crate) fn stored_state(existing: Option<&[u8]>) -> Option<String> {
    let record = AgentRecordJson::decode(existing?)?;
    if record.state.is_empty() {
        return None;
    }
    Some(record.state)
}

/// Withdraw the `state` from a stored record, preserving every field a human
/// authored.
///
/// The counterpart to [`compose`], for two paths:
///
/// 1. the retract of a record that also carries human-authored identity (see
///    [`AgentRecordArbiter::has_explicit_identity`]) — `DELETE`ing the key
///    there would wipe the name, session and attention the human chose, and
///    they are unrecoverable, because restarting the agent only re-creates the
///    detector's own view of it; and
/// 2. the withdrawal of an explicit **declaration** whose subject is provably
///    gone (see [`AgentRecordArbiter::note_declaration_withdrawn`]).
///
/// Either way the state falls back to the vocabulary's `unknown`: the agent is
/// gone, and a dead process must not lie about being `working`. `unknown` is
/// also the only value safe to pair with a `kind` this function does not
/// touch (I2) — it describes no process, so it cannot describe the wrong one.
///
/// `attention` is cleared with the state, and that is not incidental: L3 §3.7
/// derives `attention` from `state` when absent, so a record left reading
/// `state: unknown, attention: high` heals into a pane that is nothing at all
/// and still wearing a red badge — a notification for a process that no longer
/// exists. §3.7's MUST-preserve list is `name`, `kind` and `session`;
/// `attention` is deliberately not on it.
///
/// Byte-idempotent: withdrawing an already-withdrawn record yields identical
/// bytes, so `metadata_set` suppresses the broadcast and a repeated withdrawal
/// costs nothing.
///
/// `None` when there is no stored record to rewrite.
pub(crate) fn withdraw_state(existing: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut record = AgentRecordJson::decode(existing?)?;
    record.state.clear();
    record.state.push_str("unknown");
    record.attention = None;
    Some(record.encode())
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use phux_protocol::ids::TerminalId as WireTerminalId;

    use super::{
        AgentRecordArbiter, IdentityOwnership, compose, explicit_kind_is_contradicted,
        withdraw_state,
    };
    use crate::agent_detect::record::AgentRecordJson;
    use crate::agent_detect::rules::{ManifestSpec, RuleSet};

    fn terminal(id: u32) -> WireTerminalId {
        WireTerminalId::new(id)
    }

    /// A pane no explicit writer has ever touched: every identity field is
    /// the detector's to assert.
    const DETECTOR: IdentityOwnership = IdentityOwnership::DETECTOR;

    /// The ownership bits an arbiter would report after `value` was written
    /// by an explicit `SET_METADATA` — so the `compose` cases below are wired
    /// exactly as the drain wires them, rather than to hand-picked bools.
    fn ownership_after(value: &[u8]) -> IdentityOwnership {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, value);
        arb.identity_ownership(&t)
    }

    #[test]
    fn a_declared_state_stands_the_detector_down() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        assert!(!arb.is_declared(&t), "nothing declared yet");
        arb.note_explicit_set(&t, br#"{"name":"me","state":"blocked"}"#);
        assert!(arb.is_declared(&t));
    }

    /// The useful half: a human names the agent, the detector keeps tracking
    /// its lifecycle.
    #[test]
    fn an_identity_only_declaration_leaves_the_detector_running() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, br#"{"name":"reviewer","kind":"claude"}"#);
        assert!(!arb.is_declared(&t));
    }

    #[test]
    fn an_explicit_unknown_state_is_not_a_declaration() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, br#"{"name":"x","state":"unknown"}"#);
        assert!(!arb.is_declared(&t), "`unknown` declares nothing");
    }

    #[test]
    fn a_delete_withdraws_the_declaration_and_the_detector_resumes() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, br#"{"name":"me","state":"done"}"#);
        assert!(arb.is_declared(&t));
        arb.note_explicit_delete(&t);
        assert!(!arb.is_declared(&t));
    }

    /// The detector may only delete what it wrote. A human's record is not
    /// its to retract.
    #[test]
    fn the_detector_only_owns_records_it_wrote() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        assert!(!arb.detector_owns(&t));
        arb.note_detector_write(&t);
        assert!(arb.detector_owns(&t));
        arb.note_detector_retract(&t);
        assert!(!arb.detector_owns(&t));
    }

    /// An explicit write over a detector-authored record transfers ownership
    /// away, so the detector can no longer delete it.
    #[test]
    fn an_explicit_set_takes_ownership_from_the_detector() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_detector_write(&t);
        arb.note_explicit_set(&t, br#"{"name":"mine","kind":"claude"}"#);
        assert!(!arb.detector_owns(&t), "the detector must not delete this");
        assert!(!arb.is_declared(&t), "but it may still fill in `state`");
    }

    /// THE label-eater. `phux agent set --name reviewer` is deliberately NOT a
    /// declaration — the detector keeps running so it can fill `state` in. But
    /// its very next write re-acquires `detector_owned`, so by the time the
    /// agent exits, "the detector authored the stored record" is true of a
    /// record whose NAME the human chose. Authorizing the retract `DELETE` off
    /// that bit alone destroys their name, session and attention — and
    /// unrecoverably, since restarting the agent only re-creates the detector's
    /// own view of it. Ownership of `state` is not ownership of the identity.
    #[test]
    fn a_detector_write_over_a_humans_name_does_not_make_the_record_deletable() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(
            &t,
            br#"{"name":"reviewer","kind":"claude","session":"fleet-7"}"#,
        );
        assert!(!arb.is_declared(&t), "identity only: the detector runs on");
        assert!(arb.has_explicit_identity(&t));

        // The detector fills `state` in, re-acquiring ownership of the record.
        arb.note_detector_write(&t);
        assert!(arb.detector_owns(&t), "it did write the record");
        assert!(
            arb.has_explicit_identity(&t),
            "but the human's identity is still in there, and is not ours to delete",
        );
    }

    /// A `SET_METADATA` replaces the stored value wholesale, so a later write
    /// that drops the identity fields drops the mark with them. The set tracks
    /// what is IN THE STORE, not what was ever written to it.
    #[test]
    fn an_explicit_set_without_identity_fields_clears_the_mark() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, br#"{"name":"reviewer"}"#);
        assert!(arb.has_explicit_identity(&t));
        arb.note_explicit_set(&t, br#"{"name":"","state":"done"}"#);
        assert!(
            !arb.has_explicit_identity(&t),
            "the name is gone from the store; there is nothing left to preserve",
        );
    }

    /// A `kind` is not identity: the detector derives it itself, so a record
    /// holding nothing else is not something a human would miss.
    #[test]
    fn a_bare_kind_is_not_human_authored_identity() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, br#"{"name":"","kind":"claude"}"#);
        assert!(!arb.has_explicit_identity(&t));
    }

    #[test]
    fn a_delete_drops_the_human_authored_identity_mark() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, br#"{"name":"reviewer"}"#);
        arb.note_explicit_delete(&t);
        assert!(
            !arb.has_explicit_identity(&t),
            "the record is gone from the store, and the identity with it",
        );
    }

    #[test]
    fn malformed_bytes_declare_nothing() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, b"not json at all");
        assert!(!arb.is_declared(&t));
    }

    #[test]
    fn forget_drops_every_trace_of_a_reaped_terminal() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(7);
        arb.note_explicit_set(&t, br#"{"name":"x","state":"working"}"#);
        arb.note_detector_write(&t);
        arb.forget(&t);
        assert!(!arb.is_declared(&t));
        assert!(!arb.detector_owns(&t));
        assert!(!arb.has_explicit_identity(&t));
    }

    #[test]
    fn terminals_are_tracked_independently() {
        let mut arb = AgentRecordArbiter::default();
        let (a, b) = (terminal(1), terminal(2));
        arb.note_explicit_set(&a, br#"{"name":"a","state":"done"}"#);
        assert!(arb.is_declared(&a));
        assert!(!arb.is_declared(&b));
    }

    // --- the explicit-kind bucket (L3 §3.7's preserve list) ----------------

    /// `explicit_identity` deliberately excludes `kind` ("a bare kind is not
    /// something a human would miss") — correct for the DELETE question it
    /// governs, and not an answer to the preserve question. L3 §3.7 lists
    /// `kind` alongside `name` and `session`, and now that the detector
    /// reasserts its own `kind` on every write, an explicitly-set one needs a
    /// bucket of its own or it is quietly overwritten.
    #[test]
    fn an_explicit_kind_is_tracked_even_though_it_is_not_identity() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, br#"{"name":"","kind":"my-agent"}"#);
        assert!(
            !arb.has_explicit_identity(&t),
            "still not identity: this record is not protected from DELETE",
        );
        assert!(arb.has_explicit_kind(&t), "but the kind is theirs to keep");
        let owned = arb.identity_ownership(&t);
        assert!(!owned.name);
        assert!(owned.kind);
    }

    #[test]
    fn a_record_with_no_kind_leaves_the_kind_to_the_detector() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, br#"{"name":"reviewer"}"#);
        assert!(!arb.has_explicit_kind(&t));
        arb.note_explicit_set(&t, br#"{"name":"reviewer","kind":""}"#);
        assert!(!arb.has_explicit_kind(&t), "an empty kind supplies nothing");
    }

    #[test]
    fn a_delete_and_a_reap_drop_the_explicit_kind() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, br#"{"kind":"my-agent"}"#);
        arb.note_explicit_delete(&t);
        assert!(!arb.has_explicit_kind(&t));

        arb.note_explicit_set(&t, br#"{"kind":"my-agent"}"#);
        arb.forget(&t);
        assert!(!arb.has_explicit_kind(&t), "a reaped pane keeps nothing");
    }

    // --- withdrawing a declaration (phux-w7z2.13) --------------------------

    /// THE wedge, at the arbiter. A declared pane whose process is confirmed
    /// gone has its declaration withdrawn — and only that. The human's label
    /// is still theirs, and the detector does NOT inherit the right to delete
    /// a record it never wrote.
    #[test]
    fn withdrawing_a_declaration_clears_only_the_declaration() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(
            &t,
            br#"{"name":"me","kind":"claude","session":"fleet-7","state":"working"}"#,
        );
        assert!(arb.is_declared(&t));

        arb.note_declaration_withdrawn(&t);

        assert!(!arb.is_declared(&t), "the detector may derive again");
        assert!(
            arb.has_explicit_identity(&t),
            "the human's name and session are still theirs",
        );
        assert!(arb.has_explicit_kind(&t), "as is their kind");
        assert!(
            !arb.detector_owns(&t),
            "withdrawing a state it never wrote does not make the record deletable",
        );
    }

    /// And the detector picks the record back up the ordinary way: its next
    /// write is what makes the record its own.
    #[test]
    fn after_a_withdrawal_the_detector_reacquires_by_writing() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, br#"{"name":"me","state":"working"}"#);
        arb.note_declaration_withdrawn(&t);
        arb.note_detector_write(&t);
        assert!(arb.detector_owns(&t));
        assert!(
            arb.has_explicit_identity(&t),
            "which still does not make the human's name the detector's",
        );
    }

    // --- compose ----------------------------------------------------------

    #[test]
    fn compose_from_nothing_writes_the_detector_view() {
        let bytes = compose(None, "claude", "claude", "working", DETECTOR);
        assert_eq!(
            String::from_utf8(bytes).expect("utf8"),
            r#"{"name":"claude","kind":"claude","state":"working"}"#
        );
    }

    /// The field-for-field preservation the ADR promises.
    #[test]
    fn compose_preserves_an_identity_only_declaration() {
        let existing = br#"{"name":"reviewer","kind":"claude","session":"fleet-7"}"#;
        let bytes = compose(
            Some(existing),
            "claude",
            "claude",
            "blocked",
            ownership_after(existing),
        );
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.name, "reviewer", "the human's name survives");
        assert_eq!(got.session.as_deref(), Some("fleet-7"), "and their label");
        assert_eq!(got.state, "blocked", "the detector supplies only `state`");
    }

    #[test]
    fn compose_preserves_a_declared_attention() {
        let existing = br#"{"name":"a","attention":"high"}"#;
        let bytes = compose(
            Some(existing),
            "claude",
            "claude",
            "idle",
            ownership_after(existing),
        );
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.attention.as_deref(), Some("high"));
        assert_eq!(got.state, "idle");
    }

    #[test]
    fn compose_fills_a_missing_name_and_kind() {
        let existing = br#"{"name":"","session":"s"}"#;
        let bytes = compose(
            Some(existing),
            "claude",
            "claude",
            "idle",
            ownership_after(existing),
        );
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(
            got.name, "claude",
            "an empty stored name is not an owned one: L3 §3.7 requires a non-empty `name`",
        );
        assert_eq!(got.kind.as_deref(), Some("claude"));
        assert_eq!(got.session.as_deref(), Some("s"));
    }

    /// THE .27 half, at the composition seam. A `kind` the DETECTOR wrote is
    /// reasserted from the current report on every single write.
    ///
    /// Previously any already-present `kind` was preserved, so when a pane's
    /// occupant changed the record kept the old `kind` and gained a state
    /// derived from the new occupant's screen: a fresh state, a present name,
    /// and a kind that was simply a lie. Nothing about the record looked
    /// stale, which is what made it undetectable from the outside.
    #[test]
    fn compose_reasserts_a_kind_the_detector_authored() {
        let existing = br#"{"name":"claude","kind":"claude","state":"working"}"#;
        let bytes = compose(Some(existing), "codex", "codex", "idle", DETECTOR);
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(
            got.kind.as_deref(),
            Some("codex"),
            "the pane runs codex now; the record must not keep saying claude",
        );
        assert_eq!(got.name, "codex", "and the name it came with");
        assert_eq!(got.state, "idle");
    }

    /// The other side of the same rule: a `kind` an explicit writer supplied
    /// is theirs, and `docs/spec/L3.md` §3.7 says to preserve it.
    #[test]
    fn compose_preserves_an_explicitly_set_kind() {
        let existing = br#"{"name":"reviewer","kind":"my-agent"}"#;
        let owned = ownership_after(existing);
        assert!(owned.kind, "the writer supplied a kind");
        let bytes = compose(Some(existing), "claude", "claude", "working", owned);
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.kind.as_deref(), Some("my-agent"));
        assert_eq!(got.name, "reviewer");
        assert_eq!(got.state, "working");
    }

    /// Garbage in the store must not stop the detector from writing a clean
    /// record over it.
    #[test]
    fn compose_over_malformed_bytes_starts_fresh() {
        let bytes = compose(Some(b"}{ nonsense"), "claude", "claude", "idle", DETECTOR);
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.name, "claude");
        assert_eq!(got.state, "idle");
    }

    /// The dedup contract: recomposing an unchanged state yields byte-identical
    /// output, which is what makes `metadata_set` suppress the broadcast.
    ///
    /// Reasserting `kind` and `name` on every write must not disturb this —
    /// they are reasserted to the SAME values, so the bytes do not move.
    #[test]
    fn compose_is_stable_across_repeats() {
        let first = compose(None, "claude", "claude", "working", DETECTOR);
        let second = compose(Some(&first), "claude", "claude", "working", DETECTOR);
        assert_eq!(first, second, "a steady state must produce identical bytes");
    }

    // --- a contradicted explicit kind (phux-w7z2.45) ------------------------

    /// Two kinds the detector has manifests for, so "the detector could have
    /// derived this" is true of both — and one it has never heard of.
    fn detectable() -> RuleSet {
        let mut set = RuleSet::default();
        for kind in ["claude", "codex"] {
            let spec: ManifestSpec =
                toml::from_str(&format!("kind = \"{kind}\"\nbinaries = [\"{kind}\"]\n"))
                    .expect("manifest parses");
            set.install(spec).expect("compiles");
        }
        set
    }

    /// THE .45 case. A shim pane declares `kind: claude`; the human kills
    /// claude and runs codex in it. The record must not take codex's derived
    /// state while still reading `kind: claude` — a state and a kind from two
    /// different processes is exactly the lie .27 was filed to remove, and I2
    /// forbids it whoever owns the field.
    #[test]
    fn a_declared_kind_the_pane_no_longer_runs_is_contradicted() {
        let stored = br#"{"name":"claude","kind":"claude","state":"working"}"#;
        assert!(explicit_kind_is_contradicted(
            Some(stored),
            "codex",
            &detectable()
        ));
    }

    /// The regression guard for the documented useful half. An
    /// open-vocabulary slug the detector has no manifest for asserts nothing
    /// the detector can check, so it is never contradicted and the detector
    /// keeps filling `state` in around it — which is the entire point of
    /// `phux agent set --name reviewer --kind my-agent`.
    #[test]
    fn an_open_vocabulary_kind_the_detector_cannot_derive_is_never_contradicted() {
        let stored = br#"{"name":"reviewer","kind":"my-agent"}"#;
        assert!(
            !explicit_kind_is_contradicted(Some(stored), "claude", &detectable()),
            "a label the detector could never have produced is not a claim it can falsify",
        );
    }

    #[test]
    fn a_kind_the_pane_actually_runs_is_not_contradicted() {
        let stored = br#"{"name":"claude","kind":"claude","state":"idle"}"#;
        let rules = detectable();
        assert!(!explicit_kind_is_contradicted(
            Some(stored),
            "claude",
            &rules
        ));
        assert!(
            !explicit_kind_is_contradicted(
                Some(br#"{"name":"c","kind":"Claude"}"#),
                "claude",
                &rules
            ),
            "the same kind in another case is the same kind",
        );
    }

    /// Nothing to contradict: no record, no `kind`, an empty `kind`, or bytes
    /// that do not decode. Every one of these leaves the detector free, which
    /// is the fail-open direction — this predicate can only ever WITHHOLD a
    /// state, so an over-eager one is the expensive mistake.
    #[test]
    fn a_record_without_a_usable_kind_contradicts_nothing() {
        let rules = detectable();
        assert!(!explicit_kind_is_contradicted(None, "codex", &rules));
        assert!(!explicit_kind_is_contradicted(
            Some(br#"{"name":"x"}"#),
            "codex",
            &rules
        ));
        assert!(!explicit_kind_is_contradicted(
            Some(br#"{"name":"x","kind":""}"#),
            "codex",
            &rules
        ));
        assert!(!explicit_kind_is_contradicted(
            Some(b"}{ nonsense"),
            "codex",
            &rules
        ));
    }

    /// A server with no manifests loaded (`PHUX_AGENT_DETECT=0`, or an
    /// operator whose overrides all failed to compile) can falsify nothing.
    #[test]
    fn an_empty_rule_set_contradicts_nothing() {
        let stored = br#"{"name":"claude","kind":"claude"}"#;
        assert!(!explicit_kind_is_contradicted(
            Some(stored),
            "codex",
            &RuleSet::default()
        ));
    }

    // --- withdraw_state ----------------------------------------------------

    /// The retract path for a record a human named. Their fields survive; the
    /// fields describing a process that no longer exists do not.
    ///
    /// The `attention` assertion INVERTED here, deliberately. It previously
    /// pinned `attention: "high"` surviving into a withdrawn record — a pane
    /// that is nothing at all, wearing a red "needs you" badge, for a process
    /// that is gone. L3 §3.7's MUST-preserve list is `name`, `kind` and
    /// `session`; `attention` is deliberately not on it, and §3.7 derives it
    /// from `state` when absent. Clearing it is both spec-legal and the only
    /// honest reading of `unknown`.
    #[test]
    fn withdraw_state_keeps_the_human_fields_and_drops_the_detectors() {
        let stored = br#"{"name":"reviewer","kind":"claude","state":"working","attention":"high","session":"fleet-7"}"#;
        let bytes = withdraw_state(Some(stored)).expect("a record to rewrite");
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.name, "reviewer", "the human's name survives the agent");
        assert_eq!(got.session.as_deref(), Some("fleet-7"));
        assert_eq!(got.kind.as_deref(), Some("claude"), "and their kind");
        assert_eq!(got.state, "unknown", "and the detector's verdict is gone");
        assert_eq!(
            got.attention, None,
            "an unknown pane must not keep a badge demanding attention for a dead process",
        );
    }

    /// The write-rate guard for the withdrawal path: withdrawing twice yields
    /// identical bytes, so `metadata_set` suppresses the second broadcast. A
    /// withdrawal that varied — a timestamp, a counter, a `withdrawn_at` —
    /// would be a write and a `METADATA_CHANGED` per subscriber per tick.
    #[test]
    fn withdraw_state_is_byte_idempotent() {
        let stored = br#"{"name":"me","kind":"claude","state":"working","attention":"high"}"#;
        let first = withdraw_state(Some(stored)).expect("a record to rewrite");
        let second = withdraw_state(Some(&first)).expect("still a record");
        assert_eq!(
            first, second,
            "withdrawing an unknown record changes nothing"
        );
    }

    #[test]
    fn withdraw_state_has_nothing_to_rewrite_without_a_record() {
        assert!(withdraw_state(None).is_none());
        assert!(withdraw_state(Some(b"}{ nonsense")).is_none());
    }
}
