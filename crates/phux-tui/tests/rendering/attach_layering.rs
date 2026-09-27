//! Layering guard: the attach driver is a one-way orchestrator. Shared
//! vocabulary lives in `attach/pane_state.rs` and `attach/outcome.rs`; one
//! `use super::driver::Foo;` in a sibling would silently reopen a module
//! cycle. No file under `src/` may name `driver::` in code except the driver
//! itself and `attach/mod.rs`. The comment filter is deliberately naive (a
//! trimmed line starting with `//`).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::fs;
use std::path::{Path, PathBuf};

/// Files besides the driver allowed to name `driver::` in code.
const EXEMPT: &[&str] = &["attach/mod.rs"];

const RULE: &str = "\
attach/driver is a one-way orchestrator: nothing else may depend on it. Move
shared vocabulary to attach/pane_state.rs or attach/outcome.rs instead.";

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
        if EXEMPT.contains(&rel.as_str()) || rel.starts_with("attach/driver/") {
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
