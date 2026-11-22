use super::*;

fn m(f: impl FnOnce(&mut Mods)) -> Mods {
    let mut mods = Mods::default();
    f(&mut mods);
    mods
}

#[test]
fn plain_text_types() {
    assert_eq!(
        typed(
            &Key::Text("a".into()),
            Mods::default(),
            Some('a'),
            None,
            false
        ),
        Some(Typed::Text("a".into()))
    );
}

#[test]
fn ctrl_space_is_the_space_key() {
    let Some(Typed::Key(event)) = typed(
        &Key::Text(" ".into()),
        m(|m| m.ctrl = true),
        Some(' '),
        None,
        false,
    ) else {
        panic!("ctrl-space is a key atom");
    };
    assert_eq!(event.key, PhysicalKey::Space);
}

#[test]
fn named_keys_carry_shift() {
    let Some(Typed::Key(event)) = typed(&Key::Tab, m(|m| m.shift = true), None, None, false) else {
        panic!("tab is a key atom");
    };
    assert_eq!(event.key, PhysicalKey::Tab);
    assert!(event.mods.contains(ModSet::SHIFT));
}

#[test]
fn function_keys_map_and_out_of_range_ones_do_not() {
    assert_eq!(function(1), Some(PhysicalKey::F1));
    assert_eq!(function(12), Some(PhysicalKey::F12));
    assert_eq!(function(0), None);
    assert_eq!(function(13), None);
}

#[cfg(target_os = "macos")]
#[test]
fn command_chords_are_app_shortcuts_not_input() {
    let cmd = m(|m| m.cmd = true);
    assert_eq!(
        shortcut(&Key::Command('v'), cmd, None),
        Some(Shortcut::Paste)
    );
    assert_eq!(
        shortcut(&Key::Command('t'), cmd, None),
        Some(Shortcut::NewTerminal)
    );
    assert_eq!(typed(&Key::Command('q'), cmd, Some('q'), None, false), None);
}

#[test]
fn option_as_meta_sends_the_chord() {
    let Some(Typed::Key(event)) = typed(
        &Key::Text("å".into()),
        m(|m| m.alt = true),
        Some('a'),
        None,
        true,
    ) else {
        panic!("option is meta");
    };
    assert_eq!(event.key, physical_key_for_char('a'));
    assert!(event.mods.contains(ModSet::ALT));
}

#[cfg(target_os = "macos")]
#[test]
fn option_types_layout_text_on_macos() {
    assert_eq!(
        typed(
            &Key::Text("å".into()),
            m(|m| m.alt = true),
            Some('a'),
            None,
            false
        ),
        Some(Typed::Text("å".into()))
    );
}

#[test]
fn ctrl_chords_follow_the_physical_key_on_other_layouts() {
    // A Cyrillic layout types 'с' on the C key; Ctrl-C must still be ^C.
    let Some(Typed::Key(event)) = typed(
        &Key::Text("с".into()),
        m(|m| m.ctrl = true),
        Some('с'),
        Some(PhysicalKeyCode::KeyC),
        false,
    ) else {
        panic!("ctrl-c is a key atom");
    };
    assert_eq!(event.key, physical_key_for_char('c'));
    assert_eq!(event.unshifted_codepoint, Some(u32::from('c')));
}

#[test]
fn shifted_symbols_match_by_physical_key() {
    let chord = m(|m| {
        m.shift = true;
        m.cmd = true;
        m.logo = cfg!(target_os = "macos");
        m.ctrl = !cfg!(target_os = "macos");
    });
    let plus = shortcut(&Key::Command('+'), chord, Some(PhysicalKeyCode::Equal));
    assert_eq!(plus, Some(Shortcut::ZoomIn));
}

#[cfg(target_os = "macos")]
#[test]
fn chords_stay_exact_with_other_modifiers() {
    let held = m(|m| {
        m.cmd = true;
        m.alt = true;
    });
    assert_eq!(shortcut(&Key::Command('d'), held, None), None);
}
