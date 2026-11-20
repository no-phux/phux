//! Composing, contradicting, and withdrawing a stored record.

use super::super::{EvidenceSource, compose, explicit_kind_is_contradicted, withdraw_state};
use super::{DETECTOR, ownership_after};
use crate::agent_record::AgentRecordJson;
use phux_agent_rules::rules::{ManifestSpec, RuleSet};

// --- compose ----------------------------------------------------------

/// `compose`, decoded back into the record it wrote.
fn composed(
    existing: Option<&[u8]>,
    kind: &str,
    name: &str,
    state: &str,
    ownership: super::super::IdentityOwnership,
) -> AgentRecordJson {
    AgentRecordJson::decode(&compose(existing, kind, name, state, ownership)).expect("decodes")
}

/// An identity-only SET that beat the arbiter bit is merged, not
/// clobbered.
#[test]
fn compose_preserves_an_identity_only_name_without_ownership_bits() {
    let existing = br#"{"name":"reviewer","session":"fleet-7"}"#;
    let got = composed(Some(existing), "claude", "claude", "blocked", DETECTOR);
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
    let got = composed(Some(existing), "claude", "claude", "working", DETECTOR);
    assert_eq!(got.kind.as_deref(), Some("my-agent"));
    assert_eq!(got.name, "reviewer");
    assert_eq!(got.state, "working");
}

#[test]
fn compose_preserves_a_declared_attention() {
    let existing = br#"{"name":"a","attention":"high"}"#;
    let got = composed(
        Some(existing),
        "claude",
        "claude",
        "idle",
        ownership_after(existing),
    );
    assert_eq!(got.attention.as_deref(), Some("high"));
    assert_eq!(got.state, "idle");
}

#[test]
fn compose_fills_a_missing_name_and_kind() {
    let existing = br#"{"name":"","session":"s"}"#;
    let got = composed(
        Some(existing),
        "claude",
        "claude",
        "idle",
        ownership_after(existing),
    );
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
    let got = composed(Some(existing), "codex", "codex", "idle", DETECTOR);
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
    let got = composed(Some(existing), "claude", "claude", "working", owned);
    assert_eq!(got.kind.as_deref(), Some("my-agent"));
    assert_eq!(got.name, "reviewer");
    assert_eq!(got.state, "working");
}

/// Garbage in the store does not block a clean write.
#[test]
fn compose_over_malformed_bytes_starts_fresh() {
    let got = composed(Some(b"}{ nonsense"), "claude", "claude", "idle", DETECTOR);
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

/// Label, stored record, the kind the pane runs, the rules, and whether the
/// stored kind is contradicted.
type Contradiction<'a> = (&'a str, Option<&'a [u8]>, &'a str, &'a RuleSet, bool);

/// A declared kind is contradicted only when the detector could have derived
/// it and the pane now runs another; everything else fails open.
#[test]
fn an_explicit_kind_is_contradicted_only_by_a_derivable_other_kind() {
    let rules = detectable();
    let cases: [Contradiction<'_>; 7] = [
        (
            "declared claude, now running codex",
            Some(br#"{"name":"claude","kind":"claude","state":"working"}"#),
            "codex",
            &rules,
            true,
        ),
        // A label the detector could never have produced is not a claim it
        // can falsify.
        (
            "open-vocabulary kind",
            Some(br#"{"name":"reviewer","kind":"my-agent"}"#),
            "claude",
            &rules,
            false,
        ),
        (
            "the kind the pane runs",
            Some(br#"{"name":"claude","kind":"claude","state":"idle"}"#),
            "claude",
            &rules,
            false,
        ),
        (
            "the same kind in another case",
            Some(br#"{"name":"c","kind":"Claude"}"#),
            "claude",
            &rules,
            false,
        ),
        ("no record", None, "codex", &rules, false),
        ("no kind", Some(br#"{"name":"x"}"#), "codex", &rules, false),
        (
            "empty kind",
            Some(br#"{"name":"x","kind":""}"#),
            "codex",
            &rules,
            false,
        ),
    ];
    for (label, stored, running, rules, contradicted) in cases {
        assert_eq!(
            explicit_kind_is_contradicted(stored, running, rules),
            contradicted,
            "{label}"
        );
    }
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

/// `Stream > Hook > Screen`, strictly.
#[test]
fn the_evidence_ladder_runs_stream_hook_screen() {
    use EvidenceSource::{Hook, Screen, Stream};

    let ladder = [Screen, Hook, Stream];
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
