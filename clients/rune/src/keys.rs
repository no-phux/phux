//! Rune keys as the terminal's structured key atoms.
//!
//! Rune resolves layout and modifiers; phux's libghostty encoder turns the
//! atom into bytes. Nothing here writes an escape sequence by hand.

use phux_client_runtime::control::keys::physical_key_for_char;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use rune::{Key, Mods, PhysicalKeyCode};

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
    Palette,
    ToggleSidebar,
    JumpToAttention,
    SplitRight,
    SplitDown,
    ZoomPane,
    CloseTerminal,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    Find,
    FindNext,
    FindPrevious,
    FindSelection,
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

/// The US-layout character of a physical key: what a chord means whatever
/// the active layout types there (Ctrl-C on a Cyrillic layout is still ^C).
pub fn us_char(code: PhysicalKeyCode) -> Option<char> {
    use PhysicalKeyCode as C;
    const LETTERS: [PhysicalKeyCode; 26] = [
        C::KeyA,
        C::KeyB,
        C::KeyC,
        C::KeyD,
        C::KeyE,
        C::KeyF,
        C::KeyG,
        C::KeyH,
        C::KeyI,
        C::KeyJ,
        C::KeyK,
        C::KeyL,
        C::KeyM,
        C::KeyN,
        C::KeyO,
        C::KeyP,
        C::KeyQ,
        C::KeyR,
        C::KeyS,
        C::KeyT,
        C::KeyU,
        C::KeyV,
        C::KeyW,
        C::KeyX,
        C::KeyY,
        C::KeyZ,
    ];
    const DIGITS: [PhysicalKeyCode; 10] = [
        C::Digit0,
        C::Digit1,
        C::Digit2,
        C::Digit3,
        C::Digit4,
        C::Digit5,
        C::Digit6,
        C::Digit7,
        C::Digit8,
        C::Digit9,
    ];
    if let Some(i) = LETTERS.iter().position(|k| *k == code) {
        return char::from_u32('a' as u32 + i as u32);
    }
    if let Some(i) = DIGITS.iter().position(|k| *k == code) {
        return char::from_digit(i as u32, 10);
    }
    Some(match code {
        C::Minus => '-',
        C::Equal => '=',
        C::BracketLeft => '[',
        C::BracketRight => ']',
        C::Backslash => '\\',
        C::Semicolon => ';',
        C::Quote => '\'',
        C::Comma => ',',
        C::Period => '.',
        C::Slash => '/',
        C::Backquote => '`',
        C::Space => ' ',
        _ => return None,
    })
}

/// The Command (macOS) or Ctrl-Shift (elsewhere) chords the app keeps.
/// `physical` names the key itself, so a shifted chord (Ctrl-Shift-minus
/// arrives as `_`) and other layouts still match.
pub fn shortcut(key: &Key, mods: Mods, physical: Option<PhysicalKeyCode>) -> Option<Shortcut> {
    let Key::Command(c) = key else {
        return None;
    };
    // Off macOS Rune reports Ctrl chords as `Command`; only Ctrl-Shift ones
    // belong to the app, so plain Ctrl-C still reaches the shell.
    if !cfg!(target_os = "macos") && !mods.shift {
        return None;
    }
    // Exact chords: ⌘⌥D or ⌘⌃T stay with the program.
    if mods.alt || (cfg!(target_os = "macos") && mods.ctrl) {
        return None;
    }
    Some(match physical.and_then(us_char).unwrap_or(*c) {
        'c' => Shortcut::Copy,
        'v' => Shortcut::Paste,
        't' => Shortcut::NewTerminal,
        'k' | 'p' => Shortcut::Palette,
        'b' => Shortcut::ToggleSidebar,
        'j' => Shortcut::JumpToAttention,
        'w' => Shortcut::CloseTerminal,
        'd' if mods.shift && cfg!(target_os = "macos") => Shortcut::SplitDown,
        'd' => Shortcut::SplitRight,
        'e' if !cfg!(target_os = "macos") => Shortcut::SplitDown,
        'e' => Shortcut::FindSelection,
        'f' => Shortcut::Find,
        'g' if mods.shift => Shortcut::FindPrevious,
        'g' => Shortcut::FindNext,
        'n' => Shortcut::NewSession,
        ']' | '}' => Shortcut::NextTerminal,
        '[' | '{' => Shortcut::PreviousTerminal,
        '=' | '+' => Shortcut::ZoomIn,
        '-' | '_' => Shortcut::ZoomOut,
        '0' | ')' => Shortcut::ZoomReset,
        _ => return None,
    })
}

/// Command-↑/↓ (and Home/End) jump through history rather than reaching the
/// shell.
pub fn scroll_shortcut(key: &Key, mods: Mods) -> Option<Shortcut> {
    if !(mods.cmd && cfg!(target_os = "macos")) {
        return None;
    }
    // ⌘⌥ arrows move between panes; ⌘⇧↩ zooms one.
    if mods.alt {
        return match key {
            Key::Left => Some(Shortcut::FocusLeft),
            Key::Right => Some(Shortcut::FocusRight),
            Key::Up => Some(Shortcut::FocusUp),
            Key::Down => Some(Shortcut::FocusDown),
            _ => None,
        };
    }
    if mods.shift && *key == Key::Enter {
        return Some(Shortcut::ZoomPane);
    }
    match key {
        Key::Home | Key::Up => Some(Shortcut::ScrollTop),
        Key::End | Key::Down => Some(Shortcut::ScrollBottom),
        _ => None,
    }
}

/// The physical key and unshifted character a Ctrl or Meta chord encodes:
/// the key's own position when known, else the layout's character.
fn chord(physical: Option<PhysicalKeyCode>, fallback: Option<char>) -> Option<(PhysicalKey, char)> {
    let ch = physical
        .and_then(us_char)
        .or(fallback)?
        .to_ascii_lowercase();
    let key = if ch == ' ' {
        PhysicalKey::Space
    } else {
        physical_key_for_char(ch)
    };
    Some((key, ch))
}

/// Translate one key down. `base` is the character the key types with no
/// modifiers (Rune's `Tx::key_base`) and `physical` the key itself
/// (`Tx::physical_key`). `None`: not terminal input.
pub fn typed(
    key: &Key,
    key_mods: Mods,
    base: Option<char>,
    physical: Option<PhysicalKeyCode>,
) -> Option<Typed> {
    let set = mods(key_mods);
    if let Some(physical) = named(key) {
        return Some(Typed::Key(atom(physical, set, None)));
    }
    match key {
        Key::Text(text) if !key_mods.ctrl && !alt_is_meta(key_mods) => {
            Some(Typed::Text(text.clone()))
        }
        Key::Text(text) => {
            let (key, ch) = chord(physical, base.or_else(|| text.chars().next()))?;
            Some(Typed::Key(atom(key, set, Some(ch))))
        }
        // Off macOS a Ctrl chord arrives as `Command`; it is terminal input
        // unless `shortcut` claimed it.
        Key::Command(c) if !cfg!(target_os = "macos") => {
            let (key, ch) = chord(physical, Some(base.unwrap_or(*c)))?;
            Some(Typed::Key(atom(key, set, Some(ch))))
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
            typed(&Key::Text("a".into()), Mods::default(), Some('a'), None),
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
        ) else {
            panic!("ctrl-space is a key atom");
        };
        assert_eq!(event.key, PhysicalKey::Space);
    }

    #[test]
    fn named_keys_carry_shift() {
        let Some(Typed::Key(event)) = typed(&Key::Tab, m(|m| m.shift = true), None, None) else {
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
        assert_eq!(typed(&Key::Command('q'), cmd, Some('q'), None), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn option_types_layout_text_on_macos() {
        assert_eq!(
            typed(&Key::Text("å".into()), m(|m| m.alt = true), Some('a'), None),
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
    fn shifted_zoom_chords_match_by_physical_key() {
        let chord = m(|m| {
            m.cmd = true;
            m.shift = true;
        });
        let minus = shortcut(&Key::Command('_'), chord, Some(PhysicalKeyCode::Minus));
        assert_eq!(minus, Some(Shortcut::ZoomOut));
        let zero = shortcut(&Key::Command(')'), chord, Some(PhysicalKeyCode::Digit0));
        assert_eq!(zero, Some(Shortcut::ZoomReset));
    }

    #[test]
    fn us_chars_cover_letters_digits_and_punctuation() {
        assert_eq!(us_char(PhysicalKeyCode::KeyA), Some('a'));
        assert_eq!(us_char(PhysicalKeyCode::KeyZ), Some('z'));
        assert_eq!(us_char(PhysicalKeyCode::Digit7), Some('7'));
        assert_eq!(us_char(PhysicalKeyCode::BracketRight), Some(']'));
        assert_eq!(us_char(PhysicalKeyCode::F1), None);
    }
}
