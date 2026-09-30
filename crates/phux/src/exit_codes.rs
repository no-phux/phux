//! The canonical exit-code table, rendered by `phux help exit-codes` and
//! `docs/reference/exit-codes.md`. Verbs exit 0, 1, or 2; selector paths add 3
//! (partial view); `wait` adds 124; `run` and plugin actions add 125 (`run`
//! otherwise mirrors the child's code). Call sites use these consts so the
//! table and the behavior cannot drift.

/// Success.
pub(crate) const EXIT_SUCCESS: u8 = 0;

/// Generic failure (`ExitCode::FAILURE`): no server, no such target, or
/// the verb itself failed.
pub(crate) const EXIT_FAILURE: u8 = 1;

/// Usage error (clap's own code for a bad invocation), or the server
/// refused the request.
pub(crate) const EXIT_USAGE: u8 = 2;

/// The selector was resolved against a partial view of the fleet (a
/// federation satellite was unreachable) — distinct from
/// [`EXIT_FAILURE`] so a script can branch: retry is right for 3 and
/// wrong for 1. See `commands::partial`.
pub(crate) const EXIT_PARTIAL_VIEW: u8 = 3;

/// `phux wait` gave up because `--timeout` expired (the conventional
/// `timeout(1)` code).
pub(crate) const EXIT_WAIT_TIMEOUT: u8 = 124;

/// `phux run` (and a plugin action) gave up because `--timeout` expired.
/// 125 rather than 124 because `run` mirrors the exit code of the command
/// it ran, and the child itself may legitimately exit 124.
pub(crate) const EXIT_RUN_TIMEOUT: u8 = 125;

/// One documented exit code: the value plus its meaning, in help-section
/// prose. `lines` is the meaning broken for the fixed-width help layout;
/// the markdown renderer joins the lines back into one cell.
pub(crate) struct ExitCodeSpec {
    /// The process exit code.
    pub(crate) code: u8,
    /// The meaning, pre-broken into help-width lines (first line follows
    /// the code column; the rest are continuation lines).
    pub(crate) lines: &'static [&'static str],
}

/// Every exit code the binary uses, ascending. The canonical table.
pub(crate) const EXIT_CODES: &[ExitCodeSpec] = &[
    ExitCodeSpec {
        code: EXIT_SUCCESS,
        lines: &["Success."],
    },
    ExitCodeSpec {
        code: EXIT_FAILURE,
        lines: &["Failure: no server, no such target, or the verb itself failed."],
    },
    ExitCodeSpec {
        code: EXIT_USAGE,
        lines: &["Usage error, or the server refused the request."],
    },
    ExitCodeSpec {
        code: EXIT_PARTIAL_VIEW,
        lines: &[
            "Unanswerable: the selector was resolved against a partial view",
            "of the fleet (a federation satellite was unreachable). Retry",
            "once the link is back — unlike 1, the target may exist.",
        ],
    },
    ExitCodeSpec {
        code: EXIT_WAIT_TIMEOUT,
        lines: &["`phux wait` gave up because `--timeout` expired."],
    },
    ExitCodeSpec {
        code: EXIT_RUN_TIMEOUT,
        lines: &[
            "`phux run` gave up because `--timeout` expired; otherwise",
            "`run` mirrors the exit code of the command it ran, so",
            "`phux run … && next` composes like a shell.",
        ],
    },
];

/// Render the EXIT STATUS help section from [`EXIT_CODES`] — the exact
/// block `phux help exit-codes` prints.
pub(crate) fn exit_status_section() -> String {
    use std::fmt::Write as _;

    let mut section = String::from("EXIT STATUS\n");
    for spec in EXIT_CODES {
        let mut lines = spec.lines.iter();
        // The table guarantees at least one line per code; an empty
        // meaning would render a bare number, so treat it as absent.
        let first = lines.next().copied().unwrap_or_default();
        let _ = writeln!(section, "  {:<6}{first}", spec.code);
        for line in lines {
            let _ = writeln!(section, "  {:<6}{line}", "");
        }
    }
    // Drop the final newline: the caller joins sections with blank lines.
    section.pop();
    section
}

#[cfg(test)]
mod tests {
    use super::{EXIT_CODES, exit_status_section};

    /// The rendered help section opens each code's row with the code in
    /// a fixed-width column, so the `help_inventory` gate (and a human
    /// scanning `--help`) finds every code at the line start.
    #[test]
    fn exit_status_section_renders_one_row_per_code() {
        let section = exit_status_section();
        assert!(section.starts_with("EXIT STATUS\n"));
        for spec in EXIT_CODES {
            assert!(
                section
                    .lines()
                    .any(|line| line.strip_prefix("  ").is_some_and(|rest| {
                        rest.split_whitespace().next() == Some(&spec.code.to_string())
                    })),
                "EXIT STATUS section has no row for {}:\n{section}",
                spec.code
            );
        }
        assert!(
            !section.ends_with('\n'),
            "section must not carry a trailing newline (the caller joins)"
        );
    }

    /// The exact block `phux help exit-codes` prints, pinned so a table
    /// edit that shifts the layout is a visible diff here.
    #[test]
    fn exit_status_section_matches_the_pinned_layout() {
        const EXPECTED: &str = "\
EXIT STATUS
  0     Success.
  1     Failure: no server, no such target, or the verb itself failed.
  2     Usage error, or the server refused the request.
  3     Unanswerable: the selector was resolved against a partial view
        of the fleet (a federation satellite was unreachable). Retry
        once the link is back — unlike 1, the target may exist.
  124   `phux wait` gave up because `--timeout` expired.
  125   `phux run` gave up because `--timeout` expired; otherwise
        `run` mirrors the exit code of the command it ran, so
        `phux run … && next` composes like a shell.";
        assert_eq!(exit_status_section(), EXPECTED);
    }
}
