//! Integration tests for `phux_config::keybind`.

#![allow(clippy::panic, reason = "test helpers panic on bad fixtures")]

use std::collections::BTreeMap;

use phux_config::keybind::{
    Feed, KeyChord, KeybindError, Resolver, parse_chord, parse_chord_sequence,
};
use phux_config::{Action, KeybindingsCfg};
use phux_protocol::input::key::{ModSet, PhysicalKey};

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

fn ck(s: &str) -> KeyChord {
    parse_chord(s).unwrap_or_else(|e| panic!("{s:?} should parse: {e:?}"))
}

/// Chord grammar, including aliases: bare uppercase implies Shift,
/// `Esc`/`Escape`, `BackTab` is Shift+Tab, and `A-` means `M-`.
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
        assert_eq!(ck(spec), chord(*mods, *key), "{spec:?}");
    }
    assert_eq!(parse_chord_sequence("C-b c").unwrap(), [ck("C-b"), ck("c")]);
}

#[test]
fn parse_errors_for_malformed_specs() {
    assert!(matches!(
        parse_chord(""),
        Err(KeybindError::Syntax { pos: 0, .. })
    ));
    assert!(matches!(
        parse_chord_sequence(""),
        Err(KeybindError::Syntax { pos: 0, .. })
    ));
    assert!(matches!(
        parse_chord("NotAKey"),
        Err(KeybindError::UnknownKey(ref s)) if s == "NotAKey"
    ));
    assert!(matches!(
        parse_chord("q-"),
        Err(KeybindError::Syntax { pos: 1, .. })
    ));
    // Error positions are within the whole sequence.
    assert!(matches!(
        parse_chord_sequence("C-b q-"),
        Err(KeybindError::Syntax { pos: 5, .. })
    ));
}

#[test]
fn resolver_rejects_a_global_binding_on_the_prefix_chord() {
    let c = cfg("C-b", &[("c", "new-window")], &[("C-b", "kill-pane")]);
    assert!(matches!(
        Resolver::new(&c),
        Err(KeybindError::AmbiguousPrefix(ref s)) if s == "C-b"
    ));
}

/// Prefix walks, one-chord globals, mismatches, and resets.
#[test]
fn resolver_walks_prefix_tables_and_globals() {
    let c = cfg(
        "C-b",
        &[("c", "new-window"), ("d", "detach")],
        &[("M-q", "toggle-zoom")],
    );
    let mut r = Resolver::new(&c).unwrap();
    let resolved = |feed: Feed| match feed {
        Feed::Resolved(action) => action.action,
        other => panic!("expected Resolved, got {other:?}"),
    };

    assert_eq!(r.feed(ck("C-b")), Feed::Partial);
    assert_eq!(resolved(r.feed(ck("c"))), "new-window");
    assert_eq!(r.feed(ck("C-b")), Feed::Partial);
    assert_eq!(resolved(r.feed(ck("d"))), "detach");
    assert_eq!(resolved(r.feed(ck("M-q"))), "toggle-zoom");

    assert_eq!(r.feed(ck("M-z")), Feed::NoMatch);
    assert_eq!(r.feed(ck("C-b")), Feed::Partial);
    assert_eq!(r.feed(ck("x")), Feed::NoMatch, "mismatch resets");
    assert_eq!(r.feed(ck("C-b")), Feed::Partial);
    r.reset();
    assert_eq!(r.feed(ck("c")), Feed::NoMatch, "reset returns to the root");
}

/// `pending_at_prefix` (the which-key trigger) holds only right after the
/// prefix chord: not at the root, not after resolve / mismatch / reset, not
/// deeper in a nested sequence, and not mid-way through a global sequence.
#[test]
fn pending_at_prefix_tracks_only_the_prefix_state() {
    let c = cfg(
        "C-b",
        &[("c", "new-window"), ("n x", "kill-window")],
        &[("M-g g", "next-window")],
    );
    let mut r = Resolver::new(&c).unwrap();
    let state = |r: &Resolver| (r.is_pending(), r.pending_at_prefix());

    assert_eq!(state(&r), (false, false));
    assert_eq!(r.feed(ck("C-b")), Feed::Partial);
    assert_eq!(state(&r), (true, true));
    assert!(matches!(r.feed(ck("c")), Feed::Resolved(_)));
    assert_eq!(state(&r), (false, false));

    r.feed(ck("C-b"));
    r.feed(ck("x"));
    assert_eq!(state(&r), (false, false), "cleared by NoMatch");

    r.feed(ck("C-b"));
    r.reset();
    assert_eq!(state(&r), (false, false), "cleared by reset");

    r.feed(ck("C-b"));
    assert_eq!(r.feed(ck("n")), Feed::Partial);
    assert_eq!(state(&r), (true, false), "deeper than the prefix");
    r.reset();

    assert_eq!(r.feed(ck("M-g")), Feed::Partial);
    assert_eq!(state(&r), (true, false), "a global sequence");
}
