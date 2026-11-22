//! Hidden `phux gen-reference-docs` — write the generated reference pages.
//!
//! Developer tooling behind `just docs-gen`, deliberately not part of the
//! user-facing CLI surface (the subcommand is `hide`). It exists as
//! a subcommand rather than an xtask or build script because the pages are
//! a pure function of this binary's own inventories: the clap tree is
//! already compiled in, an xtask would compile the dependency graph a
//! second time, and a build script must not write into the source tree.
//! Rationale in [ADR-0069](https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0069-generated-reference-docs.md).

use std::path::PathBuf;
use std::process::ExitCode;

use crate::refdocs;

/// Default output directory, relative to the current working directory.
/// `just docs-gen` runs from the repo root, which is the tree the
/// freshness test in `refdocs` byte-compares against.
const DEFAULT_OUT: &str = "docs/reference";

/// Render every registered page into `out` (default: the checkout's
/// generated-reference tree), and the editor schemas into `spec/` beside it.
/// Idempotent: an up-to-date file is left untouched and reported as
/// unchanged.
pub(crate) fn run_gen_reference_docs(out: Option<PathBuf>) -> ExitCode {
    let out = out.unwrap_or_else(|| PathBuf::from(DEFAULT_OUT));
    let spec = out.parent().unwrap_or(&out).join("spec");
    let schemas = [
        (
            "config.schema.json",
            phux_config::json_schema::config_schema(),
        ),
        (
            "plugin.schema.json",
            phux_config::json_schema::plugin_schema(),
        ),
    ];
    let files = refdocs::pages()
        .into_iter()
        .map(|page| (out.join(page.file), page.render()))
        .chain(schemas.map(|(file, schema)| (spec.join(file), schema)));
    for (path, rendered) in files {
        if let Some(dir) = path.parent()
            && let Err(err) = std::fs::create_dir_all(dir)
        {
            error!("cannot create {}: {err}", dir.display());
            return ExitCode::FAILURE;
        }
        if std::fs::read(&path).is_ok_and(|bytes| bytes == rendered.as_bytes()) {
            outln!("unchanged {}", path.display());
            continue;
        }
        if let Err(err) = std::fs::write(&path, rendered) {
            error!("cannot write {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
        outln!("wrote {}", path.display());
    }
    ExitCode::SUCCESS
}

/// Run the hidden `phux gen-reference-docs` as parsed.
pub(crate) fn run(args: crate::commands::GenReferenceDocsArgs) -> ExitCode {
    run_gen_reference_docs(args.out)
}
