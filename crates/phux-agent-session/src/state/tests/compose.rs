//! Composing, contradicting, and withdrawing a stored record.

use super::super::{EvidenceSource, compose, explicit_kind_is_contradicted, withdraw_state};
use super::{DETECTOR, ownership_after};
use crate::agent_record::AgentRecordJson;
use phux_agent_rules::rules::{ManifestSpec, RuleSet};

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
        !explicit_kind_is_contradicted(Some(br#"{"name":"c","kind":"Claude"}"#), "claude", &rules),
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
