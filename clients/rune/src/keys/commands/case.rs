//! The chrome's one casing for in-app labels: sentence case.

/// Proper nouns and product names a sentence-cased label keeps capitalised.
pub(super) const PROPER: &[&str] = &[
    "Galaxy", "Reef", "Pitch", "Kitchen", "Pit", "Lane", "Hive", "Quick", "Look", "Mac", "Linux",
    "Ghostty", "Omarchy", "Rune", "phux",
];

/// `title` in sentence case, the chrome's one casing for commands and
/// labels: the first word capitalised, every later word lower case except
/// acronyms (`TOML`) and proper nouns. macOS menu bar items are the one
/// Title Case exception, and keep [`Command::title`].
pub fn sentence_case(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for (i, word) in title.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let bare = word.trim_matches(|c: char| !c.is_alphanumeric());
        let acronym = bare.len() > 1 && bare.chars().all(|c| !c.is_lowercase());
        if i == 0 || acronym || PROPER.contains(&bare) {
            out.push_str(word);
            continue;
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_lowercase());
            out.push_str(chars.as_str());
        }
    }
    out
}
