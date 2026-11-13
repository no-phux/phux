//! Characters no installed face draws.
//!
//! On macOS only the last-resort font covers them (an unassigned code point,
//! a noncharacter). Shaping one makes the text
//! system add that font to the window's symbol fallback, where it covers every
//! character and shadows the faces a later symbol needs: ⏵ then draws as a box
//! for the rest of the window's life. So these are never shaped; the grid
//! draws its own empty box in their cells.

/// Whether only the last-resort font would draw `ch`, as CoreText decides it.
/// Each character is asked once per thread.
#[cfg(target_vendor = "apple")]
pub(super) fn nothing_draws(ch: char) -> bool {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static ASKED: RefCell<HashMap<char, bool>> = RefCell::default();
    }
    // Private use is left to whatever face the person picked, whose icons
    // CoreText may not know.
    if ch.is_ascii() || private_use(ch) {
        return false;
    }
    ASKED.with_borrow_mut(|asked| {
        *asked
            .entry(ch)
            .or_insert_with(|| !bundled(ch) && last_resort(ch))
    })
}

#[cfg(target_vendor = "apple")]
const fn private_use(ch: char) -> bool {
    matches!(ch, '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}')
}

/// Rune's own faces have it. CoreText doesn't know them: they're loaded in
/// process, not installed.
#[cfg(target_vendor = "apple")]
fn bundled(ch: char) -> bool {
    use skrifa::MetadataProvider;
    use std::sync::LazyLock;
    static FACES: LazyLock<Vec<skrifa::FontRef<'static>>> = LazyLock::new(|| {
        (super::FONTS.iter())
            .filter_map(|bytes| skrifa::FontRef::new(bytes).ok())
            .collect()
    });
    FACES.iter().any(|face| face.charmap().map(ch).is_some())
}

#[cfg(not(target_vendor = "apple"))]
pub(super) const fn nothing_draws(_: char) -> bool {
    false
}

/// CoreText's fallback for `ch` from the system font, the lookup the text
/// system itself makes, is the last-resort font.
#[cfg(target_vendor = "apple")]
#[allow(unsafe_code)]
fn last_resort(ch: char) -> bool {
    use objc2_core_foundation::{CFRange, CFString};
    use objc2_core_text::{CTFont, CTFontUIFontType};
    let text = CFString::from_str(ch.encode_utf8(&mut [0; 4]));
    let range = CFRange {
        location: 0,
        length: text.length(),
    };
    // SAFETY: CoreText fonts made for the lookup and read for their family
    // name only, as the text system's own fallback does.
    unsafe {
        let Some(base) = CTFont::new_ui_font_for_language(CTFontUIFontType::System, 0.0, None)
        else {
            return false;
        };
        let font = CTFont::for_string_with_language(&base, &text, range, None);
        font.family_name().to_string().trim_start_matches('.') == "LastResort"
    }
}

#[cfg(all(test, target_vendor = "apple"))]
mod tests {
    use super::nothing_draws;

    #[test]
    fn only_characters_no_face_has_are_held_back() {
        // Unassigned and a noncharacter.
        for ch in ['\u{378}', '\u{FDD0}'] {
            assert!(nothing_draws(ch), "{:04X}", ch as u32);
        }
        // Text, a symbol only a fallback face has, an emoji, private use (a Nerd
        // Font icon), and a power symbol only the bundled Nerd Font has.
        for ch in [
            'a',
            'é',
            '⏵',
            '✔',
            '😀',
            '\u{E0A0}',
            '\u{F0001}',
            '\u{23FB}',
        ] {
            assert!(!nothing_draws(ch), "{:04X}", ch as u32);
        }
    }
}
