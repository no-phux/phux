//! The chord tests for Linux, twins of the macOS ones in `keys.rs`: the app's
//! chords all carry Ctrl+Shift here, so the bare Ctrl chords stay terminal input.

use rune::{Key, Mods, PhysicalKeyCode};

use super::*;

fn m(f: impl FnOnce(&mut Mods)) -> Mods {
    let mut mods = Mods::default();
    f(&mut mods);
    mods
}

/// Ctrl and Shift held, as Rune reports them off macOS (`cmd` is Ctrl).
fn ctrl_shift(f: impl FnOnce(&mut Mods)) -> Mods {
    m(|m| {
        m.cmd = true;
        m.ctrl = true;
        m.shift = true;
        f(m);
    })
}

/// Off macOS the app's chords all carry Shift, so the bare Ctrl chords a
/// shell and its editors live on (^C ^W ^F ^V ^T) reach the terminal.
#[test]
fn linux_plain_ctrl_chords_are_terminal_input() {
    let ctrl = m(|m| {
        m.cmd = true;
        m.ctrl = true;
    });
    for c in ['c', 'w', 'f', 'v', 't', 'g'] {
        let physical = physical_code(c);
        assert_eq!(shortcut(&Key::Command(c), ctrl, physical), None, "^{c}");
        let Some(Typed::Key(event)) = typed(&Key::Command(c), ctrl, Some(c), physical, false)
        else {
            panic!("^{c} is a key atom for the terminal");
        };
        assert_eq!(event.key, physical_key_for_char(c));
        assert!(event.mods.contains(ModSet::CTRL), "^{c}");
    }
}

#[test]
fn linux_chords_stay_exact_with_other_modifiers() {
    let f = Some(PhysicalKeyCode::KeyF);
    // Super belongs to the compositor (Omarchy binds it); a held Super
    // or an extra Alt is not the app's Find.
    assert_eq!(
        shortcut(&Key::Command('F'), ctrl_shift(|m| m.logo = true), f),
        None
    );
    assert_eq!(
        shortcut(&Key::Command('F'), ctrl_shift(|m| m.alt = true), f),
        None
    );
}

fn physical_code(c: char) -> Option<PhysicalKeyCode> {
    Some(match c {
        'c' => PhysicalKeyCode::KeyC,
        'w' => PhysicalKeyCode::KeyW,
        'f' => PhysicalKeyCode::KeyF,
        'v' => PhysicalKeyCode::KeyV,
        't' => PhysicalKeyCode::KeyT,
        'g' => PhysicalKeyCode::KeyG,
        _ => return None,
    })
}
