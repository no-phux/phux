//! The generated exit-codes reference, rendered from the canonical
//! table in `crate::exit_codes` — the same table the root `--help`
//! EXIT STATUS section renders from, so the page, the help text, and
//! the non-clap exit sites (which consume the table's consts) cannot
//! disagree.

use crate::exit_codes::EXIT_CODES;

use super::Page;

/// Render `docs/reference/exit-codes.md`.
pub(crate) fn page() -> Page {
    use std::fmt::Write as _;

    let mut body = String::from(
        "Every exit code the `phux` binary uses, from the canonical \
         in-code table that also renders the EXIT STATUS section of \
         `phux --help`. The codes are chosen so a script can branch: \
         `3` is distinct from `1` because retry is right for `3` and \
         wrong for `1`; `run`'s timeout is `125` rather than `124` \
         because `run` mirrors the exit code of the command it ran, and \
         the child itself may legitimately exit `124`.\n\n\
         | Code | Meaning |\n\
         |---|---|\n",
    );
    for spec in EXIT_CODES {
        let meaning = spec.lines.join(" ").replace('|', "\\|");
        let _ = writeln!(body, "| `{}` | {meaning} |", spec.code);
    }
    body.push_str(
        "\n`phux run` exits with the child command's own code on \
         success, so `phux run … && next` composes like a shell. A \
         `--help` or `--version` request exits `0`, including when the \
         reader hangs up early (`phux --help | head`).\n",
    );

    Page {
        file: "exit-codes.md",
        title: "phux exit codes reference",
        summary: "Every exit code the binary uses and what each one means.",
        tldr: "The canonical exit-code table: 0 success, 1 failure, 2 \
               usage error or server refusal, 3 partial-fleet \
               unanswerable, 124 `wait` timeout, 125 `run` timeout. \
               Rendered from the same in-code table `phux --help`'s EXIT \
               STATUS section uses, so the two cannot disagree.",
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::{EXIT_CODES, page};

    /// One table row per canonical exit code, and no extras.
    #[test]
    fn exit_codes_page_has_a_row_per_code() {
        let page = page();
        for spec in EXIT_CODES {
            assert!(
                page.body.contains(&format!("| `{}` |", spec.code)),
                "generated exit-codes reference has no row for {}",
                spec.code
            );
        }
        let rows = page
            .body
            .lines()
            .filter(|line| line.starts_with("| `"))
            .count();
        assert_eq!(
            rows,
            EXIT_CODES.len(),
            "row count must match the canonical table exactly"
        );
    }

    /// The page and the `--help` EXIT STATUS section describe the same
    /// code set — the two-renderers-one-table contract of this task.
    #[test]
    fn page_and_help_section_agree_on_the_code_set() {
        let section = crate::exit_codes::exit_status_section();
        for spec in EXIT_CODES {
            assert!(
                section.lines().any(|line| {
                    line.strip_prefix("  ").is_some_and(|rest| {
                        rest.split_whitespace().next() == Some(&spec.code.to_string())
                    })
                }),
                "--help EXIT STATUS section is missing code {}",
                spec.code
            );
        }
    }
}
