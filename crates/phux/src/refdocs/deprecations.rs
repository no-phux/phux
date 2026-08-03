//! The generated deprecations reference, rendered from the canonical
//! table in `crate::deprecations` — the same rows the binary-level audit
//! in `tests/deprecated_aliases.rs` runs one by one, so the page cannot
//! list a spelling the binary no longer warns for, nor omit one it does.

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
        DEPRECATED[0].note
    );

    Page {
        file: "deprecations.md",
        title: "phux deprecations reference",
        summary: "Every deprecated spelling, its replacement, and its \
                  removal release.",
        tldr: "Deprecated spellings the binary still accepts: the \
               machine-registry verbs (`remote`, `satellite`, top-level \
               `enroll`) absorbed into `phux host`, and the \
               `--horizontal`/`--vertical` booleans absorbed into \
               `--split`. Each still parses, warns once on stderr with \
               its replacement, is hidden from help and completions, and \
               is scheduled for removal one release cycle or more after \
               deprecation.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::{DEPRECATED, page};

    /// One table row per deprecation, and no extras — the one-table
    /// contract of phux-i0e8.13.4.
    #[test]
    fn deprecations_page_has_a_row_per_table_entry() {
        let page = page();
        for row in DEPRECATED {
            assert!(
                page.body
                    .contains(&format!("| `{}` | `{}` |", row.old, row.new)),
                "generated deprecations reference has no row for {}",
                row.old
            );
        }
        let rows = page
            .body
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count();
        assert_eq!(
            rows,
            DEPRECATED.len(),
            "row count must match the canonical table exactly"
        );
    }

    /// Every row names both lifecycle releases, so a reader always knows
    /// when a spelling appeared on death row and when it goes.
    #[test]
    fn every_row_carries_both_lifecycle_releases() {
        let page = page();
        for line in page.body.lines().filter(|line| line.starts_with("| `")) {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            // Leading/trailing pipes produce empty first/last cells.
            assert_eq!(cells.len(), 6, "four columns per row: {line}");
            assert!(
                cells[3].starts_with('v') && cells[4].starts_with('v'),
                "both release cells must carry a version: {line}"
            );
        }
    }
}
