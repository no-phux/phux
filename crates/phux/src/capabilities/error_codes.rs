//! The `error_codes` catalogue in `phux --capabilities --json`: every stable
//! `error.code` a `--json` failure can carry, with what it means, read from
//! the sources that define them so the list cannot fall behind.

use serde_json::{Value, json};

/// Source files whose `&str` constants are `--json` error codes.
const SOURCES: [&str; 2] = [
    include_str!("../commands/json_err/codes.rs"),
    include_str!("../../../phux-client/src/spatial.rs"),
];

/// `[{ "code", "meaning" }]`, sorted by code. A code two sources share keeps
/// the first meaning.
pub(super) fn catalogue() -> Value {
    let mut entries: Vec<(String, String)> = Vec::new();
    for source in SOURCES {
        for (code, meaning) in parse(source) {
            if !entries.iter().any(|(seen, _)| *seen == code) {
                entries.push((code, meaning));
            }
        }
    }
    entries.sort();
    Value::Array(
        entries
            .into_iter()
            .map(|(code, meaning)| json!({ "code": code, "meaning": meaning }))
            .collect(),
    )
}

/// Each documented `const NAME: &str = "code";` in `source`, with its doc
/// comment joined into one sentence run.
fn parse(source: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut doc: Vec<&str> = Vec::new();
    for line in source.lines().map(str::trim) {
        if let Some(text) = line.strip_prefix("///") {
            doc.push(text.trim());
            continue;
        }
        if let Some(code) = code_literal(line)
            && !doc.is_empty()
        {
            found.push((code.to_owned(), doc.join(" ")));
        }
        doc.clear();
    }
    found
}

/// The string literal of a `pub[(crate)] const NAME: &str = "code";` line.
fn code_literal(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("pub(crate) const ")
        .or_else(|| line.strip_prefix("pub const "))?;
    let (_, value) = rest.split_once(": &str = \"")?;
    value.strip_suffix("\";")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test fixture assertions")]
mod tests {
    use super::*;

    /// Every constant in `codes.rs` is catalogued once, with a meaning.
    #[test]
    fn every_cli_code_is_catalogued_with_a_meaning() {
        let catalogue = catalogue();
        let entries = catalogue.as_array().unwrap();
        let declared = SOURCES[0]
            .lines()
            .filter(|line| code_literal(line.trim()).is_some())
            .count();
        let cli: Vec<_> = parse(SOURCES[0]);
        assert_eq!(cli.len(), declared, "an undocumented code in codes.rs");
        for (code, _) in &cli {
            let entry = entries.iter().find(|entry| entry["code"] == code.as_str());
            let entry = entry.unwrap_or_else(|| panic!("{code} missing"));
            assert!(!entry["meaning"].as_str().unwrap().is_empty(), "{code}");
        }
        let mut codes: Vec<_> = entries.iter().map(|e| e["code"].clone()).collect();
        codes.dedup();
        assert_eq!(codes.len(), entries.len(), "codes are unique");
    }

    #[test]
    fn spatial_codes_are_included() {
        let catalogue = catalogue();
        assert!(
            catalogue
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["code"] == "selector_miss")
        );
    }

    #[test]
    fn a_multi_line_doc_is_one_meaning() {
        let parsed = parse(
            "/// A wait gave up\n/// before it held.\npub(crate) const TIMED_OUT: &str = \"timed_out\";\nconst X: u8 = 1;\n",
        );
        assert_eq!(
            parsed,
            [(
                "timed_out".to_owned(),
                "A wait gave up before it held.".to_owned()
            )]
        );
    }
}
