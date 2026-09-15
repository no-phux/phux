//! Canonical scope encoding and the registry grammar
//! (`docs/spec/workload-auth.md` §5).
//!
//! The goldens pin the §5 bytes, and every refusal rule the spec lists gets
//! its own image, so a lenient decoder cannot hide behind a normalizing
//! round trip.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use phux_protocol::scope::{
    EffectiveScopeSet, Host, MAX_GRANTS, ScopeError, ScopeGrammarError, ScopeGrant, Selector,
    TerminalScopeSet, Verb, Verbs,
};

fn host(name: &str) -> Host {
    Host::new(name).expect("valid host")
}

fn grant(text: &str) -> ScopeGrant {
    ScopeGrant::parse(text).expect("valid grant")
}

fn set(texts: &[&str]) -> TerminalScopeSet {
    TerminalScopeSet::from_grants(texts.iter().map(|text| grant(text))).expect("valid set")
}

#[test]
fn scope_set_canonical_bytes_match_spec_section_5_goldens() {
    let selectors: [(Selector, &[u8]); 6] = [
        (Selector::Global, &[0x00]),
        (Selector::HostLocal, &[0x01, 0x00]),
        (
            Selector::HostSatellite(host("ab")),
            &[0x01, 0x01, 0x00, 0x02, b'a', b'b'],
        ),
        (Selector::Group(7), &[0x02, 0x00, 0x00, 0x00, 0x07]),
        (
            Selector::TerminalLocal(9),
            &[0x03, 0x00, 0x00, 0x00, 0x00, 0x09],
        ),
        (
            Selector::TerminalSatellite(host("ab"), 3),
            &[0x03, 0x01, 0x00, 0x02, b'a', b'b', 0x00, 0x00, 0x00, 0x03],
        ),
    ];
    for (selector, bytes) in selectors {
        assert_eq!(selector.to_bytes(), bytes, "{selector}");
        assert_eq!(Selector::decode(bytes).unwrap(), selector, "{selector}");
    }

    // Grants sort by selector bytes, whatever order they were written in.
    let scopes = set(&["observe@terminal:9", "*@global"]);
    let golden = [
        0x00, 0x02, // grant_count
        0x00, 0x01, 0x00, 0x3F, // GLOBAL, all six verbs
        0x00, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x09, 0x02, // TERMINAL_LOCAL(9), OBSERVE
    ];
    assert_eq!(scopes.encode(), golden);
    assert_eq!(TerminalScopeSet::decode(&golden).unwrap(), scopes);

    // Equal selectors merge by OR-ing their verbs; the encoder emits one.
    let merged = set(&["observe@group:7", "bind@group:7"]);
    assert_eq!(
        merged.encode(),
        [0x00, 0x01, 0x00, 0x05, 0x02, 0x00, 0x00, 0x00, 0x07, 0x0A]
    );

    // An effective clause carries both selectors.
    let effective = EffectiveScopeSet::intersect(
        &set(&["bind,observe@group:7"]),
        &set(&["observe@terminal:9"]),
    )
    .unwrap();
    let clause_golden = [
        0x00, 0x01, // clause_count
        0x00, 0x05, 0x02, 0x00, 0x00, 0x00, 0x07, // requested GROUP(7)
        0x00, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x09, // ceiling TERMINAL_LOCAL(9)
        0x02, // OBSERVE
    ];
    assert_eq!(effective.encode(), clause_golden);
    assert_eq!(
        EffectiveScopeSet::decode(&clause_golden).unwrap(),
        effective
    );
}

#[test]
fn scope_set_rejects_unsorted_duplicate_unknown_bit_nonminimal_and_trailing() {
    let cases: [(&str, Vec<u8>, ScopeError); 12] = [
        (
            "unsorted",
            vec![
                0x00, 0x02, 0x00, 0x06, 0x03, 0x00, 0x00, 0x00, 0x00, 0x09, 0x02, 0x00, 0x01, 0x00,
                0x3F,
            ],
            ScopeError::Unsorted,
        ),
        (
            "duplicate",
            vec![0x00, 0x02, 0x00, 0x01, 0x00, 0x02, 0x00, 0x01, 0x00, 0x04],
            ScopeError::Duplicate,
        ),
        (
            "unknown verb bit",
            vec![0x00, 0x01, 0x00, 0x01, 0x00, 0x42],
            ScopeError::UnknownVerbBits,
        ),
        (
            "zero verbs",
            vec![0x00, 0x01, 0x00, 0x01, 0x00, 0x00],
            ScopeError::ZeroVerbs,
        ),
        (
            "non-minimal selector length",
            vec![0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x02],
            ScopeError::NonMinimalSelector,
        ),
        (
            "trailing byte",
            vec![0x00, 0x01, 0x00, 0x01, 0x00, 0x02, 0xFF],
            ScopeError::TrailingBytes,
        ),
        (
            "count larger than the grants present",
            vec![0x00, 0x02, 0x00, 0x01, 0x00, 0x02],
            ScopeError::Truncated,
        ),
        (
            "truncated selector",
            vec![0x00, 0x01, 0x00, 0x05, 0x02, 0x00],
            ScopeError::Truncated,
        ),
        (
            "unknown selector tag",
            vec![0x00, 0x01, 0x00, 0x01, 0x04, 0x02],
            ScopeError::UnknownSelectorTag,
        ),
        (
            "unknown subtype",
            vec![0x00, 0x01, 0x00, 0x02, 0x01, 0x02, 0x02],
            ScopeError::UnknownSubtype,
        ),
        (
            "empty host",
            vec![0x00, 0x01, 0x00, 0x04, 0x01, 0x01, 0x00, 0x00, 0x02],
            ScopeError::InvalidHost,
        ),
        (
            "more than 64 grants",
            vec![0x00, 0x41],
            ScopeError::TooManyGrants,
        ),
    ];
    for (name, image, expected) in cases {
        assert_eq!(
            TerminalScopeSet::decode(&image),
            Err(expected),
            "{name}: {image:02x?}"
        );
    }

    // A valid image with one trailing byte is refused, not trimmed, and the
    // same holds for an effective set.
    let effective = EffectiveScopeSet::intersect(&set(&["*@global"]), &set(&["*@global"])).unwrap();
    let mut image = effective.encode();
    image.push(0x00);
    assert_eq!(
        EffectiveScopeSet::decode(&image),
        Err(ScopeError::TrailingBytes)
    );
    assert_eq!(
        EffectiveScopeSet::decode(&[0x00, 0x41]),
        Err(ScopeError::TooManyClauses)
    );

    // The encoder refuses more than 64 distinct selectors outright.
    let too_many =
        (0..=u32::try_from(MAX_GRANTS).unwrap()).map(|id| grant(&format!("observe@terminal:{id}")));
    assert_eq!(
        TerminalScopeSet::from_grants(too_many),
        Err(ScopeError::TooManyGrants)
    );
}

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
        grant("inventory,observe,create,bind,input,signal@global").to_string(),
        "*@global",
        "all six verbs print as the wildcard"
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

#[test]
fn effective_set_keeps_conjunctive_clauses_and_never_flattens_group_to_terminal() {
    let requested = set(&["bind,observe@group:7"]);
    let ceiling = set(&["observe,input@terminal:9"]);
    let effective = EffectiveScopeSet::intersect(&requested, &ceiling).unwrap();
    assert_eq!(effective.clauses().len(), 1);
    let clause = &effective.clauses()[0];
    assert_eq!(clause.requested, Selector::Group(7), "the Group survives");
    assert_eq!(clause.ceiling, Selector::TerminalLocal(9));
    assert_eq!(clause.verbs, Verbs::of(&[Verb::Observe]));

    // Terminal 9 while it is in Group 7: both selectors contain it.
    let in_group =
        |selector: &Selector| matches!(selector, Selector::Group(7) | Selector::TerminalLocal(9));
    assert!(effective.admits(Verb::Observe, in_group));
    assert!(
        !effective.admits(Verb::Input, in_group),
        "INPUT is not in both"
    );
    assert!(
        !effective.admits(Verb::Bind, in_group),
        "BIND is not in both"
    );

    // The moment Terminal 9 leaves Group 7 the clause stops matching: the
    // Group ceiling was never flattened into a bare Terminal grant.
    let moved_out = |selector: &Selector| matches!(selector, Selector::TerminalLocal(9));
    assert!(!effective.admits(Verb::Observe, moved_out));

    // Selectors that can never share a subject produce no clause.
    let disjoint =
        EffectiveScopeSet::intersect(&set(&["*@terminal:1"]), &set(&["*@terminal:2"])).unwrap();
    assert!(disjoint.is_empty());
    let other_host =
        EffectiveScopeSet::intersect(&set(&["*@host:a"]), &set(&["*@terminal:b/1"])).unwrap();
    assert!(other_host.is_empty());

    // A set intersected with itself keeps its own grants as clauses.
    let own = set(&["*@global", "observe@terminal:5"]);
    let effective = EffectiveScopeSet::intersect(&own, &own).unwrap();
    assert!(effective.clauses().iter().any(|clause| {
        clause.requested == Selector::Global
            && clause.ceiling == Selector::Global
            && clause.verbs == phux_protocol::scope::all_verbs()
    }));
}

#[test]
fn unattenuated_set_is_one_clause_per_grant_and_grants_the_same() {
    let ceiling = set(&[
        "bind,observe@group:7",
        "observe,input@terminal:9",
        "*@host:a",
    ]);
    let diagonal = EffectiveScopeSet::unattenuated(&ceiling).unwrap();
    assert_eq!(diagonal.clauses().len(), 3);
    assert!(
        diagonal
            .clauses()
            .iter()
            .all(|clause| clause.requested == clause.ceiling)
    );
    // Canonical: it decodes as its own encoding.
    assert_eq!(
        EffectiveScopeSet::decode(&diagonal.encode()).unwrap(),
        diagonal
    );
    let full = EffectiveScopeSet::intersect(&ceiling, &ceiling).unwrap();
    let subjects: [&dyn Fn(&Selector) -> bool; 3] = [
        &|s| matches!(s, Selector::Group(7) | Selector::TerminalLocal(9)),
        &|s| matches!(s, Selector::TerminalLocal(9)),
        &|s| matches!(s, Selector::HostSatellite(host) if host.as_str() == "a"),
    ];
    for contains in subjects {
        for verb in Verb::ALL {
            assert_eq!(
                diagonal.admits(verb, contains),
                full.admits(verb, contains),
                "{verb:?}"
            );
        }
    }
    // A full ceiling fits, where its self-intersection may not.
    let mut wide: Vec<String> = (0..63).map(|n| format!("observe@host:h{n}")).collect();
    wide.push("*@global".to_owned());
    let wide = TerminalScopeSet::parse_all(&wide).unwrap();
    assert_eq!(
        EffectiveScopeSet::unattenuated(&wide)
            .unwrap()
            .clauses()
            .len(),
        64
    );
}
