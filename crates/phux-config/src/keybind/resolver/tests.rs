//! Key modes: the resolver's mode state machine, its diagnostics, and how
//! the layers settle mode tables.

use super::super::{KeybindError, parse_chord};
use super::{EXIT_MODE_ACTION, Feed, Resolver};

fn ck(s: &str) -> super::KeyChord {
    parse_chord(s).expect("parses")
}

/// The config `user` layered over the shipped defaults.
fn layered(user: &str) -> crate::Config {
    crate::parse_with_defaults(user, std::path::Path::new("config.toml"))
        .expect("layered config parses")
}

fn resolver(user: &str) -> Resolver {
    let (resolver, diagnostics) = Resolver::new_lenient(&layered(user).keybindings);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    resolver
}

/// The action name `chord` resolves to, `None` for anything else.
fn action(resolver: &mut Resolver, chord: &str) -> Option<String> {
    match resolver.feed(ck(chord)) {
        Feed::Resolved(action) => Some(action.action),
        _ => None,
    }
}

#[test]
fn the_shipped_resize_mode_resizes_without_the_prefix_until_esc() {
    let mut r = resolver("");
    assert_eq!(r.feed(ck("C-a")), Feed::Partial);
    let Feed::Resolved(enter) = r.feed(ck("R")) else {
        panic!("prefix R enters a mode");
    };
    assert_eq!(enter.action, "enter-mode");
    assert_eq!(enter.args["name"].as_str(), Some("resize"));

    assert!(r.enter_mode("resize", false));
    assert_eq!(r.active_mode(), Some("resize"));
    let Feed::Resolved(step) = r.feed(ck("h")) else {
        panic!("h resizes in the mode");
    };
    assert_eq!(step.action, "resize-pane");
    assert_eq!(step.args["amount"].as_integer(), Some(1));
    assert_eq!(action(&mut r, "Left").as_deref(), Some("resize-pane"));
    assert_eq!(action(&mut r, "L").as_deref(), Some("resize-pane"));
    // Exclusive: an unbound key, the prefix included, goes nowhere.
    assert_eq!(r.feed(ck("x")), Feed::Swallowed);
    assert_eq!(r.feed(ck("C-a")), Feed::Swallowed);
    assert_eq!(action(&mut r, "Enter").as_deref(), Some(EXIT_MODE_ACTION));
    assert_eq!(action(&mut r, "Esc").as_deref(), Some(EXIT_MODE_ACTION));
    assert_eq!(r.active_mode(), Some("resize"), "the dispatcher leaves");

    assert!(r.exit_mode());
    assert!(!r.exit_mode(), "already out");
    assert_eq!(r.feed(ck("h")), Feed::NoMatch, "h is the pane's again");
    assert_eq!(r.feed(ck("Esc")), Feed::NoMatch);
}

#[test]
fn a_non_exclusive_mode_passes_unbound_keys_on() {
    let mut r = resolver(
        "[keybindings.modes.nav]\nn = \"next-pane\"\n\
         [keybindings.global]\n\"M-z\" = \"toggle-zoom\"\n",
    );
    assert!(r.enter_mode("nav", false));
    assert_eq!(action(&mut r, "n").as_deref(), Some("next-pane"));
    assert_eq!(r.feed(ck("x")), Feed::NoMatch, "x reaches the pane");
    assert_eq!(action(&mut r, "M-z").as_deref(), Some("toggle-zoom"));
    assert_eq!(r.feed(ck("C-a")), Feed::Partial, "the prefix still works");
    assert!(r.pending_at_prefix());
    assert_eq!(action(&mut r, "z").as_deref(), Some("toggle-zoom"));
    assert_eq!(r.active_mode(), Some("nav"));
}

#[test]
fn once_leaves_after_the_first_mode_binding_only() {
    let mut r = resolver("[keybindings.modes.nav]\nn = \"next-pane\"\n");
    assert!(r.enter_mode("nav", true));
    assert_eq!(r.feed(ck("x")), Feed::NoMatch);
    assert_eq!(
        r.active_mode(),
        Some("nav"),
        "an unbound key keeps the mode"
    );
    assert_eq!(action(&mut r, "n").as_deref(), Some("next-pane"));
    assert_eq!(r.active_mode(), None);
    assert_eq!(r.feed(ck("n")), Feed::NoMatch);
}

#[test]
fn a_mode_that_binds_esc_keeps_it() {
    let mut r = resolver("[keybindings.modes.nav]\nEsc = \"next-pane\"\n");
    assert!(r.enter_mode("nav", false));
    assert_eq!(action(&mut r, "Esc").as_deref(), Some("next-pane"));
    // Only a bare Esc leaves; a modified one is an ordinary key.
    let mut r = resolver("[keybindings.modes.nav]\nn = \"next-pane\"\n");
    assert!(r.enter_mode("nav", false));
    assert_eq!(r.feed(ck("M-Esc")), Feed::NoMatch);
}

#[test]
fn sequences_work_inside_a_mode_and_a_miss_is_swallowed_when_exclusive() {
    let mut r = resolver(
        "[keybindings.modes.g]\n\"g g\" = \"next-pane\"\n\
         [keybindings.mode-options.g]\nexclusive = true\n",
    );
    assert!(r.enter_mode("g", false));
    assert_eq!(r.feed(ck("g")), Feed::Partial);
    assert!(!r.pending_at_prefix());
    assert_eq!(action(&mut r, "g").as_deref(), Some("next-pane"));
    assert_eq!(r.feed(ck("g")), Feed::Partial);
    assert_eq!(r.feed(ck("x")), Feed::Swallowed);
    assert!(!r.is_pending());
}

#[test]
fn entering_switches_modes_and_an_unknown_mode_changes_nothing() {
    let mut r = resolver("[keybindings.modes.nav]\nn = \"next-pane\"\n");
    assert!(r.enter_mode("nav", false));
    assert!(!r.enter_mode("nope", false));
    assert_eq!(r.active_mode(), Some("nav"));
    assert!(r.enter_mode("resize", false));
    assert_eq!(r.active_mode(), Some("resize"));
    assert_eq!(r.feed(ck("n")), Feed::Swallowed, "nav's keys are gone");
}

#[test]
fn mode_problems_are_diagnostics_that_skip_only_the_bad_entry() {
    let cfg = layered(
        "[keybindings.prefix-table]\nr = { action = \"enter-mode\", name = \"navv\" }\n\
         e = \"enter-mode\"\n\
         [keybindings.modes.nav]\nn = \"next-pane\"\nx = \"kill-pain\"\n\
         [keybindings.modes.Bad]\nn = \"next-pane\"\n",
    );
    let (mut r, diagnostics) = Resolver::new_lenient(&cfg.keybindings);
    let rendered: Vec<String> = diagnostics.iter().map(ToString::to_string).collect();
    assert_eq!(
        rendered,
        [
            "keybinding \"e\": enter-mode needs a mode: add name = \"<mode>\"",
            "keybinding \"r\": unknown key mode `navv` (did you mean `nav`?)",
            "keybinding \"mode:Bad\": invalid key mode name `Bad`: use lowercase \
             letters, digits, `-`, and `_`",
            "keybinding \"mode:nav x\": unknown action `kill-pain` (did you mean \
             `kill-pane`?)",
        ],
    );
    assert!(matches!(
        diagnostics[1].error,
        KeybindError::UnknownMode { .. }
    ));
    assert!(!r.enter_mode("Bad", false));
    assert!(r.enter_mode("nav", false));
    assert_eq!(action(&mut r, "n").as_deref(), Some("next-pane"));
}

#[test]
fn none_unbinds_a_key_in_one_mode_and_a_later_spelling_replaces_it() {
    let cfg = layered(
        "[keybindings.modes.resize]\nh = \"none\"\n\"S-l\" = \"next-pane\"\n\
         [keybindings.prefix-table]\nh = \"none\"\n",
    );
    let resize = &cfg.keybindings.modes["resize"];
    assert!(!resize.contains_key("h") && !resize.contains_key("L"));
    assert!(resize.contains_key("j"), "the rest of the mode stays");
    // A mode's "none" is not a prefix-table unbind a plugin must respect.
    assert_eq!(
        cfg.keybindings.unbound,
        std::iter::once(super::parse_chord_sequence("h").expect("parses")).collect()
    );
    let (mut r, diagnostics) = Resolver::new_lenient(&cfg.keybindings);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(r.enter_mode("resize", false));
    assert_eq!(r.feed(ck("h")), Feed::Swallowed);
    assert_eq!(action(&mut r, "L").as_deref(), Some("next-pane"));
}

#[test]
fn provenance_forgets_bindings_a_mode_unbound() {
    let (_, provenance) = crate::merged_config_with_provenance(
        "[keybindings.modes.resize]\nh = \"none\"\n",
        std::path::Path::new("config.toml"),
    )
    .expect("merges");
    assert!(
        !provenance
            .keys
            .keys()
            .any(|key| key.starts_with("keybindings.modes.resize.h"))
    );
    assert!(
        provenance
            .keys
            .contains_key("keybindings.modes.resize.j.action")
    );
}
