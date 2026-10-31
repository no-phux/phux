//! The host/app seam: only `runtime/apps/terminal` may reach the Terminal
//! engine. The compiler already keeps VT and PTY crates out of the host's
//! production code; this keeps the engine's own API out too.
//!
//! [`HOST_CROSSINGS`] is a ratchet of the crossings that remain. A new one
//! fails here: add it to the app instead. Removing one fails too, until its
//! row is lowered, so the table can only shrink. It counts per file, so a
//! swap of one crossing for another inside a listed file goes unseen; review
//! catches those.

use std::collections::BTreeMap;
use std::path::Path;

const TOKENS: [&str; 12] = [
    "phux_terminal",
    "terminal_actor",
    "resource::terminal",
    "TerminalResource",
    ".terminal()",
    "TerminalHandle",
    "TerminalActor",
    "TerminalInput",
    "crate::agent_detect",
    "crate::native_state",
    "crate::grid",
    "crate::input",
];

/// Host file, relative to `src/`, and its crossings.
const HOST_CROSSINGS: &[(&str, usize)] = &[
    ("hooks/mod.rs", 1),
    ("lib.rs", 14),
    ("runtime/checkout.rs", 3),
    ("runtime/commands.rs", 1),
    ("runtime/commands/attach_terminal.rs", 3),
    ("runtime/commands/lifecycle.rs", 9),
    ("runtime/config.rs", 2),
    ("runtime/connections/agent_state.rs", 6),
    ("runtime/connections/frames.rs", 6),
    ("runtime/connections/hello.rs", 1),
    ("runtime/connections/history.rs", 9),
    ("runtime/connections/lifecycle.rs", 6),
    ("runtime/connections/terminal_streams.rs", 13),
    ("runtime/native_agent/launch.rs", 4),
    ("runtime/organization/effects.rs", 4),
    ("runtime/program_status.rs", 2),
    ("runtime/pump.rs", 5),
    ("runtime/startup.rs", 5),
    ("runtime/upgrade/handoff.rs", 1),
    ("runtime/upgrade/mod.rs", 4),
    ("runtime/upload.rs", 3),
    ("state/config.rs", 2),
    ("state/defaults.rs", 1),
    ("state/mod.rs", 1),
    ("state/upgrade_blob.rs", 6),
    ("state/upgrade_blob/assemble.rs", 1),
    ("state/upgrade_blob/import.rs", 4),
    ("state/upgrade_blob/rebuild.rs", 4),
    ("state/viewport.rs", 4),
];

fn is_test_source(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.split('/').any(|part| part == "tests")
        || name == "tests.rs"
        || name == "test_support.rs"
        || name.starts_with("tests_")
        || name.ends_with("_tests.rs")
}

fn count(source: &str) -> usize {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(|line| {
            TOKENS
                .iter()
                .map(|token| line.matches(token).count())
                .sum::<usize>()
        })
        .sum()
}

fn walk(dir: &Path, root: &Path, found: &mut BTreeMap<String, usize>) {
    for entry in std::fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            walk(&path, root, found);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        let is_rust = path.extension().is_some_and(|ext| ext == "rs");
        if !is_rust || relative.starts_with("runtime/apps/terminal/") || is_test_source(&relative) {
            continue;
        }
        let crossings = count(&std::fs::read_to_string(&path).expect("read source"));
        if crossings > 0 {
            found.insert(relative, crossings);
        }
    }
}

#[test]
fn the_host_reaches_the_terminal_engine_only_through_its_app() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("set by the test runner");
    let src = Path::new(&manifest).join("src");
    let mut found = BTreeMap::new();
    walk(&src, &src, &mut found);
    let allowed: BTreeMap<String, usize> = HOST_CROSSINGS
        .iter()
        .map(|(path, crossings)| ((*path).to_owned(), *crossings))
        .collect();
    assert_eq!(
        found, allowed,
        "host crossings changed; move new ones into runtime/apps/terminal, or lower the ratchet"
    );
}
