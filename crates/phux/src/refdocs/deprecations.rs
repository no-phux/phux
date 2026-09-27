//! The generated deprecations reference, rendered from `crate::deprecations`
//! — the same rows the binary-level audit runs.

use crate::deprecations::DEPRECATED;

use super::Page;

/// Render `docs/reference/deprecations.md`.
pub(crate) fn page() -> Page {
    use std::fmt::Write as _;

    let mut body = String::from(
        "Every deprecated spelling this build of the binary still \
         accepts. Each one parses with its full argument surface and runs \
         its replacement's implementation; the differences from the old \
         behavior are exactly three, and a binary-level test pins each of \
         them per row:\n\n\
         1. one warning line on stderr naming the replacement — \
            suppressed under `--json`, where stdout carries only the \
            document and stderr is reserved for the one-line error \
            contract;\n\
         2. absence from `--help`;\n\
         3. absence from the generated shell completions.\n\n\
         A deprecated spelling survives at least one full release cycle \
         with the warning in place; the planned-removal release is the \
         earliest it can disappear. Move scripts to the replacement before \
         then.\n\n\
         | Deprecated spelling | Use instead | Deprecated in | Planned removal |\n\
         |---|---|---|---|\n",
    );

    // A slice pattern rather than `is_empty()`, which clippy flags as
    // always-true on a `const` while the table has rows.
    match DEPRECATED {
        [] => body.push_str("\nNo spelling is currently deprecated.\n"),
        [first, ..] => {
            for row in DEPRECATED {
                let _ = writeln!(
                    body,
                    "| `{}` | `{}` | {} | {} |",
                    row.old, row.new, row.deprecated_in, row.removed_in
                );
            }
            let _ = write!(
                body,
                "\nThe warning is one greppable stderr line per invocation, of \
                 the form:\n\n```text\n{}\n```\n",
                first.note
            );
        }
    }

    Page {
        file: "deprecations.md",
        title: "phux deprecations reference",
        summary: "Every deprecated spelling, its replacement, and its \
                  removal release.",
        tldr: "Deprecated spellings the current binary still accepts, \
               each pinned with its replacement and lifecycle releases; \
               empty when nothing is currently deprecated. Every row still \
               parses, warns once on stderr with its replacement, is \
               hidden from help and completions, and is scheduled for \
               removal one release cycle or more after deprecation.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::{DEPRECATED, page};

    /// One row per deprecation, each naming both lifecycle releases.
    #[test]
    fn deprecations_page_has_a_row_per_table_entry() {
        let body = page().body;
        let rows: Vec<&str> = body
            .lines()
            .filter(|line| line.starts_with("| `"))
            .collect();
        assert_eq!(rows.len(), DEPRECATED.len());
        for (row, line) in DEPRECATED.iter().zip(&rows) {
            assert!(
                line.starts_with(&format!("| `{}` | `{}` |", row.old, row.new)),
                "no row for {}",
                row.old
            );
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            assert!(
                cells[3].starts_with('v') && cells[4].starts_with('v'),
                "both release cells must carry a version: {line}"
            );
        }
    }
}
