//! The registry grammar, held verbs, and the schema v1/v2 verb sets.

use super::{grant, host, set};
use phux_protocol::scope::{
    EffectiveScopeSet, ScopeError, ScopeGrammarError, ScopeGrant, Selector, TerminalScopeSet, Verb,
    Verbs,
};

#[test]
fn scope_grammar_parses_every_selector_form_and_rejects_partial_grants() {
    for (text, selector) in [
        ("*@global", Selector::Global),
        ("inventory@host", Selector::HostLocal),
        (
            "observe,input@host:devbox",
            Selector::HostSatellite(host("devbox")),
        ),
        ("bind@group:0", Selector::Group(0)),
        ("signal,create@group:4294967295", Selector::Group(u32::MAX)),
        (
            "inventory,observe,create,bind,input,signal@terminal:7",
            Selector::TerminalLocal(7),
        ),
        (
            "observe@terminal:devbox/12",
            Selector::TerminalSatellite(host("devbox"), 12),
        ),
        ("input@host:a:b", Selector::HostSatellite(host("a:b"))),
        (
            "observe@terminal:a/b/3",
            Selector::TerminalSatellite(host("a/b"), 3),
        ),
    ] {
        let parsed = ScopeGrant::parse(text).unwrap_or_else(|err| panic!("{text}: {err}"));
        assert_eq!(parsed.selector, selector, "{text}");
        let respelled = ScopeGrant::parse(&parsed.to_string()).unwrap();
        assert_eq!(respelled, parsed, "Display round-trips {text}");
    }
    assert_eq!(
        grant("inventory,observe,create,bind,input,signal,organize,terminate@global").to_string(),
        "*@global",
        "all eight verbs print as the wildcard"
    );
    assert_eq!(
        grant("inventory,observe,create,bind,input,signal@global").to_string(),
        "inventory,observe,create,bind,input,signal@global",
        "the six v1 verbs are no longer every verb"
    );
    assert_eq!(
        grant("input,observe@terminal:3").to_string(),
        "observe,input@terminal:3"
    );

    let refusals = [
        ("", ScopeGrammarError::Shape),
        ("terminal.control", ScopeGrammarError::Shape),
        ("@global", ScopeGrammarError::UnknownVerb),
        ("observe,@global", ScopeGrammarError::UnknownVerb),
        ("Observe@global", ScopeGrammarError::UnknownVerb),
        (" observe@global", ScopeGrammarError::UnknownVerb),
        ("observe,observe@global", ScopeGrammarError::RedundantVerb),
        ("*,observe@global", ScopeGrammarError::RedundantVerb),
        ("observe@", ScopeGrammarError::UnknownSelector),
        ("observe@everything", ScopeGrammarError::UnknownSelector),
        ("observe@global ", ScopeGrammarError::UnknownSelector),
        ("observe@host:", ScopeGrammarError::InvalidHost),
        ("observe@host:a\u{0}b", ScopeGrammarError::InvalidHost),
        ("observe@host:a\nb", ScopeGrammarError::InvalidHost),
        ("observe@group:", ScopeGrammarError::InvalidId),
        ("observe@group:07", ScopeGrammarError::InvalidId),
        ("observe@group:+7", ScopeGrammarError::InvalidId),
        ("observe@group:4294967296", ScopeGrammarError::InvalidId),
        ("observe@terminal:/3", ScopeGrammarError::InvalidHost),
        ("observe@terminal:devbox/", ScopeGrammarError::InvalidId),
    ];
    for (text, expected) in refusals {
        assert_eq!(ScopeGrant::parse(text), Err(expected), "{text:?}");
    }
    let long_host = format!("observe@host:{}", "h".repeat(256));
    assert_eq!(
        ScopeGrant::parse(&long_host),
        Err(ScopeGrammarError::InvalidHost)
    );

    // One bad string refuses the whole record: no partial grant.
    assert_eq!(
        TerminalScopeSet::parse_all(&["*@global", "observe@nowhere", "input@terminal:1"]),
        Err((1, Some(ScopeGrammarError::UnknownSelector)))
    );
    let whole = TerminalScopeSet::parse_all(&["input@terminal:1", "observe@terminal:1"]).unwrap();
    assert_eq!(whole.grants().len(), 1, "equal selectors merge");
    assert_eq!(whole.grants()[0].to_string(), "observe,input@terminal:1");

    // Refusals never echo the input.
    let secret = "-----BEGIN PRIVATE KEY-----MIIEvQ";
    let error = ScopeGrant::parse(secret).unwrap_err();
    assert!(!error.to_string().contains("MIIEvQ"));
    assert!(!format!("{error:?}").contains("MIIEvQ"));
}

/// `?signal` marks the grant's `SIGNAL` as held for approval (ADR-0128):
/// it parses, prints back, merges as a union of authority, reaches the
/// effective clauses, and never reaches the canonical image.
#[test]
fn held_signal_grammar_parses_merges_and_never_reaches_the_image() {
    let held = grant("input,?signal@terminal:3");
    assert_eq!(held.verbs, Verbs::of(&[Verb::Input, Verb::Signal]));
    assert_eq!(held.held, Verbs::of(&[Verb::Signal]));
    assert_eq!(held.to_string(), "input,?signal@terminal:3");
    assert_eq!(ScopeGrant::parse(&held.to_string()).unwrap(), held);
    assert_eq!(
        grant("inventory,observe,create,bind,input,?signal@global").to_string(),
        "inventory,observe,create,bind,input,?signal@global",
        "a held verb is never folded into the wildcard"
    );

    for (text, expected) in [
        ("?observe@global", ScopeGrammarError::UnholdableVerb),
        ("?*@global", ScopeGrammarError::RedundantVerb),
        ("signal,?signal@global", ScopeGrammarError::RedundantVerb),
        ("??signal@global", ScopeGrammarError::UnknownVerb),
        ("? signal@global", ScopeGrammarError::UnknownVerb),
    ] {
        assert_eq!(ScopeGrant::parse(text), Err(expected), "{text:?}");
    }

    // The same selector named held and un-held is un-held: a merge is a
    // union of authority, and a hold only ever restricts.
    let merged = set(&["?signal@global", "signal@global"]);
    assert_eq!(merged.grants()[0].held, Verbs::EMPTY);
    let still_held = set(&["?signal@global", "observe@global"]);
    assert_eq!(still_held.grants()[0].to_string(), "observe,?signal@global");

    // The image carries the verbs only; a decoded set holds nothing.
    let image = still_held.encode();
    assert_eq!(image, set(&["observe,signal@global"]).encode());
    assert_eq!(
        TerminalScopeSet::decode(&image).unwrap().grants()[0].held,
        Verbs::EMPTY
    );

    let effective = EffectiveScopeSet::unattenuated(&set(&["input,?signal@global"])).unwrap();
    assert!(effective.holds_any());
    assert!(
        effective.admits(Verb::Signal, |_| true),
        "a held verb reaches"
    );
    assert!(
        !effective.without_holds().admits(Verb::Signal, |_| true),
        "but not without a decision"
    );
    assert!(effective.without_holds().admits(Verb::Input, |_| true));

    // Held on either side of an intersection holds the clause.
    let requested = set(&["signal@terminal:3"]);
    let ceiling = set(&["?signal@global"]);
    let clauses = EffectiveScopeSet::intersect(&requested, &ceiling).unwrap();
    assert!(!clauses.without_holds().admits(Verb::Signal, |_| true));
    assert!(clauses.admits(Verb::Signal, |_| true));
}

/// Schema v2 adds `ORGANIZE = 0x40` and `TERMINATE = 0x80`; a v1 image still
/// refuses those bits, and a v1 grant reads with `SIGNAL` implying
/// `TERMINATE` and `CREATE` implying `ORGANIZE` (workload-auth §5, R07).
#[test]
fn schema_v2_verbs_and_the_v1_implication() {
    use phux_protocol::scope::{SCOPE_SCHEMA_VERSION, ScopeSchema};

    assert_eq!(SCOPE_SCHEMA_VERSION, 2);
    assert_eq!(ScopeSchema::from_version(1), Some(ScopeSchema::V1));
    assert_eq!(ScopeSchema::from_version(3), None);
    assert_eq!(Verb::Organize.bit(), 0x40);
    assert_eq!(Verb::Terminate.bit(), 0x80);
    assert_eq!(Verbs::KNOWN_BITS, 0xFF);

    // The new bits decode under v2 and are refused under v1.
    let organize_global = [0x00, 0x01, 0x00, 0x01, 0x00, 0x42];
    let v2 = TerminalScopeSet::decode(&organize_global).unwrap();
    assert_eq!(
        v2.grants()[0].verbs,
        Verbs::of(&[Verb::Observe, Verb::Organize])
    );
    assert_eq!(
        TerminalScopeSet::decode_versioned(&organize_global, ScopeSchema::V1),
        Err(ScopeError::UnknownVerbBits)
    );
    assert_eq!(
        grant("organize,terminate@global").verbs,
        Verbs::of(&[Verb::Organize, Verb::Terminate])
    );

    // A v1 image gains the implied verbs, and only those.
    let signal_create = [0x00, 0x01, 0x00, 0x01, 0x00, 0x24];
    let upgraded = TerminalScopeSet::decode_versioned(&signal_create, ScopeSchema::V1).unwrap();
    assert_eq!(
        upgraded.grants()[0].verbs,
        Verbs::of(&[Verb::Create, Verb::Signal, Verb::Organize, Verb::Terminate])
    );
    let observe = [0x00, 0x01, 0x00, 0x01, 0x00, 0x02];
    assert_eq!(
        TerminalScopeSet::decode_versioned(&observe, ScopeSchema::V1).unwrap(),
        TerminalScopeSet::decode(&observe).unwrap()
    );

    // v1 registry strings: the six names, `*` as those six, then the
    // implication; a held `?signal` implies nothing.
    let v1 = |text: &str| ScopeGrant::parse_versioned(text, ScopeSchema::V1);
    assert_eq!(v1("organize@global"), Err(ScopeGrammarError::UnknownVerb));
    assert_eq!(v1("*@global").unwrap().verbs, Verbs::of(&Verb::ALL));
    assert_eq!(
        v1("signal@host").unwrap().verbs,
        Verbs::of(&[Verb::Signal, Verb::Terminate])
    );
    let held = v1("?signal,create@host").unwrap();
    assert_eq!(
        held.verbs,
        Verbs::of(&[Verb::Create, Verb::Signal, Verb::Organize])
    );
    assert_eq!(held.held, Verbs::of(&[Verb::Signal]));
    assert_eq!(
        TerminalScopeSet::parse_all_versioned(&["signal@global"], ScopeSchema::V1)
            .unwrap()
            .grants()[0]
            .verbs,
        Verbs::of(&[Verb::Signal, Verb::Terminate])
    );
}
