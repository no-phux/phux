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
    }

    /// Note an explicit `DELETE_METADATA`. The declaration is withdrawn, the
    /// human's identity is gone from the store with the rest of the record,
    /// and the detector resumes full ownership.
    pub(crate) fn note_explicit_delete(&mut self, terminal: &WireTerminalId) {
        self.declared.remove(terminal);
        self.detector_owned.remove(terminal);
        self.explicit_identity.remove(terminal);
    }

    /// Whether the stored record carries identity a human authored, in which
    /// case the detector may withdraw its `state` but must not `DELETE` the
    /// key.
    pub(crate) fn has_explicit_identity(&self, terminal: &WireTerminalId) -> bool {
        self.explicit_identity.contains(terminal)
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
    }
}

/// Compose the record the detector should write, preserving every field an
/// explicit (state-less) declaration supplied.
///
/// `existing` is the currently-stored value, if any. `name` and `kind` come
/// from the detector's manifest but yield to a human-declared `name`: if
/// someone called their pane "reviewer", it stays "reviewer" while the
/// detector tracks its state. `session` is carried through untouched.
///
/// `attention` is deliberately NOT set by the detector — `docs/spec/L3.md`
/// §3.7 already derives it from `state` when absent — but a declared one is
/// preserved.
pub(crate) fn compose(existing: Option<&[u8]>, kind: &str, name: &str, state: &str) -> Vec<u8> {
    let prior = existing.and_then(AgentRecordJson::decode);
    let record = match prior {
        Some(mut prior) => {
            if prior.name.is_empty() {
                prior.name.clear();
                prior.name.push_str(name);
            }
            if prior.kind.is_none() {
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

/// Withdraw the detector's `state` from a stored record, preserving every
/// field a human authored.
///
/// The counterpart to [`compose`], for the retract path when the record also
/// carries human-authored identity (see
/// [`AgentRecordArbiter::has_explicit_identity`]). `DELETE`ing the key there
/// would wipe the name, session and attention the human chose — and they are
/// unrecoverable, because restarting the agent only re-creates the detector's
/// own view of it. So the detector withdraws the one field it owns, and the
/// state falls back to the vocabulary's `unknown`: the agent is gone, and a
/// dead process must not lie about being `working`.
///
/// `None` when there is no stored record to rewrite.
pub(crate) fn withdraw_state(existing: Option<&[u8]>) -> Option<Vec<u8>> {
    let mut record = AgentRecordJson::decode(existing?)?;
    record.state.clear();
    record.state.push_str("unknown");
    Some(record.encode())
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use phux_protocol::ids::TerminalId as WireTerminalId;

    use super::{AgentRecordArbiter, compose, withdraw_state};
    use crate::agent_detect::record::AgentRecordJson;

    fn terminal(id: u32) -> WireTerminalId {
        WireTerminalId::new(id)
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

    // --- compose ----------------------------------------------------------

    #[test]
    fn compose_from_nothing_writes_the_detector_view() {
        let bytes = compose(None, "claude", "claude", "working");
        assert_eq!(
            String::from_utf8(bytes).expect("utf8"),
            r#"{"name":"claude","kind":"claude","state":"working"}"#
        );
    }

    /// The field-for-field preservation the ADR promises.
    #[test]
    fn compose_preserves_an_identity_only_declaration() {
        let existing = br#"{"name":"reviewer","kind":"claude","session":"fleet-7"}"#;
        let bytes = compose(Some(existing), "claude", "claude", "blocked");
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.name, "reviewer", "the human's name survives");
        assert_eq!(got.session.as_deref(), Some("fleet-7"), "and their label");
        assert_eq!(got.state, "blocked", "the detector supplies only `state`");
    }

    #[test]
    fn compose_preserves_a_declared_attention() {
        let existing = br#"{"name":"a","attention":"high"}"#;
        let bytes = compose(Some(existing), "claude", "claude", "idle");
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.attention.as_deref(), Some("high"));
        assert_eq!(got.state, "idle");
    }

    #[test]
    fn compose_fills_a_missing_name_and_kind() {
        let existing = br#"{"name":"","session":"s"}"#;
        let bytes = compose(Some(existing), "claude", "claude", "idle");
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.name, "claude");
        assert_eq!(got.kind.as_deref(), Some("claude"));
        assert_eq!(got.session.as_deref(), Some("s"));
    }

    /// Garbage in the store must not stop the detector from writing a clean
    /// record over it.
    #[test]
    fn compose_over_malformed_bytes_starts_fresh() {
        let bytes = compose(Some(b"}{ nonsense"), "claude", "claude", "idle");
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.name, "claude");
        assert_eq!(got.state, "idle");
    }

    /// The dedup contract: recomposing an unchanged state yields byte-identical
    /// output, which is what makes `metadata_set` suppress the broadcast.
    #[test]
    fn compose_is_stable_across_repeats() {
        let first = compose(None, "claude", "claude", "working");
        let second = compose(Some(&first), "claude", "claude", "working");
        assert_eq!(first, second, "a steady state must produce identical bytes");
    }

    // --- withdraw_state ----------------------------------------------------

    /// The retract path for a record a human named. Their fields survive; the
    /// one field the detector owns is withdrawn — and a dead agent must not
    /// leave a `working` badge spinning behind it.
    #[test]
    fn withdraw_state_keeps_the_human_fields_and_drops_the_detectors() {
        let stored = br#"{"name":"reviewer","kind":"claude","state":"working","attention":"high","session":"fleet-7"}"#;
        let bytes = withdraw_state(Some(stored)).expect("a record to rewrite");
        let got = AgentRecordJson::decode(&bytes).expect("decodes");
        assert_eq!(got.name, "reviewer", "the human's name survives the agent");
        assert_eq!(got.session.as_deref(), Some("fleet-7"));
        assert_eq!(got.attention.as_deref(), Some("high"));
        assert_eq!(got.state, "unknown", "and the detector's verdict is gone");
    }

    #[test]
    fn withdraw_state_has_nothing_to_rewrite_without_a_record() {
        assert!(withdraw_state(None).is_none());
        assert!(withdraw_state(Some(b"}{ nonsense")).is_none());
    }
}
