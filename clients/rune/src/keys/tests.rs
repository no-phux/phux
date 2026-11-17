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
fn ctrl_letters_are_key_atoms_on_the_physical_key() {
    let Some(Typed::Key(event)) = typed(
        &Key::Text("c".into()),
        m(|m| m.ctrl = true),
        Some('c'),
        None,
        false,
    ) else {
        panic!("ctrl-c is a key atom");
    };
    assert_eq!(event.key, physical_key_for_char('c'));
    assert!(event.mods.contains(ModSet::CTRL));
    assert_eq!(event.text, None);
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
fn non_typing_keys_reach_the_terminal_with_their_modifiers() {
    let held = m(|m| {
        m.shift = true;
        m.ctrl = true;
        m.alt = true;
    });
    for (key, physical) in [
        (Key::Insert, PhysicalKey::Insert),
        (Key::PrintScreen, PhysicalKey::PrintScreen),
        (Key::ScrollLock, PhysicalKey::ScrollLock),
        (Key::Pause, PhysicalKey::Pause),
    ] {
        for modifiers in [Mods::default(), held] {
            assert_eq!(shortcut(&key, modifiers, None), None);
            assert_eq!(
                typed(&key, modifiers, None, None, false),
                Some(Typed::Key(atom(physical, mods(modifiers), None))),
                "{key:?}"
            );
        }
    }
}

#[test]
fn function_keys_map_and_out_of_range_ones_do_not() {
    assert_eq!(function(1), Some(PhysicalKey::F1));
    assert_eq!(function(12), Some(PhysicalKey::F12));
    assert_eq!(function(0), None);
    assert_eq!(function(13), None);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn omarchy_super_c_and_v_copy_and_paste() {
    let ctrl = m(|m| m.ctrl = true);
    let shift = m(|m| m.shift = true);
    assert_eq!(shortcut(&Key::Insert, ctrl, None), Some(Shortcut::Copy));
    assert_eq!(shortcut(&Key::Insert, shift, None), Some(Shortcut::Paste));
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

#[cfg(target_os = "macos")]
#[test]
fn find_chords() {
    let cmd = m(|m| m.cmd = true);
    let shifted = m(|m| {
        m.cmd = true;
        m.shift = true;
    });
    let g = Some(PhysicalKeyCode::KeyG);
    assert_eq!(
        shortcut(&Key::Command('f'), cmd, None),
        Some(Shortcut::Find)
    );
    assert_eq!(
        shortcut(&Key::Command('g'), cmd, g),
        Some(Shortcut::FindNext)
    );
    assert_eq!(
        shortcut(&Key::Command('G'), shifted, g),
        Some(Shortcut::FindPrevious)
    );
    assert_eq!(
        shortcut(&Key::Command('e'), cmd, None),
        Some(Shortcut::FindSelection)
    );
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
fn shift_is_part_of_the_chord() {
    let cmd = m(|m| m.cmd = true);
    let shifted = m(|m| {
        m.cmd = true;
        m.shift = true;
    });
    let w = Some(PhysicalKeyCode::KeyW);
    assert_eq!(
        shortcut(&Key::Command('w'), cmd, w),
        Some(Shortcut::CloseTerminal)
    );
    assert_eq!(
        shortcut(&Key::Command('w'), shifted, w),
        Some(Shortcut::CloseTab)
    );
    let close = Some(PhysicalKeyCode::BracketRight);
    assert_eq!(
        shortcut(&Key::Command('}'), shifted, close),
        Some(Shortcut::NextTab)
    );
    // Not in the table, so not the app's.
    assert_eq!(shortcut(&Key::Command('t'), shifted, None), None);
}

#[cfg(target_os = "macos")]
#[test]
fn command_arrows_walk_terminals_and_sessions() {
    let cmd = m(|m| m.cmd = true);
    let shifted = m(|m| {
        m.cmd = true;
        m.shift = true;
    });
    let walk = [
        (Key::Left, Shortcut::PreviousTerminalInSession),
        (Key::Right, Shortcut::NextTerminalInSession),
        (Key::Up, Shortcut::PreviousSession),
        (Key::Down, Shortcut::NextSession),
    ];
    for (key, action) in walk {
        assert_eq!(shortcut(&key, cmd, None), Some(action), "{key:?}");
    }
    assert_eq!(shortcut(&Key::Up, shifted, None), Some(Shortcut::PromptUp));
    assert_eq!(
        shortcut(&Key::Down, shifted, None),
        Some(Shortcut::PromptDown)
    );
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

#[test]
fn us_chars_cover_letters_digits_and_punctuation() {
    assert_eq!(us_char(PhysicalKeyCode::KeyA), Some('a'));
    assert_eq!(us_char(PhysicalKeyCode::KeyZ), Some('z'));
    assert_eq!(us_char(PhysicalKeyCode::Digit7), Some('7'));
    assert_eq!(us_char(PhysicalKeyCode::BracketRight), Some(']'));
    assert_eq!(us_char(PhysicalKeyCode::F1), None);
}
