//! The generated CLI reference: one section per non-hidden invocation path,
//! each carrying the exact long help the binary renders for it.
//!
//! The walk mirrors `help_inventory`'s `collect_paths` with one deliberate
//! difference: hidden subcommands are skipped. `--help` does not show them,
//! so the published reference must not either — that keeps deprecated
//! aliases and internal tooling (including the generator itself) out of the
//! user-facing pages while the inventory snapshot still pins them.

use clap::CommandFactory;

use super::Page;
use crate::Cli;

/// Recursively collect every visible command invocation path with its
/// (built) `clap::Command`, skipping clap's auto-injected `help`
/// pseudo-command and anything marked hidden.
fn collect_visible(cmd: &clap::Command, path: &str, out: &mut Vec<(String, clap::Command)>) {
    out.push((path.to_owned(), cmd.clone()));
    for sub in cmd.get_subcommands() {
        if sub.get_name() == "help" || sub.is_hide_set() {
            continue;
        }
        let child = format!("{path} {}", sub.get_name());
        collect_visible(sub, &child, out);
    }
}

/// Render `docs/reference/cli.md`.
pub(crate) fn page() -> Page {
    use std::fmt::Write as _;

    // `build()` finalizes the tree the way a real parse would — in
    // particular it propagates bin names, so a subcommand's usage line
    // reads `Usage: phux agent set …` instead of a bare `set …`.
    let mut root = Cli::command();
    root.build();

    let mut entries = Vec::new();
    collect_visible(&root, "phux", &mut entries);
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut body = String::from(
        "Each section below is the verbatim `--help` text for one \
         invocation path, rendered by the same argument parser the binary \
         runs — flags, defaults, value names, and descriptions here are the \
         ones the binary enforces. Hidden internal subcommands are omitted, \
         exactly as they are from `--help` itself.\n\n",
    );
    for (path, cmd) in &mut entries {
        let help = cmd.render_long_help().to_string();
        // clap indents the blank spacer before a possible-values block.
        // Preserve every visible byte of help while keeping generated
        // Markdown free of trailing whitespace.
        let help = help
            .lines()
            .map(str::trim_end)
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
}
