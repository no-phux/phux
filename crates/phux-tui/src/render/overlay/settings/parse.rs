//! Parsing the value editor's text, and text layout helpers.

use super::{Color, Edit, FromStr, Path, SettingKind, home_to_tilde};

/// Parse the editor's text into an edit for `kind`.
pub(super) fn parse_input(kind: SettingKind, text: &str) -> Result<Edit, String> {
    if accepts_unset(kind) && is_unset_word(text) {
        return Ok(Edit::Unset);
    }
    match kind {
        SettingKind::Bool => parse_bool_edit(text, "expected true or false"),
        SettingKind::OptionalBool => parse_bool_edit(text, "expected true, false, or unset"),
        SettingKind::Integer { min, max } | SettingKind::OptionalInteger { min, max } => {
            parse_int(text, min, max)
        }
        SettingKind::Text | SettingKind::Chord => {
            require_non_empty(text)?;
            Ok(set_string(text))
        }
        SettingKind::OptionalText => Ok(set_string(text)),
        SettingKind::Choice(variants) => parse_choice(text, variants),
        SettingKind::Argv => {
            let words = split_words(text)?;
            Ok(Edit::Set(toml::Value::Array(
                words.into_iter().map(toml::Value::String).collect(),
            )))
        }
        SettingKind::Color => {
            require_non_empty(text)?;
            Color::from_str(text)
                .map(|_| set_string(text))
                .map_err(|_| "not a color: use a name, #rrggbb, or an index 0-255".to_owned())
        }
    }
}

/// Kinds whose editor treats empty text or `unset` as removing the key.
pub(super) const fn accepts_unset(kind: SettingKind) -> bool {
    matches!(
        kind,
        SettingKind::OptionalBool
            | SettingKind::OptionalInteger { .. }
            | SettingKind::OptionalText
            | SettingKind::Argv
    )
}

pub(super) const fn is_unset_word(text: &str) -> bool {
    text.is_empty() || text.eq_ignore_ascii_case("unset")
}

pub(super) fn require_non_empty(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Err("cannot be empty; use Del to fall back to the shipped default".to_owned());
    }
    Ok(())
}

pub(super) fn set_string(text: &str) -> Edit {
    Edit::Set(toml::Value::String(text.to_owned()))
}

pub(super) fn parse_bool_edit(text: &str, expected: &str) -> Result<Edit, String> {
    parse_bool(text)
        .map(|b| Edit::Set(toml::Value::Boolean(b)))
        .ok_or_else(|| expected.to_owned())
}

/// Match `text` case-insensitively against the choice's variants.
pub(super) fn parse_choice(text: &str, variants: &[&str]) -> Result<Edit, String> {
    let lowered = text.to_lowercase();
    variants
        .iter()
        .find(|v| **v == lowered)
        .map(|v| set_string(v))
        .ok_or_else(|| format!("expected one of {}", variants.join(", ")))
}

pub(super) fn parse_bool(text: &str) -> Option<bool> {
    match text.to_ascii_lowercase().as_str() {
        "true" | "on" | "yes" | "1" => Some(true),
        "false" | "off" | "no" | "0" => Some(false),
        _ => None,
    }
}

pub(super) fn parse_int(text: &str, min: i64, max: i64) -> Result<Edit, String> {
    let value: i64 = text
        .replace('_', "")
        .parse()
        .map_err(|_| format!("expected an integer between {min} and {max}"))?;
    if value < min || value > max {
        return Err(format!("must be between {min} and {max}"));
    }
    Ok(Edit::Set(toml::Value::Integer(value)))
}

/// Split a command line into words the way a POSIX shell would for the
/// common cases: whitespace separates, single quotes are literal, double
/// quotes allow `\"` and `\\`, a backslash escapes the next character.
pub(super) fn split_words(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            if in_word {
                words.push(std::mem::take(&mut current));
                in_word = false;
            }
            continue;
        }
        in_word = true;
        match c {
            '\'' => read_single_quoted(&mut chars, &mut current)?,
            '"' => read_double_quoted(&mut chars, &mut current)?,
            '\\' => current.push(chars.next().ok_or("trailing backslash")?),
            c => current.push(c),
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

/// Consume a single-quoted span through its closing quote: every character
/// is literal.
pub(super) fn read_single_quoted(
    chars: &mut std::str::Chars<'_>,
    out: &mut String,
) -> Result<(), String> {
    loop {
        match chars.next() {
            Some('\'') => return Ok(()),
            Some(ch) => out.push(ch),
            None => return Err("unterminated single quote".to_owned()),
        }
    }
}

/// Consume a double-quoted span through its closing quote: a backslash
/// escapes `"`, `\`, `$`, and `` ` `` and is kept literally before anything
/// else.
pub(super) fn read_double_quoted(
    chars: &mut std::str::Chars<'_>,
    out: &mut String,
) -> Result<(), String> {
    pub(super) const UNTERMINATED: &str = "unterminated double quote";
    loop {
        match chars.next().ok_or(UNTERMINATED)? {
            '"' => return Ok(()),
            '\\' => match chars.next().ok_or(UNTERMINATED)? {
                esc @ ('"' | '\\' | '$' | '`') => out.push(esc),
                other => {
                    out.push('\\');
                    out.push(other);
                }
            },
            ch => out.push(ch),
        }
    }
}

/// The header's path: `~` for the home directory, so the header fits.
pub(super) fn display_path_for(path: &Path, home: Option<&str>) -> String {
    home_to_tilde(&path.display().to_string(), home)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn split_words_handles_quotes_and_escapes() {
        assert_eq!(
            split_words("curl -sf 'a b' \"c \\\" d\" e\\ f").unwrap(),
            vec!["curl", "-sf", "a b", "c \" d", "e f"]
        );
        assert_eq!(split_words("   ").unwrap(), Vec::<String>::new());
        assert!(split_words("'open").is_err());
        assert!(split_words("trailing\\").is_err());
        // An empty quoted span is still a word; quotes join adjacent text.
        assert_eq!(split_words("a '' b").unwrap(), vec!["a", "", "b"]);
        assert_eq!(split_words("x'y z'\"w\"").unwrap(), vec!["xy zw"]);
        // Inside double quotes only `"`, `\`, `$`, and `` ` `` are escapes.
        assert_eq!(split_words(r#""\$ \` \n""#).unwrap(), vec![r"$ ` \n"]);
        assert_eq!(
            split_words("\"open").unwrap_err(),
            "unterminated double quote"
        );
        assert_eq!(
            split_words("\"open\\").unwrap_err(),
            "unterminated double quote"
        );
        assert_eq!(
            split_words("'open").unwrap_err(),
            "unterminated single quote"
        );
        assert_eq!(split_words("x\\").unwrap_err(), "trailing backslash");
    }

    #[test]
    fn parse_input_covers_every_kind() {
        assert_eq!(
            parse_input(SettingKind::Bool, "on"),
            Ok(Edit::Set(toml::Value::Boolean(true)))
        );
        assert!(parse_input(SettingKind::Bool, "maybe").is_err());
        assert_eq!(parse_input(SettingKind::OptionalBool, ""), Ok(Edit::Unset));
        assert_eq!(
            parse_input(SettingKind::Integer { min: 0, max: 10 }, "7"),
            Ok(Edit::Set(toml::Value::Integer(7)))
        );
        assert!(parse_input(SettingKind::Integer { min: 0, max: 10 }, "11").is_err());
        assert_eq!(
            parse_input(SettingKind::OptionalInteger { min: 1, max: 9 }, "unset"),
            Ok(Edit::Unset)
        );
        assert_eq!(
            parse_input(SettingKind::Choice(&["left", "right"]), "RIGHT"),
            Ok(Edit::Set(toml::Value::String("right".to_owned())))
        );
        assert!(parse_input(SettingKind::Choice(&["left", "right"]), "up").is_err());
        assert!(parse_input(SettingKind::Text, "").is_err());
        assert_eq!(parse_input(SettingKind::OptionalText, ""), Ok(Edit::Unset));
        assert!(parse_input(SettingKind::Color, "#12345").is_err());
        assert!(parse_input(SettingKind::Color, "cyan").is_ok());
        // Unset words apply only to the optional kinds and argv.
        assert_eq!(parse_input(SettingKind::Argv, "UNSET"), Ok(Edit::Unset));
        assert_eq!(
            parse_input(SettingKind::Argv, "a 'b c'"),
            Ok(Edit::Set(toml::Value::Array(vec![
                toml::Value::String("a".to_owned()),
                toml::Value::String("b c".to_owned()),
            ])))
        );
        assert_eq!(
            parse_input(SettingKind::Text, "unset"),
            Ok(Edit::Set(toml::Value::String("unset".to_owned())))
        );
        assert_eq!(
            parse_input(SettingKind::Bool, "").unwrap_err(),
            "expected true or false"
        );
        assert_eq!(
            parse_input(SettingKind::OptionalBool, "maybe").unwrap_err(),
            "expected true, false, or unset"
        );
        assert!(parse_input(SettingKind::Integer { min: 0, max: 9 }, "unset").is_err());
        assert!(parse_input(SettingKind::Chord, "").is_err());
        assert_eq!(
            parse_input(SettingKind::Color, "").unwrap_err(),
            "cannot be empty; use Del to fall back to the shipped default"
        );
        assert_eq!(
            parse_input(SettingKind::Choice(&["left", "right"]), "up").unwrap_err(),
            "expected one of left, right"
        );
    }
}
