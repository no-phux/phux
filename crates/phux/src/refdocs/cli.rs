//! The generated CLI reference: one section per non-hidden invocation path,
//! each carrying the long help the binary renders for it. The live crate
//! version is stripped from the root banner so a release bump does not
//! fail the freshness test on open PRs.
//!
//! The walk mirrors `help_inventory`'s `collect_paths` with one deliberate
//! difference: hidden subcommands are skipped. `--help` does not show them,
//! so the published reference must not either — that keeps deprecated
//! aliases and internal tooling (including the generator itself) out of the
//! user-facing pages while the inventory snapshot still pins them.

use super::Page;
use crate::Cli;

fn collect_visible<'a>(
    meta: &'a usage::spec::CommandMeta<'a>,
    path: &str,
    out: &mut Vec<(String, &'a usage::spec::CommandMeta<'a>)>,
) {
    out.push((path.to_owned(), meta));
    for sub in meta.subcommands {
        if sub.hide || sub.cmd.name == "help" {
            continue;
        }
        let child = format!("{path} {}", sub.cmd.name);
        collect_visible(sub, &child, out);
    }
}

/// Render `docs/reference/cli.md`.
pub(crate) fn page() -> Page {
    use std::fmt::Write as _;

    let mut entries = Vec::new();
    collect_visible(Cli::spec().root, "phux", &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut body = String::from(
        "Each section below is the `--help` text for one invocation path, \
         rendered by the same argument parser the binary runs — flags, \
         defaults, value names, and descriptions here are the ones the \
         binary enforces. The root page omits the live crate version that \
         `--help` prints (`phux <version>`), so a release bump does not \
         churn this file. Hidden internal subcommands are omitted, exactly \
         as they are from `--help` itself.\n\n",
    );
    for (path, cmd) in &entries {
        let help =
            crate::render_help_page(cmd.cmd, true, usage::help::Style::PLAIN).unwrap_or_default();
        // Preserve every visible byte of help while keeping generated
        // Markdown free of trailing whitespace. Drop the live crate version
        // from the root banner so a release-please bump cannot fail the
        // freshness test on every open PR (phux-k0sj). `phux --help` still
        // prints it.
        let help = help
            .lines()
            .map(str::trim_end)
            .map(without_crate_version_banner)
            .collect::<Vec<_>>()
            .join("\n");
        let _ = write!(body, "## `{path}`\n\n```text\n{}\n```\n\n", help.trim_end());
    }

    Page {
        file: "cli.md",
        title: "phux CLI reference",
        summary: "Every non-hidden `phux` invocation path with its flags, \
                  defaults, and help text.",
        tldr: "The complete `phux` command surface: one section per \
               invocation path, each carrying the exact long help of the \
               binary that generated it. Rendered from the argument parser \
               itself, so the flags, defaults, and descriptions shown here \
               are the ones the binary enforces.",
        body,
    }
}

/// Replace the root `--help` banner (`phux <crate version>`, including a
/// next-channel label) with the program name. Any other line is returned
/// unchanged.
fn without_crate_version_banner(line: &str) -> &str {
    if line == crate::BANNER { "phux" } else { line }
}

#[cfg(test)]
mod tests {
    use super::page;

    /// The reference covers the visible tree end to end — the root, a
    /// nested leaf from each namespace depth — and carries fully-qualified
    /// usage lines (the `build()` requirement documented on `page`).
    #[test]
    fn cli_page_covers_visible_paths_with_qualified_usage() {
        let page = page();
        for heading in [
            "## `phux`\n",
            "## `phux agent set`\n",
            "## `phux config reload`\n",
            "## `phux worktree remove`\n",
        ] {
            assert!(
                page.body.contains(heading),
                "generated CLI reference lost the {heading:?} section"
            );
        }
        assert!(
            page.body.contains("Usage: phux agent set"),
            "subcommand usage lines must carry the full `phux …` path"
        );
    }

    /// Hidden subcommands are absent from `--help`, so they must be absent
    /// here too — the generator itself is the canary.
    #[test]
    fn cli_page_omits_hidden_subcommands() {
        let page = page();
        assert!(
            !page.body.contains("gen-reference-docs"),
            "hidden subcommands must stay out of the generated CLI reference"
        );
    }

    /// The live crate version must not appear in the generated page. A
    /// release-please bump of `CARGO_PKG_VERSION` would otherwise fail
    /// `generated_reference_docs_match_the_tree` on every open PR whose
    /// checked-in `cli.md` still has the prior banner (phux-k0sj).
    #[test]
    fn cli_page_omits_the_live_crate_version() {
        let rendered = page().render();
        assert!(
            !rendered.contains(crate::BANNER),
            "generated CLI reference must not embed the version banner"
        );
        assert!(
            !rendered.contains(env!("CARGO_PKG_VERSION")),
            "generated CLI reference must not embed CARGO_PKG_VERSION"
        );
        assert!(
            rendered.contains("```text\nphux\n"),
            "root help must still open with the program name"
        );
    }
}
