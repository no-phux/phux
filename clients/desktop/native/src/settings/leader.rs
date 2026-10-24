//! Optional desktop leader chords and their one-key command layer.
//!
//! Tab jumps use the desktop's visible 1–9 shortcuts (9 selects the last tab).
//! There is deliberately no TUI-style zero-based `0` tab shortcut.

use super::ghostty::ghostty_chord;
use super::keymap::{KeyChord, key_chord};

/// Normalize one modified chord. Bare typing, Escape and key sequences are not leaders.
#[must_use]
pub fn normalize_leader(value: &str) -> Option<String> {
    let value = value.trim();
    let mut rest = value;
    let mut tui = String::new();
    // Accept the familiar TUI spelling without treating the minus key as a separator.
    while let Some((modifier, tail)) = rest.split_once('-') {
        let modifier = match modifier {
            "C" | "c" => "ctrl",
            "M" | "m" | "A" | "a" => "alt",
            "S" | "s" => "shift",
            _ => break,
        };
        tui.push_str(modifier);
        tui.push('+');
        rest = tail;
    }
    let chord = if tui.is_empty() {
        ghostty_chord(value)?
    } else {
        tui.push_str(rest);
        ghostty_chord(&tui)?
    };
    let (modifiers, key) = chord.rsplit_once('+')?;
    let mut event = KeyChord::new(key);
    for modifier in modifiers.split('+') {
        match modifier {
            "cmd" => event.cmd = true,
            "ctrl" => event.ctrl = true,
            "alt" => event.alt = true,
            "shift" => event.shift = true,
            _ => return None,
        }
    }
    if !(event.cmd || event.ctrl || event.alt) || key == "escape" {
        return None;
    }
    // Use the same shifted-punctuation normalization as the runtime key lookup.
    let chord = key_chord(&event);
    let key = chord.rsplit('+').next()?;
    let supported = matches!(
        key,
        "," | "="
            | "-"
            | "["
            | "]"
            | "\\"
            | ";"
            | "'"
            | "."
            | "/"
            | "`"
            | "pageup"
            | "pagedown"
            | "home"
            | "end"
            | "left"
            | "right"
            | "up"
            | "down"
            | "enter"
            | "tab"
            | "space"
            | "backspace"
            | "delete"
    ) || (key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric())
        || key.strip_prefix('f').is_some_and(|number| {
            number
                .parse::<u8>()
                .is_ok_and(|number| (1..=24).contains(&number))
        });
    supported.then_some(chord)
}

/// Compact help for the supported desktop layer, not a claim of full TUI parity.
pub const HINTS: &[(&str, &str)] = &[
    ("h j k l / arrows", "Focus pane"),
    ("H J K L", "Resize pane"),
    ("| % / - \"", "Split right / down"),
    ("o / ;", "Next / previous pane"),
    ("z", "Zoom pane"),
    ("c", "New tab"),
    ("n p / Tab Shift+Tab", "Next / previous tab"),
    ("1–8 / 9", "Tab position / last tab"),
    ("b", "Toggle sidebar"),
    ("Space : ?", "Command palette"),
    ("w", "Go To"),
    ("s", "Session menu"),
    (",", "Rename tab"),
    ("S", "Settings"),
    ("q", "Agent needing attention"),
];

/// Map a leader suffix from either GPUI's produced key or its unshifted key form.
#[must_use]
pub fn command(key: &str, shift: bool) -> Option<&'static str> {
    match key {
        "H" => Some("resize-left"),
        "J" => Some("resize-down"),
        "K" => Some("resize-up"),
        "L" => Some("resize-right"),
        "S" => Some("settings"),
        "h" if shift => Some("resize-left"),
        "j" if shift => Some("resize-down"),
        "k" if shift => Some("resize-up"),
        "l" if shift => Some("resize-right"),
        "s" if shift => Some("settings"),
        "h" | "left" => Some("pane-left"),
        "j" | "down" => Some("pane-down"),
        "k" | "up" => Some("pane-up"),
        "l" | "right" => Some("pane-right"),
        "|" | "%" => Some("split-right"),
        "\\" | "5" if shift => Some("split-right"),
        "\"" => Some("split-down"),
        "'" if shift => Some("split-down"),
        "-" if !shift => Some("split-down"),
        ":" | "?" | "space" | " " => Some("palette"),
        ";" | "/" if shift => Some("palette"),
        "tab" if shift => Some("tab-prev"),
        _ if shift => None,
        "o" => Some("pane-next"),
        ";" => Some("pane-prev"),
        "z" => Some("zoom"),
        "c" => Some("new"),
        "n" | "tab" => Some("tab-next"),
        "p" => Some("tab-prev"),
        "b" => Some("sidebar"),
        "w" => Some("goto"),
        "s" => Some("main-menu"),
        "," => Some("rename"),
        "q" => Some("attention"),
        "1" => Some("tab-1"),
        "2" => Some("tab-2"),
        "3" => Some("tab-3"),
        "4" => Some("tab-4"),
        "5" => Some("tab-5"),
        "6" => Some("tab-6"),
        "7" => Some("tab-7"),
        "8" => Some("tab-8"),
        "9" => Some("tab-9"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_modified_leaders() {
        for (input, expected) in [
            ("C-a", "ctrl+a"),
            ("C-b", "ctrl+b"),
            ("C-Space", "ctrl+space"),
            (" ctrl + A ", "ctrl+a"),
            ("Control+space", "ctrl+space"),
            ("C-M-S-a", "ctrl+alt+shift+a"),
            ("Command+option+arrow_left", "cmd+alt+left"),
            ("C--", "ctrl+-"),
            ("ctrl+|", "ctrl+shift+\\"),
            ("alt+?", "alt+shift+/"),
            ("ctrl+F24", "ctrl+f24"),
        ] {
            assert_eq!(
                normalize_leader(input).as_deref(),
                Some(expected),
                "{input}"
            );
        }
    }

    #[test]
    fn rejects_typing_and_malformed_chords() {
        for input in [
            "",
            "off",
            "a",
            "Space",
            "shift+a",
            "ctrl",
            "ctrl+shift",
            "C-",
            "Escape",
            "ctrl+Escape",
            "ctrl+a b",
            "C-a C-b",
            "ctrl+a>ctrl+b",
            "ctrl+a,ctrl+b",
            "ctrl++a",
            "ctrl+a+b",
            "ctrl+é",
            "ctrl+insert",
            "ctrl+f25",
            "ctrl+!",
            "ctrl+\n",
        ] {
            assert_eq!(normalize_leader(input), None, "{input:?}");
        }
    }

    #[test]
    fn handles_shifted_gpui_keys() {
        for (produced, physical, expected) in [
            ("H", "h", "resize-left"),
            ("J", "j", "resize-down"),
            ("K", "k", "resize-up"),
            ("L", "l", "resize-right"),
            ("S", "s", "settings"),
            ("|", "\\", "split-right"),
            ("%", "5", "split-right"),
            ("\"", "'", "split-down"),
            (":", ";", "palette"),
            ("?", "/", "palette"),
        ] {
            assert_eq!(command(produced, false), Some(expected), "{produced}");
            assert_eq!(command(produced, true), Some(expected), "{produced}");
            assert_eq!(command(physical, true), Some(expected), "{physical}");
        }
        assert_eq!(command("tab", true), Some("tab-prev"));
        assert_eq!(command("c", true), None);
        assert_eq!(command("0", false), None);
        assert_eq!(command("escape", false), None);
    }
}
