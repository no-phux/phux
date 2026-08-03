//! The canonical exit-code table for the `phux` binary (phux-i0e8.11.4).
//!
//! This module is the single source both exit-status surfaces render
//! from: the root `--help` EXIT STATUS section (`root_after_long_help` in
//! `main.rs` builds it via [`exit_status_section`]) and the generated
//! `docs/reference/exit-codes.md` page (`refdocs::exit_codes`). Adding or
//! changing a code here updates both; using a code the table does not
//! carry is what the audit below exists to prevent.
//!
//! Audit of the `ExitCode` sites (2026-08-02): every verb exits 0
//! ([`EXIT_SUCCESS`]), 1 ([`EXIT_FAILURE`] — `ExitCode::FAILURE`
//! everywhere), or 2 ([`EXIT_USAGE`]); the selector paths add 3
//! ([`EXIT_PARTIAL_VIEW`], `commands::partial`); `phux wait` adds 124
//! ([`EXIT_WAIT_TIMEOUT`], `commands::wait`); `phux run` and the plugin
//! action runner add 125 ([`EXIT_RUN_TIMEOUT`], `commands::run` and
//! `commands::config`) — `run` otherwise mirrors the child's own code,
//! which is why its timeout is 125 and not 124. Those non-clap sites
//! consume the consts here, so the table and the behavior cannot drift.

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
/// block `phux --help` shows (via `root_after_long_help` in `main.rs`).
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
    use super::{
        EXIT_CODES, EXIT_FAILURE, EXIT_PARTIAL_VIEW, EXIT_RUN_TIMEOUT, EXIT_SUCCESS, EXIT_USAGE,
        EXIT_WAIT_TIMEOUT, exit_status_section,
    };

    /// The table carries exactly the audited code set, ascending, each
    /// with non-empty prose — the invariant both renderers rely on.
    #[test]
    fn table_is_the_audited_code_set_in_order() {
        let codes: Vec<u8> = EXIT_CODES.iter().map(|spec| spec.code).collect();
        assert_eq!(
            codes,
            vec![
                EXIT_SUCCESS,
                EXIT_FAILURE,
                EXIT_USAGE,
                EXIT_PARTIAL_VIEW,
                EXIT_WAIT_TIMEOUT,
                EXIT_RUN_TIMEOUT,
            ],
        );
        assert_eq!(codes, vec![0, 1, 2, 3, 124, 125]);
        for spec in EXIT_CODES {
            assert!(
                spec.lines.iter().all(|line| !line.is_empty()),
                "empty prose line for exit code {}",
                spec.code
            );
            assert!(
                !spec.lines.is_empty(),
                "no prose for exit code {}",
                spec.code
            );
        }
    }

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
}
