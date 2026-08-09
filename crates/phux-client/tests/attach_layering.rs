//! Layering guard: `attach/driver.rs` is a one-way orchestrator.
//!
//! `attach/driver.rs` owns the attach lifecycle — the connection, raw mode,
//! stdin, SIGWINCH, and the `tokio::select!` loop. For a long time it *also*
//! owned the vocabulary its siblings needed: `AttachError`, `AttachEnd`,
//! `PaneSlot`, the session-kernel alias, the VCS and attention indices, and a
//! handful of pure helpers. Every sibling that wanted one of those imported it
//! from the driver, and the module graph grew three mutual-dependency pairs
//! (`driver` <-> `server_frame`, `driver` <-> `paint`, `driver` <->
//! `input_dispatch`) plus a fourth with `crate::layout_ops`.
//!
//! Rust permits mutually recursive modules, so none of that was a compile
//! error. It was a reading problem: there was no order in which to read the
//! subsystem, and no way to state which module is downstream of which.
//! phux-4fbs.4 moved the shared vocabulary into two leaf modules —
//! `attach/outcome.rs` (the exit vocabulary) and `attach/pane_state.rs` (the
//! per-pane state and the indices over it) — leaving the driver with exactly
//! one production consumer, `attach/mod.rs`.
//!
//! Nothing about that arrangement is enforced by the compiler. One
//! `use super::driver::Foo;` in a sibling silently reopens a cycle, and the
//! cost does not show up until the next person tries to read the subsystem.
//! This file is what notices.
//!
//! # The rule
//!
//! No file under `crates/phux-client/src` may name `driver::` in code, with
//! two exemptions: `attach/driver.rs` itself (`super::driver::` inside the
//! driver would be odd but is not a cycle) and `attach/mod.rs`, which declares
//! the module and re-exports its entry points. Doc comments and line comments
//! are exempt everywhere — four of them legitimately point a reader at
//! `driver::RawModeGuard` and friends.
//!
//! # Known limitation
//!
//! The comment stripping is deliberately naive: a line is treated as a comment
//! when its trimmed form starts with `//`. A `driver::` buried in a block
//! comment or a string literal would slip past, and a trailing `// driver::`
//! after code on the same line would be flagged. Both are acceptable for a
//! layering guard — a real parser here would cost more than the invariant is
//! worth, and the failure modes are a missed catch and an easily-read false
//! positive, not a wrong build.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::fs;
use std::path::{Path, PathBuf};

/// Files allowed to name `driver::` in code, relative to `src/`.
const EXEMPT: &[&str] = &["attach/driver.rs", "attach/mod.rs"];

/// The invariant, spelled out in the failure message so the next person does
/// not have to reconstruct it from a grep.
const RULE: &str = "\
attach/driver.rs is a one-way orchestrator (phux-4fbs.4): it owns the attach
lifecycle and nothing else may depend on it. Shared vocabulary belongs in
attach/pane_state.rs (per-pane state, the session-kernel alias, the VCS and
attention indices) or attach/outcome.rs (AttachError / AttachEnd). Importing
from the driver reopens the mutual-recursion cycle this test exists to
prevent — move the item instead.";

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_module_depends_on_attach_driver() {
    let root = src_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(
        files.len() > 20,
        "source walk found only {} files; the scan is not looking where it thinks it is",
        files.len()
    );

    let mut offenders = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .expect("path under src")
            .to_string_lossy()
            .replace('\\', "/");
        if EXEMPT.contains(&rel.as_str()) {
            continue;
        }
        let body = fs::read_to_string(&path).expect("read source file");
        for (idx, line) in body.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if line.contains("driver::") {
                offenders.push(format!("{rel}:{}: {}", idx + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "{RULE}\n\nfound {} back-edge(s) into attach/driver.rs:\n  {}",
        offenders.len(),
        offenders.join("\n  ")
    );
}
