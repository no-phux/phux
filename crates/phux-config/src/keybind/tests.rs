//! Chord parsing and resolver tests.

use super::*;

fn ck(s: &str) -> KeyChord {
    parse_chord(s).expect("parses")
}

fn cfg_from(toml: &str) -> crate::Config {
    crate::parse_str(toml, std::path::Path::new("test.toml")).expect("test config parses")
}

fn shipped() -> crate::Config {
    cfg_from(crate::DEFAULT_CONFIG_TOML)
}

fn resolves(resolver: &mut Resolver, chords: &[&str]) -> Option<ResolvedAction> {
    let (last, init) = chords.split_last()?;
    for chord in init {
        assert_eq!(resolver.feed(ck(chord)), Feed::Partial, "{chord}");
    }
    match resolver.feed(ck(last)) {
        Feed::Resolved(action) => Some(action),
        _ => None,
    }
}

/// Punctuation maps to US-layout physical keys; shifted glyphs carry
/// Shift; non-ASCII stays unknown.
#[test]
fn punctuation_parses_to_physical_keys() {
    let none = ModSet::empty();
    for (spec, modifiers, key) in [
        ("|", ModSet::SHIFT, K::Backslash),
        ("S-\\", ModSet::SHIFT, K::Backslash),
        ("-", none, K::Minus),
        ("/", none, K::Slash),
        ("=", none, K::Equal),
        ("'", none, K::Quote),
        ("\"", ModSet::SHIFT, K::Quote),
        (";", none, K::Semicolon),
        ("C-/", ModSet::CTRL, K::Slash),
        ("F24", none, K::F24),
        ("7", none, K::Digit7),
    ] {
        assert_eq!(ck(spec), KeyChord { modifiers, key }, "{spec}");
    }
    for bad in ["→", "F0", "F25", "Ctrl-a", "C-"] {
        assert!(parse_chord(bad).is_err(), "{bad}");
    }
}

/// The shipped bindings build cleanly, bind no chord twice even under
/// different spellings (`G` vs `S-g`), and keep the chords the TUI doc
/// (§5.3) promises.
#[test]
fn shipped_keybindings_are_clean_and_documented() {
    let keys = shipped().keybindings;
    let mut resolver = Resolver::new(&keys).expect("no overlap");
    let prefix = ck(&keys.prefix);
    let rows = keys
        .global
        .keys()
        .map(|text| (Vec::new(), text))
        .chain(keys.prefix_table.keys().map(|text| (vec![prefix], text)));
    let mut seen: BTreeMap<Vec<KeyChord>, &String> = BTreeMap::new();
    for (mut seq, text) in rows {
        seq.extend(parse_chord_sequence(text).expect("default chord parses"));
        if let Some(earlier) = seen.insert(seq, text) {
            panic!("default bindings `{earlier}` and `{text}` share a chord");
        }
    }

    for (chord, action) in [
        ("s", "session-picker"),
        ("w", "window-picker"),
        ("q", "next-attention"),
        ("Q", "return-from-attention"),
        ("A", "agent-fleet"),
        ("G", "go-to-directory"),
        ("F", "find-path"),
    ] {
        let got = resolves(&mut resolver, &["C-a", chord]).expect(chord);
        assert_eq!(got.action, action, "C-a {chord}");
    }
    for (chord, direction) in [("H", "left"), ("J", "down"), ("K", "up"), ("L", "right")] {
        let got = resolves(&mut resolver, &["C-a", chord]).expect(chord);
        assert_eq!(got.action, "resize-pane");
        assert_eq!(got.args.get("direction"), Some(&direction.into()));
        assert_eq!(got.args.get("amount"), Some(&5.into()));
    }
}

/// Each lenient-build degradation disables exactly the offending
/// binding and keeps everything else reachable.
#[test]
fn lenient_build_skips_only_the_offending_binding() {
    let cases: &[(&str, &str, &[&str], &str)] = &[
        (
            "[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\nd = \"detach\"\n",
            "q-",
            &["C-a", "d"],
            "detach",
        ),
        (
            "[keybindings]\nprefix = \"Ctrl-a\"\n[keybindings.prefix-table]\nd = \"detach\"\n",
            "Ctrl-a",
            &["C-a", "d"],
            "detach",
        ),
        (
            "[keybindings.prefix-table]\n\"c\" = \"new-window\"\n\"c x\" = \"kill-pane\"\n",
            "c x",
            &["C-a", "c"],
            "new-window",
        ),
        (
            "[keybindings.global]\n\"C-a\" = \"kill-pane\"\n[keybindings.prefix-table]\nd = \"detach\"\n",
            "C-a",
            &["C-a", "d"],
            "detach",
        ),
        (
            "[keybindings.prefix-table]\nc = \"new-windoww\"\nd = \"detach\"\n",
            "c",
            &["C-a", "d"],
            "detach",
        ),
    ];
    for &(toml, bad, chords, action) in cases {
        let (mut resolver, diags) = Resolver::new_lenient(&cfg_from(toml).keybindings);
        let named: Vec<&str> = diags.iter().map(|d| d.binding.as_str()).collect();
        assert_eq!(named, [bad], "{toml}");
        assert_eq!(
            resolves(&mut resolver, chords).map(|a| a.action),
            Some(action.to_owned()),
            "{toml}"
        );
    }

    // The bad binding is dead, not misrouted.
    let cfg = cfg_from(cases[0].0);
    let (mut resolver, _) = Resolver::new_lenient(&cfg.keybindings);
    assert_eq!(resolver.feed(ck("C-a")), Feed::Partial);
    assert_eq!(resolver.feed(ck("q")), Feed::NoMatch);
}

/// A binding to an action no dispatcher knows is a diagnostic that names
/// the nearest real action, so a strict build (reload) refuses it rather
/// than installing a chord that silently does nothing.
#[test]
fn an_unknown_action_is_a_diagnostic_with_a_suggestion() {
    let cfg = cfg_from("[keybindings.prefix-table]\nc = \"new-windoww\"\n");
    let err = Resolver::new(&cfg.keybindings).expect_err("unknown action refused");
    assert_eq!(
        err.to_string(),
        "unknown action `new-windoww` (did you mean `new-window`?)"
    );
    let cfg = cfg_from("[keybindings.prefix-table]\nc = \"zzzzzzzzzz\"\n");
    let err = Resolver::new(&cfg.keybindings).expect_err("unknown action refused");
    assert_eq!(err.to_string(), "unknown action `zzzzzzzzzz`");
}

/// Strict `new` fails with exactly the first lenient diagnostic.
#[test]
fn strict_new_errors_with_the_first_lenient_diagnostic() {
    let cfg = cfg_from(
        "[keybindings]\nprefix = \"Ctrl-a\"\n[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\n",
    );
    let (_, diags) = Resolver::new_lenient(&cfg.keybindings);
    assert_eq!(diags.len(), 2);
    assert_eq!(Resolver::new(&cfg.keybindings).unwrap_err(), diags[0].error);
}
