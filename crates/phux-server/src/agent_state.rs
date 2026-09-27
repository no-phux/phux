//! Authority over the `phux.agent/v1` record (ADR-0046 §E).
//!
//! An explicit `SET_METADATA` that supplies a `state` outranks the detector
//! until `DELETE`; one that supplies only identity is preserved field for
//! field while the detector fills `state` around it. The detector deletes
//! only records it wrote. The one exception (`docs/spec/L3.md` §3.7): on
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
    reason = "private server module shared by the sibling runtime / state modules"
)]

use std::collections::HashSet;

use phux_protocol::ids::ResourceId as WireResourceId;

use crate::agent_detect::record::AgentRecordJson;

/// Where a claim about a Terminal's agent `state` came from, ordered by
/// authority (ADR-0103 §5): closeness to the agent's own knowledge, not
/// freshness. A declaration ([`AgentRecordArbiter`]) outranks all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "the Stream producer lands with the AgentSession engine; the ladder is defined and pinned by tests here so the screen detector's side of the precedence is already correct when it does"
)]
pub(crate) enum EvidenceSource {
    /// Derived from the grid by the rules (ADR-0046): the weakest source.
    Screen,
    /// The PTY's foreground process group: identity and departure, positive
    /// evidence rather than inference.
    Process,
    /// `REPORT_AGENT_STATE` from an integration (ADR-0085): the agent
    /// speaking, but lossy and edge-triggered.
    Hook,
    /// A live `AgentSession` child's record stream (ADR-0103): the agent
    /// describing itself over a sequenced, gap-detecting channel.
    Stream,
}

#[allow(
    dead_code,
    reason = "read by the ladder's own tests and by the AgentSession engine, which lands with that engine"
)]
impl EvidenceSource {
    /// Rank within the ladder: higher wins. Separate from the ask ladder
    /// ([`crate::agent_asked::AskedSource::priority`]).
    pub(crate) const fn priority(self) -> u8 {
        match self {
            Self::Screen => 0,
            Self::Process => 1,
            Self::Hook => 2,
            Self::Stream => 3,
        }
    }

    /// Whether `self` may publish over `incumbent`. Reflexive: re-asserting
    /// one's own claim is a refresh.
    pub(crate) const fn outranks(self, incumbent: Self) -> bool {
        self.priority() >= incumbent.priority()
    }
}

/// Who currently owns each Terminal's `phux.agent/v1` record.
#[derive(Debug, Default)]
pub(crate) struct AgentRecordArbiter {
    /// Terminals whose record an explicit `SET_METADATA` wrote with a
    /// `state`; the detector stands down until `DELETE`.
    declared: HashSet<WireResourceId>,
    /// Terminals whose current record the detector wrote (and may retract).
    detector_owned: HashSet<WireResourceId>,
    /// Terminals whose stored record carries human-authored identity
    /// (`name` / `session` / `attention`). The detector may own the `state`
    /// of such a record but must never `DELETE` it.
    explicit_identity: HashSet<WireResourceId>,
    /// Terminals whose stored `kind` an explicit writer set, which the
    /// detector must preserve (§3.7). Separate from `explicit_identity`,
    /// since a bare kind does not protect a record from deletion.
    explicit_kind: HashSet<WireResourceId>,
}

/// Which identity fields of a stored record an explicit writer owns; every
/// other field is reasserted on each detector write (I1).
#[derive(Debug, Clone, Copy)]
pub(crate) struct IdentityOwnership {
    /// An explicit writer supplied `name`; keep theirs.
    pub(crate) name: bool,
    /// An explicit writer supplied `kind`; keep theirs.
    pub(crate) kind: bool,
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
    /// Note an explicit `SET_METADATA` on this Terminal's agent record.
    ///
    /// Supplying a real `state` makes it declared; identity-only writes
    /// leave the detector to fill in `state`. Either way the detector loses
    /// ownership. Marks track what is in the store (writes replace wholesale).
    pub(crate) fn note_explicit_set(&mut self, terminal: &WireResourceId, value: &[u8]) {
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
        // A kind alone is not identity a human would miss...
        let supplies_identity = record
            .as_ref()
            .is_some_and(|r| !r.name.is_empty() || r.session.is_some() || r.attention.is_some());
        if supplies_identity {
            self.explicit_identity.insert(terminal.clone());
        } else {
            self.explicit_identity.remove(terminal);
        }
        // ...but an explicitly supplied kind is still preserved (§3.7).
        let supplies_kind = record
            .as_ref()
            .is_some_and(|r| r.kind.as_ref().is_some_and(|k| !k.is_empty()));
        if supplies_kind {
            self.explicit_kind.insert(terminal.clone());
        } else {
            self.explicit_kind.remove(terminal);
        }
    }

    /// Note an explicit `DELETE_METADATA`: everything clears and the detector
    /// resumes full ownership.
    pub(crate) fn note_explicit_delete(&mut self, terminal: &WireResourceId) {
        self.declared.remove(terminal);
        self.detector_owned.remove(terminal);
        self.explicit_identity.remove(terminal);
        self.explicit_kind.remove(terminal);
    }

    /// Withdraw a declaration whose subject is provably gone (§3.7).
    ///
    /// Clears `declared` only: identity bookkeeping stays, and the detector
    /// does not gain ownership (its next write acquires it normally).
    pub(crate) fn note_declaration_withdrawn(&mut self, terminal: &WireResourceId) {
        self.declared.remove(terminal);
    }

    /// Whether the stored record carries human identity (withdraw, never
    /// `DELETE`).
    pub(crate) fn has_explicit_identity(&self, terminal: &WireResourceId) -> bool {
        self.explicit_identity.contains(terminal)
    }

    /// Whether an explicit writer supplied the stored `kind` (preserve it).
    pub(crate) fn has_explicit_kind(&self, terminal: &WireResourceId) -> bool {
        self.explicit_kind.contains(terminal)
    }

    /// The ownership bits for one Terminal, read together.
    pub(crate) fn identity_ownership(&self, terminal: &WireResourceId) -> IdentityOwnership {
        IdentityOwnership {
            name: self.has_explicit_identity(terminal),
            kind: self.has_explicit_kind(terminal),
        }
    }

    /// Whether a human declared this Terminal's state (detector must not
    /// write).
    pub(crate) fn is_declared(&self, terminal: &WireResourceId) -> bool {
        self.declared.contains(terminal)
    }

    /// Note that the detector authored this Terminal's current record.
    pub(crate) fn note_detector_write(&mut self, terminal: &WireResourceId) {
        self.detector_owned.insert(terminal.clone());
    }

    /// Note that the detector retracted this Terminal's record.
    pub(crate) fn note_detector_retract(&mut self, terminal: &WireResourceId) {
        self.detector_owned.remove(terminal);
    }

    /// Whether the detector wrote the stored record (and may delete it).
    pub(crate) fn detector_owns(&self, terminal: &WireResourceId) -> bool {
        self.detector_owned.contains(terminal)
    }

    /// Drop all bookkeeping for a reaped Terminal.
    pub(crate) fn forget(&mut self, terminal: &WireResourceId) {
        self.declared.remove(terminal);
        self.detector_owned.remove(terminal);
        self.explicit_identity.remove(terminal);
        self.explicit_kind.remove(terminal);
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
/// kind would sit beside codex's derived state. §3.7 forbids correcting the
/// kind but permits withdrawing state on positive evidence the occupant is
/// gone, so the server withholds state (the shape `%name` writes refuse).
/// Only a kind with a loaded manifest can be contradicted: an open slug
/// like `my-agent` asserts nothing checkable. Stateless, and the frozen
/// write dedups to zero broadcasts.
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
    // Only a kind with a manifest is falsifiable; a case mismatch errs
    // toward leaving the writer alone.
    rules.manifest(&stored.to_lowercase()).is_some()
}

/// The stored `state`, if any, so the `agent-state-changed` hook can report
/// the edge crossed; `None` for absent or undecodable.
pub(crate) fn stored_state(existing: Option<&[u8]>) -> Option<String> {
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
/// with it, since §3.7 derives it from state. Byte-idempotent. `None` when
/// nothing is stored.
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
    use phux_protocol::ids::ResourceId as WireResourceId;

    use super::{
        AgentRecordArbiter, EvidenceSource, IdentityOwnership, compose,
        explicit_kind_is_contradicted, withdraw_state,
    };
    use crate::agent_detect::record::AgentRecordJson;
    use crate::agent_detect::rules::{ManifestSpec, RuleSet};

    fn terminal(id: u32) -> WireResourceId {
        WireResourceId::new(id)
    }

    /// A pane no explicit writer has touched.
    const DETECTOR: IdentityOwnership = IdentityOwnership::DETECTOR;

    /// The ownership an arbiter reports after `value` is explicitly written.
    fn ownership_after(value: &[u8]) -> IdentityOwnership {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_explicit_set(&t, value);
        arb.identity_ownership(&t)
    }

    /// Only an explicit write with a real `state` declares; identity-only,
    /// `unknown`, and malformed writes leave the detector running.
    #[test]
    fn only_a_real_state_declares() {
        let cases: &[(&[u8], bool)] = &[
            (br#"{"name":"me","state":"blocked"}"#, true),
            (br#"{"name":"reviewer","kind":"claude"}"#, false),
            (br#"{"name":"x","state":"unknown"}"#, false),
            (b"not json at all", false),
        ];
        for (value, declared) in cases {
            let mut arb = AgentRecordArbiter::default();
            let t = terminal(1);
            assert!(!arb.is_declared(&t));
            arb.note_explicit_set(&t, value);
            assert_eq!(arb.is_declared(&t), *declared, "{value:?}");
        }
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

    /// The detector deletes only what it wrote.
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

    /// An explicit write over a detector record transfers ownership away.
    #[test]
    fn an_explicit_set_takes_ownership_from_the_detector() {
        let mut arb = AgentRecordArbiter::default();
        let t = terminal(1);
        arb.note_detector_write(&t);
        arb.note_explicit_set(&t, br#"{"name":"mine","kind":"claude"}"#);
        assert!(!arb.detector_owns(&t), "the detector must not delete this");
        assert!(!arb.is_declared(&t), "but it may still fill in `state`");
    }

    /// An identity-only label survives the detector re-acquiring `state`
    /// ownership: retraction must not delete the human's name.
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

    /// A later write dropping identity fields drops the mark.
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

    /// A bare `kind` is not identity.
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

    /// An explicit `kind` is preserved even though it is not identity.
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

    /// Withdrawing a gone occupant's declaration keeps the label and does
    /// not grant the detector delete rights.
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

    /// The detector's next write makes the record its own.
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

    /// An identity-only SET that beat the arbiter bit is merged, not
    /// clobbered.
    #[test]
    fn compose_preserves_an_identity_only_name_without_ownership_bits() {
        let existing = br#"{"name":"reviewer","session":"fleet-7"}"#;
        let bytes = compose(Some(existing), "claude", "claude", "blocked", DETECTOR);
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(
            got.name, "reviewer",
            "the human's name survives a stale arbiter"
        );
        assert_eq!(got.session.as_deref(), Some("fleet-7"), "and their label");
        assert_eq!(got.state, "blocked", "the detector supplies only `state`");
        assert_eq!(
            got.kind.as_deref(),
            Some("claude"),
            "kind was not declared, so the detector fills it",
        );
    }

    /// Same race for `kind`.
    #[test]
    fn compose_preserves_an_identity_only_kind_without_ownership_bits() {
        let existing = br#"{"name":"reviewer","kind":"my-agent"}"#;
        let bytes = compose(Some(existing), "claude", "claude", "working", DETECTOR);
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.kind.as_deref(), Some("my-agent"));
        assert_eq!(got.name, "reviewer");
        assert_eq!(got.state, "working");
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

    /// A detector-written `kind` is reasserted from the current report.
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

    /// An explicit `kind` is preserved (§3.7).
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

    /// Garbage in the store does not block a clean write.
    #[test]
    fn compose_over_malformed_bytes_starts_fresh() {
        let bytes = compose(Some(b"}{ nonsense"), "claude", "claude", "idle", DETECTOR);
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.name, "claude");
        assert_eq!(got.state, "idle");
    }

    /// Recomposing an unchanged state is byte-identical (dedup).
    #[test]
    fn compose_is_stable_across_repeats() {
        let first = compose(None, "claude", "claude", "working", DETECTOR);
        let second = compose(Some(&first), "claude", "claude", "working", DETECTOR);
        assert_eq!(first, second, "a steady state must produce identical bytes");
    }

    // --- a contradicted explicit kind (phux-w7z2.45) ------------------------

    /// Two kinds with manifests.
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

    /// A declared `kind: claude` pane now running codex is contradicted.
    #[test]
    fn a_declared_kind_the_pane_no_longer_runs_is_contradicted() {
        let stored = br#"{"name":"claude","kind":"claude","state":"working"}"#;
        assert!(explicit_kind_is_contradicted(
            Some(stored),
            "codex",
            &detectable()
        ));
    }

    /// An open-vocabulary kind is never contradicted.
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

    /// Nothing to contradict (no record, no or empty kind, bad bytes):
    /// fail open.
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

    /// With no manifests loaded nothing can be contradicted.
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

    /// Withdrawal keeps name, kind, and session; drops state and attention.
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

    /// Withdrawing twice is byte-identical (no second broadcast).
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

    /// `Stream > Hook > Process > Screen`, strictly.
    #[test]
    fn the_evidence_ladder_runs_stream_hook_process_screen() {
        use EvidenceSource::{Hook, Process, Screen, Stream};

        let ladder = [Screen, Process, Hook, Stream];
        for pair in ladder.windows(2) {
            let (lower, upper) = (pair[0], pair[1]);
            assert!(
                upper.priority() > lower.priority(),
                "{upper:?} must outrank {lower:?}",
            );
            assert!(
                upper.outranks(lower),
                "{upper:?} may publish over {lower:?}"
            );
            assert!(
                !lower.outranks(upper),
                "{lower:?} must not publish over {upper:?}",
            );
        }
        assert!(Stream.outranks(Screen) && !Screen.outranks(Stream));
        assert!(
            Stream.outranks(Stream),
            "a source re-asserting its own claim is a refresh, not a usurpation",
        );
    }
}
