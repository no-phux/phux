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

/// Words that name a kind, a kind's unit, or its concepts. Lowercased.
const KIND_WORDS: [&str; 3] = ["terminal", "agent", "pane"];

/// `AgentEvent` is the wire frame the event sink carries; the name predates
/// the substrate and is not an agent concept.
const WIRE_NAMES: [&str; 1] = ["AgentEvent"];

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

/// The kind words on `line`, ignoring wire names.
fn kind_words(line: &str) -> Vec<&'static str> {
    let cleaned = WIRE_NAMES
        .iter()
        .fold(line.to_owned(), |line, name| line.replace(name, ""))
        .to_lowercase();
    KIND_WORDS
        .into_iter()
        .filter(|word| cleaned.contains(word))
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
}
