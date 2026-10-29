use phux_protocol::ids::ResourceId as WireResourceId;

use super::{AgentRecordArbiter, IdentityOwnership};

mod compose;

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
