//! Integration tests for `phux_config::keybind`.

use std::collections::BTreeMap;

use phux_config::keybind::{
    Feed, KeyChord, KeybindError, Resolver, parse_chord, parse_chord_sequence,
};
use phux_config::{Action, KeybindingsCfg};
use phux_protocol::input::key::{ModSet, PhysicalKey};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn cfg(prefix: &str, prefix_table: &[(&str, &str)], global: &[(&str, &str)]) -> KeybindingsCfg {
    let mk_table = |entries: &[(&str, &str)]| -> BTreeMap<String, Action> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Action::Bare((*v).to_owned())))
            .collect()
    };
    KeybindingsCfg {
        prefix: prefix.to_owned(),
        prefix_table: mk_table(prefix_table),
        global: mk_table(global),
        ..KeybindingsCfg::default()
    }
}

const fn chord(mods: ModSet, key: PhysicalKey) -> KeyChord {
    KeyChord {
        modifiers: mods,
        key,
    }
}

// ---------------------------------------------------------------------------
// parse_chord
// ---------------------------------------------------------------------------

/// Table-driven chord grammar. Alias rows parse to the same chord as
/// their canonical spelling: bare uppercase implies shift (`A` == `S-a`),
/// `Esc`/`Escape` are aliases, `BackTab` is conventionally Shift+Tab,
/// and `A-` is an alias for the `M-` (meta) modifier.
#[test]
fn parse_chord_grammar() {
    let cases: &[(&str, ModSet, PhysicalKey)] = &[
        ("a", ModSet::empty(), PhysicalKey::A),
        ("A", ModSet::SHIFT, PhysicalKey::A),
        ("S-a", ModSet::SHIFT, PhysicalKey::A),
        ("C-c", ModSet::CTRL, PhysicalKey::C),
        (
            "M-S-Tab",
            ModSet::ALT.union(ModSet::SHIFT),
            PhysicalKey::Tab,
        ),
        ("F1", ModSet::empty(), PhysicalKey::F1),
        ("F12", ModSet::empty(), PhysicalKey::F12),
        ("Esc", ModSet::empty(), PhysicalKey::Escape),
        ("Escape", ModSet::empty(), PhysicalKey::Escape),
        ("BackTab", ModSet::SHIFT, PhysicalKey::Tab),
        ("M-x", ModSet::ALT, PhysicalKey::X),
        ("A-x", ModSet::ALT, PhysicalKey::X),
    ];
    for (spec, mods, key) in cases {
        let c = parse_chord(spec).unwrap_or_else(|e| panic!("{spec:?} should parse: {e:?}"));
        assert_eq!(c, chord(*mods, *key), "{spec:?}");
    }
}

// ---------------------------------------------------------------------------
// parse_chord_sequence
// ---------------------------------------------------------------------------

#[test]
fn parse_chord_sequence_single_and_two_chords() {
    let seq = parse_chord_sequence("C-b c").unwrap();
    assert_eq!(seq.0.len(), 2);
    assert_eq!(seq.0[0], chord(ModSet::CTRL, PhysicalKey::B));
    assert_eq!(seq.0[1], chord(ModSet::empty(), PhysicalKey::C));

    let seq = parse_chord_sequence("C-c").unwrap();
    assert_eq!(seq.0.len(), 1);
    assert_eq!(seq.0[0], chord(ModSet::CTRL, PhysicalKey::C));
}

// ---------------------------------------------------------------------------
// Error cases
// ---------------------------------------------------------------------------

#[test]
fn parse_errors_for_malformed_specs() {
    // Empty chord and empty sequence: syntax error at position 0.
    let err = parse_chord("").unwrap_err();
    assert!(
        matches!(err, KeybindError::Syntax { pos: 0, .. }),
        "expected Syntax {{ pos: 0, .. }}, got {err:?}"
    );
    let err = parse_chord_sequence("").unwrap_err();
    assert!(matches!(err, KeybindError::Syntax { pos: 0, .. }));

    // Unrecognized key name.
    let err = parse_chord("NotAKey").unwrap_err();
    assert!(
        matches!(err, KeybindError::UnknownKey(ref s) if s == "NotAKey"),
        "got {err:?}"
    );

    // "C-" — modifier with nothing after. split_modifier returns None
    // because there's no text past the dash, so the loop breaks and the
    // remaining "C-" is treated as a key token (which fails as unknown).
    // Either way, we get a parse error.
    let err = parse_chord("C-").unwrap_err();
    assert!(matches!(
        err,
        KeybindError::Syntax { .. } | KeybindError::UnknownKey(_)
    ));
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

#[test]
fn resolver_builds_from_three_bindings() {
    let c = cfg(
        "C-b",
        &[("c", "new-window"), ("d", "detach")],
        &[("M-q", "quit")],
    );
    let _r = Resolver::new(&c).expect("resolver builds");
}

#[test]
fn shipped_default_keybindings_build_a_resolver() {
    // Regression guard: every chord in the embedded default.toml must
    // parse and the prefix table must be unambiguous (phux-4li.18 added
    // the window bindings o/;/c/n/p/&/0-9/, alongside the pane ones).
    let cfg = phux_config::parse_with_defaults("", std::path::Path::new("<embedded default.toml>"))
        .expect("default config parses");
    Resolver::new(&cfg.keybindings).expect("default keybindings build a resolver");
}

#[test]
fn shipped_default_resolves_s_w_and_a_navigation() {
    // The nav redefinition: under the C-a leader, `s` opens the session
    // picker, `w` the grouped window picker, and `a` stays an alias for the
    // session picker (muscle memory). Resolve each against the *shipped*
    // default.toml so the bindings can't silently drift from the config.
    let cfg = phux_config::parse_with_defaults("", std::path::Path::new("<embedded default.toml>"))
        .expect("default config parses");
    let prefix = chord(ModSet::CTRL, PhysicalKey::A);

    let resolve = |key: PhysicalKey| {
        let mut r = Resolver::new(&cfg.keybindings).expect("resolver builds");
        assert_eq!(
            r.feed(prefix),
            Feed::Partial,
            "leader chord opens the table"
        );
        match r.feed(chord(ModSet::empty(), key)) {
            Feed::Resolved(a) => a.action,
            other => panic!("expected Resolved, got {other:?}"),
        }
    };

    assert_eq!(resolve(PhysicalKey::S), "session-picker", "C-a s");
    assert_eq!(resolve(PhysicalKey::W), "window-picker", "C-a w");
    assert_eq!(
        resolve(PhysicalKey::A),
        "session-picker",
        "C-a a stays a session-picker alias",
    );
}

#[test]
fn shipped_default_resolves_attention_navigation_and_preserves_fleet() {
    let cfg = phux_config::parse_with_defaults("", std::path::Path::new("<embedded default.toml>"))
        .expect("default config parses");
    let prefix = chord(ModSet::CTRL, PhysicalKey::A);
    let resolve = |key: PhysicalKey, mods: ModSet| {
        let mut resolver = Resolver::new(&cfg.keybindings).expect("resolver builds");
        assert_eq!(resolver.feed(prefix), Feed::Partial);
        match resolver.feed(chord(mods, key)) {
            Feed::Resolved(action) => action.action,
            other => panic!("expected Resolved, got {other:?}"),
        }
    };

    assert_eq!(
        resolve(PhysicalKey::Q, ModSet::empty()),
        "next-attention",
        "C-a q"
    );
    assert_eq!(
        resolve(PhysicalKey::Q, ModSet::SHIFT),
        "return-from-attention",
        "C-a Q",
    );
    assert_eq!(
        resolve(PhysicalKey::A, ModSet::SHIFT),
        "agent-fleet",
        "C-a A remains the fleet dashboard",
    );
}

#[test]
fn resolver_rejects_ambiguous_prefix_binding() {
    // A global "C-b" binding plus a prefix table makes the prefix chord
    // ambiguous: feeding "C-b" would both resolve the global AND open the
    // prefix table.
    let c = cfg(
        "C-b",
        &[("c", "new-window")],
        &[("C-b", "global-prefix-action")],
    );
    let err = Resolver::new(&c).unwrap_err();
    assert!(
        matches!(err, KeybindError::AmbiguousPrefix(ref s) if s == "C-b"),
        "got {err:?}"
    );
}

#[test]
fn resolver_walks_prefix_then_table_key() {
    let c = cfg("C-b", &[("c", "new-window"), ("d", "detach")], &[]);
    let mut r = Resolver::new(&c).unwrap();

    // First chord — prefix — should yield Partial.
    let f1 = r.feed(chord(ModSet::CTRL, PhysicalKey::B));
    assert_eq!(f1, Feed::Partial);

    // Second chord — 'c' — should resolve to "new-window".
    match r.feed(chord(ModSet::empty(), PhysicalKey::C)) {
        Feed::Resolved(action) => {
            assert_eq!(action.action, "new-window");
            assert!(action.args.is_empty());
        }
        other => panic!("expected Resolved, got {other:?}"),
    }

    // Now from a clean state, walk to 'd' → "detach".
    assert_eq!(r.feed(chord(ModSet::CTRL, PhysicalKey::B)), Feed::Partial);
    match r.feed(chord(ModSet::empty(), PhysicalKey::D)) {
        Feed::Resolved(action) => assert_eq!(action.action, "detach"),
        other => panic!("expected Resolved, got {other:?}"),
    }
}

#[test]
fn resolver_resolves_global_in_one_chord() {
    let c = cfg("C-b", &[], &[("M-q", "quit")]);
    let mut r = Resolver::new(&c).unwrap();

    match r.feed(chord(ModSet::ALT, PhysicalKey::Q)) {
        Feed::Resolved(a) => assert_eq!(a.action, "quit"),
        other => panic!("expected Resolved, got {other:?}"),
    }
}

#[test]
fn resolver_returns_nomatch_on_unrecognized_chord() {
    let c = cfg("C-b", &[("c", "new-window")], &[]);
    let mut r = Resolver::new(&c).unwrap();

    // An unrelated chord with no current state — should be NoMatch.
    let f = r.feed(chord(ModSet::ALT, PhysicalKey::Z));
    assert_eq!(f, Feed::NoMatch);
}

#[test]
fn resolver_partial_then_nomatch_resets() {
    let c = cfg("C-b", &[("c", "new-window")], &[]);
    let mut r = Resolver::new(&c).unwrap();

    // Walk into the prefix table.
    assert_eq!(r.feed(chord(ModSet::CTRL, PhysicalKey::B)), Feed::Partial);
    // Feed a chord that's not in the table — should be NoMatch and reset.
    assert_eq!(
        r.feed(chord(ModSet::empty(), PhysicalKey::X)),
        Feed::NoMatch
    );
    // After NoMatch, a fresh prefix walk works again.
    assert_eq!(r.feed(chord(ModSet::CTRL, PhysicalKey::B)), Feed::Partial);
}

#[test]
fn resolver_reset_clears_partial() {
    let c = cfg("C-b", &[("c", "new-window")], &[]);
    let mut r = Resolver::new(&c).unwrap();

    assert_eq!(r.feed(chord(ModSet::CTRL, PhysicalKey::B)), Feed::Partial);
    r.reset();
    // 'c' on its own (without prefix) is not registered as a binding, so
    // NoMatch.
    assert_eq!(
        r.feed(chord(ModSet::empty(), PhysicalKey::C)),
        Feed::NoMatch
    );
}

// ---------------------------------------------------------------------------
// Pending state (which-key popup trigger, phux-foz.2)
// ---------------------------------------------------------------------------

#[test]
fn pending_at_prefix_tracks_the_prefix_walk() {
    let c = cfg("C-b", &[("c", "new-window")], &[]);
    let mut r = Resolver::new(&c).unwrap();

    // At the root: nothing pending.
    assert!(!r.is_pending());
    assert!(!r.pending_at_prefix());

    // After the prefix chord: pending, and pending AT the prefix — the
    // which-key popup's arm condition.
    assert_eq!(r.feed(chord(ModSet::CTRL, PhysicalKey::B)), Feed::Partial);
    assert!(r.is_pending());
    assert!(r.pending_at_prefix());

    // A resolving continuation clears the pending state entirely.
    assert!(matches!(
        r.feed(chord(ModSet::empty(), PhysicalKey::C)),
        Feed::Resolved(_)
    ));
    assert!(!r.is_pending());
    assert!(!r.pending_at_prefix());
}

#[test]
fn pending_at_prefix_cleared_by_nomatch_and_reset() {
    let c = cfg("C-b", &[("c", "new-window")], &[]);
    let mut r = Resolver::new(&c).unwrap();

    // NoMatch clears it.
    assert_eq!(r.feed(chord(ModSet::CTRL, PhysicalKey::B)), Feed::Partial);
    assert_eq!(
        r.feed(chord(ModSet::empty(), PhysicalKey::X)),
        Feed::NoMatch
    );
    assert!(!r.is_pending());
    assert!(!r.pending_at_prefix());

    // Explicit reset (the Esc-cancel path) clears it too.
    assert_eq!(r.feed(chord(ModSet::CTRL, PhysicalKey::B)), Feed::Partial);
    assert!(r.pending_at_prefix());
    r.reset();
    assert!(!r.is_pending());
    assert!(!r.pending_at_prefix());
}

#[test]
fn pending_deeper_than_the_prefix_is_not_at_prefix() {
    // A nested `"c x"` prefix-table sequence: after `<prefix> c` the
    // resolver is pending but one level BELOW the prefix node, so the
    // which-key popup (which lists prefix-table continuations) must not
    // re-arm for that state.
    let c = cfg("C-b", &[("c x", "nested-action")], &[]);
    let mut r = Resolver::new(&c).unwrap();
    assert_eq!(r.feed(chord(ModSet::CTRL, PhysicalKey::B)), Feed::Partial);
    assert!(r.pending_at_prefix());
    assert_eq!(
        r.feed(chord(ModSet::empty(), PhysicalKey::C)),
        Feed::Partial
    );
    assert!(r.is_pending());
    assert!(!r.pending_at_prefix());
}

#[test]
fn pending_multichord_global_is_not_at_prefix() {
    // A multi-chord GLOBAL binding ("M-g g") goes Partial after its first
    // chord, but that pending state is not the prefix table's — the
    // which-key popup must stay quiet.
    let c = cfg("C-b", &[("c", "new-window")], &[("M-g g", "global-seq")]);
    let mut r = Resolver::new(&c).unwrap();
    assert_eq!(r.feed(chord(ModSet::ALT, PhysicalKey::G)), Feed::Partial);
    assert!(r.is_pending());
    assert!(!r.pending_at_prefix());
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

#[test]
fn resolver_debug_snapshot_representative_config() {
    let c = cfg(
        "C-b",
        &[("c", "new-window"), ("d", "detach")],
        &[("M-q", "quit")],
    );
    let r = Resolver::new(&c).unwrap();
    insta::assert_debug_snapshot!("resolver_representative", r);
}
