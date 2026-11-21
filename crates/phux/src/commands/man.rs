//! `phux man` — the phux(1) manual page, as roff on stdout.
//!
//! One page: the root help as DESCRIPTION, then every visible command's
//! help under COMMANDS, from the same text as `docs/reference/cli.md`
//! ([`crate::refdocs::cli::help_sections`]), so `man phux` matches the
//! binary that wrote it. Packagers run it at install time into
//! `man1/phux.1`. Contacts no server and reads no config.

use std::fmt::Write as _;
use std::process::ExitCode;

/// Print the manual page.
pub(crate) fn run() -> ExitCode {
    crate::output::bytes(page().as_bytes());
    ExitCode::SUCCESS
}

/// The whole page. Help text goes in no-fill blocks, so its columns and
/// examples keep their layout.
fn page() -> String {
    let mut out = format!(
        ".TH PHUX 1 \"\" \"{}\" \"User Commands\"\n.SH NAME\nphux \\- {}\n",
        escape(crate::BANNER),
        escape(env!("CARGO_PKG_DESCRIPTION")),
    );
    for (i, (path, help)) in crate::refdocs::cli::help_sections().iter().enumerate() {
        match i {
            0 => out.push_str(".SH DESCRIPTION\n"),
            1 => out.push_str(".SH COMMANDS\n"),
            _ => {}
        }
        if i > 0 {
            let _ = writeln!(out, ".SS {}", escape(path));
        }
        out.push_str(".nf\n");
        for line in help.lines() {
            let _ = writeln!(out, "{}", escape(line));
        }
        out.push_str(".fi\n");
    }
    out.push_str(".SH SEE ALSO\nhttps://phux.sh/docs\n");
    out
}

/// Make `text` literal roff: backslashes and hyphens escaped (a bare `-`
/// may render as a typographic hyphen that breaks a pasted flag), and a
/// leading `.` or `'` kept from reading as a request.
fn escape(text: &str) -> String {
    let body = text.replace('\\', "\\e").replace('-', "\\-");
    if body.starts_with(['.', '\'']) {
        format!("\\&{body}")
    } else {
        body
    }
}

#[cfg(test)]
mod tests {
    use super::{escape, page};

    #[test]
    fn escapes_what_roff_would_read() {
        assert_eq!(escape("--json"), "\\-\\-json");
        assert_eq!(escape(r"a\b"), r"a\eb");
        assert_eq!(escape(".hidden"), "\\&.hidden");
        assert_eq!(escape("'quoted"), "\\&'quoted");
    }

    /// Only the page's own requests start a line with `.`, and every
    /// command has its section.
    #[test]
    fn page_is_well_formed_roff_with_every_command() {
        let page = page();
        assert!(page.starts_with(".TH PHUX 1 "));
        for line in page.lines().filter(|line| line.starts_with(['.', '\''])) {
            let request = line.split(' ').next().unwrap_or_default();
            assert!(
                [".TH", ".SH", ".SS", ".nf", ".fi"].contains(&request),
                "help text leaked a roff request: {line:?}"
            );
        }
        assert!(page.contains("\n.SS phux agent set\n"));
        assert!(page.contains("\n.SS phux man\n"));
    }
}
