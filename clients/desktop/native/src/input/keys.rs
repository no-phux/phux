use gpuix_native::native_extensions::gpui::{KeyDownEvent, Keystroke, Modifiers};
use phux_client_runtime::control::keys::physical_key_for_char;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

pub(super) fn modifiers(value: Modifiers) -> ModSet {
    let mut result = ModSet::empty();
    result.set(ModSet::SHIFT, value.shift);
    result.set(ModSet::CTRL, value.control);
    result.set(ModSet::ALT, value.alt);
    result.set(ModSet::SUPER, value.platform);
    result
}

// GPUI does not expose a hardware scan code. This maps its resolved key names
// to the existing protocol vocabulary, never to hand-written escape sequences.
fn physical_key(name: &str) -> PhysicalKey {
    use PhysicalKey as K;
    const NAMED: &[(&str, PhysicalKey)] = &[
        ("enter", K::Enter),
        ("return", K::Enter),
        ("tab", K::Tab),
        ("space", K::Space),
        ("backspace", K::Backspace),
        ("delete", K::Delete),
        ("escape", K::Escape),
        ("up", K::ArrowUp),
        ("down", K::ArrowDown),
        ("left", K::ArrowLeft),
        ("right", K::ArrowRight),
        ("home", K::Home),
        ("end", K::End),
        ("pageup", K::PageUp),
        ("pagedown", K::PageDown),
        ("insert", K::Insert),
    ];
    if let Some((_, key)) = NAMED.iter().find(|(candidate, _)| *candidate == name) {
        return *key;
    }
    if let Some(number) = name.strip_prefix('f').and_then(|v| v.parse::<u32>().ok())
        && (1..=25).contains(&number)
    {
        return PhysicalKey::try_from(K::F1 as u32 + number - 1).unwrap_or(K::Unidentified);
    }
    let mut chars = name.chars();
    let first = chars.next();
    if chars.next().is_none() {
        return first.map_or(K::Unidentified, physical_key_for_char);
    }
    K::Unidentified
}

pub(super) fn uses_text(event: &KeyDownEvent, option_as_alt: bool) -> bool {
    let mods = event.keystroke.modifiers;
    if mods.control || (mods.alt && option_as_alt) {
        return false;
    }
    event.keystroke.key_char.is_some()
}

pub(super) fn event(stroke: &Keystroke, action: KeyAction) -> KeyEvent {
    KeyEvent {
        action,
        key: physical_key(&stroke.key),
        mods: modifiers(stroke.modifiers),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: single_scalar(&stroke.key),
    }
}

fn single_scalar(text: &str) -> Option<u32> {
    let mut chars = text.chars();
    let first = chars.next()?;
    chars.next().is_none().then_some(first as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_and_function_keys_use_protocol_atoms() {
        assert_eq!(physical_key("left"), PhysicalKey::ArrowLeft);
        assert_eq!(physical_key("f25"), PhysicalKey::F25);
        assert_eq!(physical_key("f26"), PhysicalKey::Unidentified);
        assert_eq!(physical_key("!"), PhysicalKey::Digit1);
        assert_eq!(physical_key("漢"), PhysicalKey::Unidentified);
    }

    #[test]
    fn option_policy_only_changes_arbitration() {
        let down = KeyDownEvent {
            keystroke: Keystroke::parse("alt-s->ß").expect("stroke"),
            is_held: true,
            prefer_character_input: false,
        };
        assert!(uses_text(&down, false));
        assert!(!uses_text(&down, true));
        let encoded = event(&down.keystroke, KeyAction::Repeat);
        assert_eq!(encoded.action, KeyAction::Repeat);
        assert_eq!(encoded.mods, ModSet::ALT);
        assert_eq!(encoded.key, PhysicalKey::S);
        assert!(encoded.text.is_none());
    }
}
