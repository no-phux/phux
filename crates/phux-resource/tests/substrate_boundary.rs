//! The substrate stays kind-neutral: no crate of an app below it, and no
//! Terminal or agent concept in its production source. A kind's needs belong
//! in the app that serves it, reached through `ResourceOps` or a companion.

#![allow(
    clippy::expect_used,
    reason = "a test fails loudly on unreadable source"
)]

use std::path::{Path, PathBuf};

/// Crates that know what a kind is.
const FORBIDDEN_DEPENDENCIES: [&str; 3] =
    ["phux-agent-rules", "phux-agent-session", "phux-terminal"];

/// Words that name a kind, a kind's unit, or its concepts. Lowercased, and
/// matched against whole words of an identifier or comment.
const KIND_WORDS: [&str; 6] = ["terminal", "agent", "pane", "pty", "scrollback", "osc"];

/// Names the substrate may not rename. `AgentEvent` is the wire frame the
/// event sink carries; the rest are `ProcessHandoff` fields that mirror what
/// the upgrade blob persists.
const ALLOWED_NAMES: [&str; 4] = ["AgentEvent", "master_fd", "osc_title", "scrollback_bytes"];

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo and nextest"))
}

fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs")
            && path.file_name().is_some_and(|name| name != "tests.rs")
        {
            found.push(path);
        }
    }
}

/// The part of `source` before its inline `cfg(test)` module, if any.
fn production_part(source: &str) -> &str {
    source
        .find("#[cfg(test)]\nmod tests {")
        .map_or(source, |end| &source[..end])
}

/// The lowercased words of `line`, splitting `snake_case` and `CamelCase`.
fn words(line: &str) -> Vec<String> {
    let mut words = vec![String::new()];
    let mut previous_lower = false;
    for character in line.chars() {
        if !character.is_alphanumeric() || (character.is_uppercase() && previous_lower) {
            words.push(String::new());
        }
        previous_lower = character.is_lowercase();
        if character.is_alphanumeric() {
            words
                .last_mut()
                .expect("seeded")
                .extend(character.to_lowercase());
        }
    }
    words
}

/// The kind words on `line`, ignoring allowed names; a plural counts.
fn kind_words(line: &str) -> Vec<&'static str> {
    let cleaned = ALLOWED_NAMES
        .iter()
        .fold(line.to_owned(), |line, name| line.replace(name, ""));
    let words = words(&cleaned);
    KIND_WORDS
        .into_iter()
        .filter(|kind| {
            words
                .iter()
                .any(|word| word.strip_suffix('s').unwrap_or(word) == *kind)
        })
        .collect()
}

#[test]
fn the_manifest_depends_on_no_app() {
    let manifest = std::fs::read_to_string(manifest_dir().join("Cargo.toml")).expect("manifest");
    let table: toml::Table = manifest.parse().expect("valid manifest");
    for section in ["dependencies", "dev-dependencies"] {
        let Some(dependencies) = table.get(section).and_then(toml::Value::as_table) else {
            continue;
        };
        for forbidden in FORBIDDEN_DEPENDENCIES {
            assert!(
                !dependencies.contains_key(forbidden),
                "phux-resource must not depend on {forbidden}"
            );
        }
    }
    let words: Vec<_> = manifest.lines().flat_map(kind_words).collect();
    assert!(words.is_empty(), "kind concepts in the manifest: {words:?}");
}

#[test]
fn production_source_names_no_kind_concept() {
    let root = manifest_dir().join("src");
    let mut sources = Vec::new();
    rust_sources(&root, &mut sources);
    assert!(!sources.is_empty(), "no sources under {}", root.display());
    let mut hits = Vec::new();
    for path in sources {
        let source = std::fs::read_to_string(&path).expect("read source");
        for (index, line) in production_part(&source).lines().enumerate() {
            for word in kind_words(line) {
                hits.push(format!("{}:{}: {word}", path.display(), index + 1));
            }
        }
    }
    assert!(hits.is_empty(), "kind concepts in the substrate: {hits:#?}");
}

#[test]
fn the_word_check_ignores_wire_names_only() {
    assert!(kind_words("event: AgentEvent,").is_empty());
    assert_eq!(kind_words("fn terminal(&self)"), ["terminal"]);
    assert_eq!(kind_words("pub agent: AgentSession"), ["agent"]);
    assert_eq!(kind_words("/// one Pane"), ["pane"]);
    assert_eq!(kind_words("struct PtyEvent"), ["pty"]);
    assert_eq!(kind_words("our panes"), ["pane"]);
    assert_eq!(kind_words("osc_sequence"), ["osc"]);
    assert_eq!(kind_words("pub scrollback: Vec<u8>"), ["scrollback"]);
    assert!(kind_words("empty panel").is_empty());
    assert!(kind_words("master_fd osc_title scrollback_bytes").is_empty());
}
