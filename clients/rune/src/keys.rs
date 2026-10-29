//! Rune keys as the terminal's structured key atoms.
//!
//! Rune resolves layout and modifiers; phux's libghostty encoder turns the
//! atom into bytes. Nothing here writes an escape sequence by hand.

use phux_client_runtime::control::keys::physical_key_for_char;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use rune::{Key, Mods};

/// What a key press should do in the terminal.
#[derive(Debug, Clone, PartialEq)]
pub enum Typed {
    /// Committed layout text with no Ctrl or Meta: type it.
    Text(String),
    /// A key the encoder resolves with its modifiers (Enter, arrows, Ctrl-C).
    Key(KeyEvent),
}

/// An app shortcut rather than terminal input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    Copy,
    Paste,
    NewTerminal,
    NewSession,
    NextTerminal,
    PreviousTerminal,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ScrollTop,
    ScrollBottom,
}

pub fn mods(mods: Mods) -> ModSet {
    let mut set = ModSet::empty();
    set.set(ModSet::SHIFT, mods.shift);
    set.set(ModSet::CTRL, mods.ctrl);
    set.set(ModSet::ALT, mods.alt);
    set.set(ModSet::SUPER, mods.logo);
    set
}

fn atom(key: PhysicalKey, mods: ModSet, base: Option<char>) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: base.map(u32::from),
    }
}

fn named(key: &Key) -> Option<PhysicalKey> {
    use PhysicalKey as K;
    Some(match key {
        Key::Enter => K::Enter,
        Key::Backspace => K::Backspace,
        Key::Delete => K::Delete,
        Key::Left => K::ArrowLeft,
        Key::Right => K::ArrowRight,
        Key::Up => K::ArrowUp,
        Key::Down => K::ArrowDown,
        Key::Home => K::Home,
        Key::End => K::End,
        Key::PageUp => K::PageUp,
        Key::PageDown => K::PageDown,
        Key::Escape => K::Escape,
        Key::Tab => K::Tab,
        Key::Function(n) => function(*n)?,
        _ => return None,
    })
}

fn function(n: u8) -> Option<PhysicalKey> {
    use PhysicalKey as K;
    const KEYS: [PhysicalKey; 12] = [
        K::F1,
        K::F2,
        K::F3,
        K::F4,
        K::F5,
        K::F6,
        K::F7,
        K::F8,
        K::F9,
        K::F10,
        K::F11,
        K::F12,
    ];
    KEYS.get(usize::from(n).checked_sub(1)?).copied()
}

/// The Command (macOS) or Ctrl-Shift (elsewhere) chords the app keeps.
pub fn shortcut(key: &Key, mods: Mods) -> Option<Shortcut> {
    let Key::Command(c) = key else {
        return None;
    };
    // Off macOS Rune reports Ctrl chords as `Command`; only Ctrl-Shift ones
    // belong to the app, so plain Ctrl-C still reaches the shell.
    if !cfg!(target_os = "macos") && !mods.shift {
        return None;
    }
    Some(match (c, mods.shift) {
        ('c', _) => Shortcut::Copy,
        ('v', _) => Shortcut::Paste,
        ('t', _) => Shortcut::NewTerminal,
        ('n', _) => Shortcut::NewSession,
        (']', _) | ('}', _) => Shortcut::NextTerminal,
        ('[', _) | ('{', _) => Shortcut::PreviousTerminal,
        ('=' | '+', _) => Shortcut::ZoomIn,
        ('-', _) => Shortcut::ZoomOut,
        ('0', _) => Shortcut::ZoomReset,
        _ => return None,
    })
}

/// Shift-Command arrows jump through history rather than reaching the shell.
pub fn scroll_shortcut(key: &Key, mods: Mods) -> Option<Shortcut> {
    if !(mods.cmd && cfg!(target_os = "macos")) {
        return None;
    }
    match key {
        Key::Home | Key::Up => Some(Shortcut::ScrollTop),
        Key::End | Key::Down => Some(Shortcut::ScrollBottom),
        _ => None,
    }
}

/// Translate one key down. `base` is the character the key types with no
/// modifiers (Rune's `Tx::key_base`). `None`: not terminal input.
pub fn typed(key: &Key, key_mods: Mods, base: Option<char>) -> Option<Typed> {
    let set = mods(key_mods);
    if let Some(physical) = named(key) {
        return Some(Typed::Key(atom(physical, set, None)));
    }
    match key {
        Key::Text(text) if !key_mods.ctrl && !alt_is_meta(key_mods) => {
            Some(Typed::Text(text.clone()))
        }
        Key::Text(text) => {
            let ch = base.or_else(|| text.chars().next())?.to_ascii_lowercase();
            let physical = if ch == ' ' {
                PhysicalKey::Space
            } else {
                physical_key_for_char(ch)
            };
            Some(Typed::Key(atom(physical, set, Some(ch))))
        }
        // Off macOS a Ctrl chord arrives as `Command`; it is terminal input
        // unless `shortcut` claimed it.
        Key::Command(c) if !cfg!(target_os = "macos") => {
            let ch = base.unwrap_or(*c).to_ascii_lowercase();
            Some(Typed::Key(atom(physical_key_for_char(ch), set, Some(ch))))
        }
        _ => None,
    }
}

/// macOS Option types layout characters (å, ∑), as Ghostty's default does;
/// elsewhere Alt is Meta.
const fn alt_is_meta(mods: Mods) -> bool {
    mods.alt && !cfg!(target_os = "macos")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(f: impl FnOnce(&mut Mods)) -> Mods {
        let mut mods = Mods::default();
        f(&mut mods);
        mods
    }

    #[test]
    fn plain_text_types() {
        assert_eq!(
            typed(&Key::Text("a".into()), Mods::default(), Some('a')),
            Some(Typed::Text("a".into()))
        );
    }

    #[test]
    fn ctrl_letters_are_key_atoms_on_the_physical_key() {
        let Some(Typed::Key(event)) =
            typed(&Key::Text("c".into()), m(|m| m.ctrl = true), Some('c'))
        else {
            panic!("ctrl-c is a key atom");
        };
        assert_eq!(event.key, physical_key_for_char('c'));
        assert!(event.mods.contains(ModSet::CTRL));
        assert_eq!(event.text, None);
    }

    #[test]
    fn ctrl_space_is_the_space_key() {
        let Some(Typed::Key(event)) =
            typed(&Key::Text(" ".into()), m(|m| m.ctrl = true), Some(' '))
        else {
            panic!("ctrl-space is a key atom");
        };
        assert_eq!(event.key, PhysicalKey::Space);
    }

    #[test]
    fn named_keys_carry_shift() {
        let Some(Typed::Key(event)) = typed(&Key::Tab, m(|m| m.shift = true), None) else {
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
        assert_eq!(shortcut(&Key::Command('v'), cmd), Some(Shortcut::Paste));
        assert_eq!(
            shortcut(&Key::Command('t'), cmd),
            Some(Shortcut::NewTerminal)
        );
        assert_eq!(typed(&Key::Command('q'), cmd, Some('q')), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn option_types_layout_text_on_macos() {
        assert_eq!(
            typed(&Key::Text("å".into()), m(|m| m.alt = true), Some('a')),
            Some(Typed::Text("å".into()))
        );
    }
}
