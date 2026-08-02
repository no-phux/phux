//! Hidden `phux gen-reference-docs` — write the generated reference pages.
//!
//! Developer tooling behind `just docs-gen`, deliberately not part of the
//! user-facing CLI surface (the subcommand is `hide = true`). It exists as
//! a subcommand rather than an xtask or build script because the pages are
//! a pure function of this binary's own inventories: the clap tree is
//! already compiled in, an xtask would compile the dependency graph a
//! second time, and a build script must not write into the source tree.
//! Rationale in `ADR/0069-generated-reference-docs.md`.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::refdocs;

/// Default output directory, relative to the current working directory.
/// `just docs-gen` runs from the repo root, which is the tree the
/// freshness test in `refdocs` byte-compares against.
const DEFAULT_OUT: &str = "docs/reference";

/// Render every registered page into `out` (default: the checkout's
/// generated-reference tree). Idempotent: an up-to-date file is left
/// untouched and reported as unchanged.
pub(crate) fn run_gen_reference_docs(out: Option<PathBuf>) -> ExitCode {
    let out = out.unwrap_or_else(|| PathBuf::from(DEFAULT_OUT));
    if let Err(err) = std::fs::create_dir_all(&out) {
        eprintln!("phux: cannot create {}: {err}", out.display());
        return ExitCode::FAILURE;
    }
    for page in refdocs::pages() {
        let path = out.join(page.file);
        let rendered = page.render();
        if std::fs::read(&path).is_ok_and(|bytes| bytes == rendered.as_bytes()) {
            outln!("unchanged {}", path.display());
            continue;
        }
        if let Err(err) = std::fs::write(&path, rendered) {
            eprintln!("phux: cannot write {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
        outln!("wrote {}", path.display());
    }
    ExitCode::SUCCESS
}
