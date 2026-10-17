//! Shell chords (`cmd+shift+p`) as GPUI keystrokes (`cmd-shift-p`).

/// Convert a settings chord into a GPUI binding stroke, or `None` when GPUI cannot name the key.
/// GPUI names punctuation by the character itself (`cmd-,`, `cmd-]`).
pub fn gpui_stroke(chord: &str) -> Option<String> {
    let known = |part: &str| match part {
        "cmd" | "ctrl" | "alt" | "shift" => true,
        "," | "=" | "-" | "[" | "]" | "\\" | ";" | "'" | "." | "/" | "`" => true,
        "pageup" | "pagedown" | "home" | "end" | "left" | "right" | "up" | "down" | "enter"
        | "escape" | "tab" | "space" | "backspace" | "delete" => true,
        other if other.len() == 1 && other.chars().all(|ch| ch.is_ascii_alphanumeric()) => true,
        other => other
            .strip_prefix('f')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=24).contains(&n)),
    };
    let parts = chord
        .split('+')
        .map(str::trim)
        .map(|part| known(part).then_some(part))
        .collect::<Option<Vec<_>>>()?;
    Some(parts.join("-"))
}

/// GPUI keystroke as the shell's `cmd+shift+p` chord, for lookup in the effective keymap.
pub fn shell_chord(
    platform: bool,
    ctrl: bool,
    alt: bool,
    shift: bool,
    key: &str,
    held: bool,
) -> String {
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
    let mods = [
        (platform, "cmd"),
        (ctrl, "ctrl"),
        (alt, "alt"),
        (shift, "shift"),
    ];
    let mut parts: Vec<_> = mods.iter().filter(|(on, _)| *on).map(|(_, m)| *m).collect();
    parts.push(name);
    parts.join("+")
}
