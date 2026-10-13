//! Shell chords (`cmd+shift+p`) as GPUI keystrokes (`cmd-shift-p`).

/// Convert a settings chord into a GPUI binding stroke, or `None` when GPUI cannot name the key.
pub fn gpui_stroke(chord: &str) -> Option<String> {
    let mut parts = Vec::new();
    for raw in chord.split('+') {
        let part = raw.trim();
        if part.is_empty() {
            return None;
        }
        let mapped = match part {
            "cmd" | "ctrl" | "alt" | "shift" => part,
            "," => "comma",
            "=" => "equal",
            "-" => "minus",
            "[" => "bracketleft",
            "]" => "bracketright",
            "\\" => "backslash",
            ";" => "semicolon",
            "'" => "quote",
            "." => "period",
            "/" => "slash",
            "`" => "backtick",
            "pageup" | "pagedown" | "home" | "end" | "left" | "right" | "up" | "down"
            | "enter" | "escape" | "tab" | "space" | "backspace" | "delete" => part,
            other if other.len() == 1 && other.chars().all(|ch| ch.is_ascii_alphanumeric()) => other,
            other if other.starts_with('f') && other[1..].parse::<u8>().ok().is_some_and(|n| (1..=24).contains(&n)) => {
                other
            }
            _ => return None,
        };
        parts.push(mapped);
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("-"))
}

/// GPUI keystroke as the shell's `cmd+shift+p` chord, for lookup in the effective keymap.
pub fn shell_chord(platform: bool, ctrl: bool, alt: bool, shift: bool, key: &str, held: bool) -> String {
    if held {
        return String::new();
    }
    let lower = key.to_ascii_lowercase();
    let name = match lower.as_str() {
        "comma" => ",",
        "equal" => "=",
        "minus" => "-",
        "bracketleft" => "[",
        "bracketright" => "]",
        "backslash" => "\\",
        "semicolon" => ";",
        "quote" => "'",
        "period" => ".",
        "slash" => "/",
        "backtick" => "`",
        "space" => "space",
        other => other,
    };
    if name.is_empty() {
        return String::new();
    }
    // An unmodified printable is typing, never a chord.
    if !platform && !ctrl && !alt && !shift && name.chars().count() == 1 {
        return String::new();
    }
    let mut parts = Vec::new();
    if platform {
        parts.push("cmd");
    }
    if ctrl {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    parts.push(name);
    parts.join("+")
}
