//! The generated deprecations reference, rendered from the canonical
//! table in `crate::deprecations` — the same rows the binary-level audit
//! in `tests/deprecated_aliases.rs` runs one by one, so the page cannot
//! list a spelling the binary no longer warns for, nor omit one it does.

use crate::deprecations::{DEPRECATED, REMOVED};

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
         then.\n\n",
    );

    // Matched as a slice pattern (not `.is_empty()`) so the empty arm stays
    // live: `DEPRECATED` is a `const`, and clippy's `const_is_empty` flags an
    // `is_empty()` call on it as always-true dead logic while the table has
    // no rows.
    match DEPRECATED {
        [] => body.push_str(
            "No spelling is currently deprecated. When one is added to \
             `crate::deprecations::DEPRECATED`, it appears here as a row \
             of this table:\n\n\
             | Deprecated spelling | Use instead | Deprecated in | Planned removal |\n\
             |---|---|---|---|\n",
        ),
        [first, ..] => {
            body.push_str(
                "| Deprecated spelling | Use instead | Deprecated in | Planned removal |\n\
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
                first.note
            );
        }
    }

    if let [_, ..] = REMOVED {
        body.push_str(
            "\n## Removed\n\nThese spellings no longer parse. They are listed \
             because a parse error names the replacement for them: clap's \
             nearest-match is computed on string distance and is usually wrong \
             here (`phux remote add` resolves to `rename`), so each row below \
             adds a `hint:` line naming the real migration. Rows age out once \
             nobody is still upgrading past the release that removed them, \
             after which clap's ordinary message is the honest answer.\n\n\
             | Removed spelling | Use instead | Removed in |\n\
             |---|---|---|\n",
        );
        for row in REMOVED {
            let _ = writeln!(
                body,
                "| `{}` | `{}` | {} |",
                row.old, row.new, row.removed_in
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
    use super::{DEPRECATED, REMOVED, page};

    /// The page carries two tables now. Split the body so each assertion
    /// reads only its own section: a row-count check that swept the whole
    /// page would silently start counting the other table's rows.
    fn sections() -> (String, String) {
        let body = page().body;
        body.split_once("\n## Removed\n").map_or_else(
            || (body.clone(), String::new()),
            |(deprecated, removed)| (deprecated.to_owned(), removed.to_owned()),
        )
    }

    fn row_lines(section: &str) -> Vec<&str> {
        section
            .lines()
            .filter(|line| line.starts_with("| `"))
            .collect()
    }

    /// One table row per deprecation, and no extras — the one-table
    /// contract of phux-i0e8.13.4.
    #[test]
    fn deprecations_page_has_a_row_per_table_entry() {
        let (deprecated, _) = sections();
        for row in DEPRECATED {
            assert!(
                deprecated.contains(&format!("| `{}` | `{}` |", row.old, row.new)),
                "generated deprecations reference has no row for {}",
                row.old
            );
        }
        assert_eq!(
            row_lines(&deprecated).len(),
            DEPRECATED.len(),
            "row count must match the canonical table exactly"
        );
    }

    /// Every row names both lifecycle releases, so a reader always knows
    /// when a spelling appeared on death row and when it goes.
    #[test]
    fn every_row_carries_both_lifecycle_releases() {
        let (deprecated, _) = sections();
        for line in row_lines(&deprecated) {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            // Leading/trailing pipes produce empty first/last cells.
            assert_eq!(cells.len(), 6, "four columns per row: {line}");
            assert!(
                cells[3].starts_with('v') && cells[4].starts_with('v'),
                "both release cells must carry a version: {line}"
            );
        }
    }

    /// The Removed section mirrors `REMOVED` exactly, so the page cannot
    /// promise a migration hint the binary does not emit, nor omit one it
    /// does.
    #[test]
    fn removed_section_has_a_row_per_removal() {
        let (_, removed) = sections();
        for row in REMOVED {
            assert!(
                removed.contains(&format!("| `{}` | `{}` |", row.old, row.new)),
                "generated deprecations reference has no removal row for {}",
                row.old
            );
        }
        assert_eq!(row_lines(&removed).len(), REMOVED.len());
        for line in row_lines(&removed) {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            assert_eq!(cells.len(), 5, "three columns per removal row: {line}");
            assert!(
                cells[3].starts_with('v'),
                "the removal release must carry a version: {line}"
            );
        }
    }
}
