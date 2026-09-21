//! The generated CLI/MCP parity reference, rendered from the MCP tool table
//! (`crates/phux-mcp/src/tool_table.rs`), compiled in here with `#[path]` so
//! the `phux` binary renders the page without linking the MCP stack.
//!
//! The same table annotates the adapter's catalog and is held to the live
//! catalog, the CLI grammar, and the kind table by the parity gate
//! (`crates/phux-mcp/tests/parity.rs`); this page is its human rendering.

use std::fmt::Write as _;

use super::Page;

#[allow(
    clippy::redundant_pub_crate,
    reason = "the table is written for the adapter crate, where pub(crate) is load-bearing"
)]
#[path = "../../../phux-mcp/src/tool_table.rs"]
mod tool_table;

use tool_table::{CLI_ONLY, DESTRUCTIVE_SOURCE, Exec, Surface, TOOLS};

/// Render `docs/reference/parity.md`.
pub(crate) fn page() -> Page {
    let mut body = String::from(
        "Every tool the MCP adapter (`phux mcp`) serves, the `phux` verb it \
         mirrors, and how it runs. `in-process` tools spawn no subprocess \
         and return the document the CLI verb prints, built by the same \
         `phux-client` function. The CLI residue runs the \
         canonical `phux` binary with argv, never a shell, for the reason \
         listed. The parity gate holds this table to the live tool catalog, \
         the CLI grammar, and the kind table, so it cannot drift from the \
         adapter.\n\n\
         | Tool | CLI verb | Runs | Read-only | Destructive |\n\
         |---|---|---|---|---|\n",
    );
    for row in TOOLS {
        let hints = tool_table::hints(row.touches).unwrap_or(tool_table::UNMAPPED);
        let _ = writeln!(
            body,
            "| `{}` | {} | {} | {} | {} |",
            row.name,
            verb(row.surface),
            match row.exec {
                Exec::InProcess => "in-process",
                Exec::Cli(_) => "CLI",
            },
            yes_no(hints.read_only),
            yes_no(hints.destructive),
        );
    }
    push_residue(&mut body);
    push_automation_only(&mut body);
    push_cli_only(&mut body);
    let _ = write!(
        body,
        "\n## Annotation sources\n\n`Read-only` is the kind table's \
         `mutating` rule: a tool is read-only only when no method it can send \
         changes server state. `Destructive` comes from {DESTRUCTIVE_SOURCE}.\n"
    );

    Page {
        file: "parity.md",
        title: "phux CLI/MCP parity reference",
        summary: "Every MCP tool, the CLI verb it mirrors, how it runs, and its annotations.",
        tldr: "Every MCP tool the adapter serves, mapped to the `phux` verb it \
               mirrors (or why it has none), whether it runs in-process or \
               through the CLI residue and why, and its read-only and \
               destructive annotations. Rendered from the tool table the \
               parity gate checks, so it cannot drift from the adapter.",
        body,
    }
}

fn verb(surface: Surface) -> String {
    match surface {
        Surface::Cli(verb) => format!("`phux {verb}`"),
        Surface::AutomationOnly(_) => "none (automation only)".to_owned(),
    }
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn push_residue(body: &mut String) {
    body.push_str(
        "\n## CLI residue\n\nThese tools still run the `phux` binary as a \
         subprocess, bounded in time and output:\n\n",
    );
    for row in TOOLS {
        if let Exec::Cli(reason) = row.exec {
            let _ = writeln!(body, "- `{}`: {reason}.", row.name);
        }
    }
}

fn push_automation_only(body: &mut String) {
    body.push_str("\n## Automation-only tools\n\n");
    for row in TOOLS {
        if let Surface::AutomationOnly(reason) = row.surface {
            let _ = writeln!(body, "- `{}`: {reason}.", row.name);
        }
    }
}

fn push_cli_only(body: &mut String) {
    body.push_str(
        "\n## CLI verbs without a tool\n\nAgent-facing verbs in the JSON index \
         of `docs/consumers/agents.md` that have no MCP tool yet:\n\n",
    );
    for (verb, reason) in CLI_ONLY {
        let _ = writeln!(body, "- `phux {verb}`: {reason}.");
    }
}

#[cfg(test)]
mod tests {
    use super::page;
    use super::tool_table::{CLI_ONLY, Surface, TOOLS};
    use crate::Cli;

    fn collect(meta: &usage::spec::CommandMeta<'_>, path: &str, out: &mut Vec<String>) {
        out.push(path.to_owned());
        for sub in meta.subcommands {
            let child = if path.is_empty() {
                sub.cmd.name.to_owned()
            } else {
                format!("{path} {}", sub.cmd.name)
            };
            collect(sub, &child, out);
        }
    }

    /// Every verb the table names is a real invocation path in the live
    /// usage spec (not only in the generated reference the MCP gate reads).
    #[test]
    fn every_mirrored_verb_is_in_the_live_grammar() {
        let mut paths = Vec::new();
        collect(Cli::spec().root, "", &mut paths);
        for row in TOOLS {
            if let Surface::Cli(verb) = row.surface {
                assert!(
                    paths.iter().any(|path| path == verb),
                    "{} mirrors `phux {verb}`, which the usage spec does not have",
                    row.name
                );
            }
        }
        for (verb, _) in CLI_ONLY {
            assert!(paths.iter().any(|path| path == verb), "CLI_ONLY `{verb}`");
        }
    }

    /// The page has one table row per tool.
    #[test]
    fn parity_page_lists_every_tool() {
        let body = page().body;
        for row in TOOLS {
            assert!(
                body.contains(&format!("| `{}` |", row.name)),
                "parity.md has no row for {}",
                row.name
            );
        }
    }
}
