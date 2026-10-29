//! A failed Actions job's log, cut to the step that failed.

use super::gh::{self, GhError, Result};

/// Most lines an excerpt keeps, counted back from the failure.
pub const EXCERPT_LINES: usize = 240;

/// The log of job `job` on `repo`, cut by [`excerpt`].
pub fn job_excerpt(repo: &str, job: u64) -> Result<String> {
    if super::split_repo(repo).is_none() {
        return Err(GhError::Failed(format!(
            "{repo} is not an owner/name repository."
        )));
    }
    let endpoint = format!("repos/{repo}/actions/jobs/{job}/logs");
    let dir = std::env::temp_dir();
    // Logs carry ANSI colour; without the flag gh refuses to print them.
    let raw = match gh::run(&dir, &["api", "--allow-escape-sequences", &endpoint], None) {
        Err(GhError::Failed(message)) if message.contains("unknown flag") => {
            gh::run(&dir, &["api", &endpoint], None)?
        }
        other => other?,
    };
    Ok(excerpt(&String::from_utf8_lossy(&raw)))
}

/// From the start of the step holding the first error through the last
/// error, timestamps and escapes removed, at most [`EXCERPT_LINES`] lines
/// back from the end. A log without an error keeps its tail.
pub fn excerpt(log: &str) -> String {
    let lines: Vec<String> = log.lines().map(clean).collect();
    let is_error = |line: &String| line.starts_with("##[error]");
    let error = lines.iter().position(is_error);
    let end = lines
        .iter()
        .rposition(is_error)
        .map_or(lines.len(), |last| last + 1);
    let step = error
        .and_then(|error| {
            lines[..error]
                .iter()
                .rposition(|line| line.starts_with("##[group]"))
        })
        .unwrap_or(0);
    let start = step.max(end.saturating_sub(EXCERPT_LINES));
    lines[start..end].join("\n")
}

/// One line without its timestamp or terminal escapes.
fn clean(line: &str) -> String {
    let line = line.trim_end_matches('\r').trim_start_matches('\u{feff}');
    let line = strip_timestamp(line);
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        // CSI: ESC [ params final-byte; anything else drops the next char.
        if chars.next_if_eq(&'[').is_some() {
            for next in chars.by_ref() {
                if ('@'..='~').contains(&next) {
                    break;
                }
            }
        } else {
            chars.next();
        }
    }
    out
}

/// Drop Actions' `2026-10-23T16:28:21.2116223Z ` prefix.
fn strip_timestamp(line: &str) -> &str {
    let Some((stamp, rest)) = line.split_once(' ') else {
        return line;
    };
    let looks = stamp.len() >= 20
        && stamp.ends_with('Z')
        && stamp.as_bytes().get(4) == Some(&b'-')
        && stamp.as_bytes().get(10) == Some(&b'T');
    if looks { rest } else { line }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_excerpt_is_the_failing_step() {
        let log = "\u{feff}2026-10-23T16:25:44.7777930Z ##[group]Run setup\n\
                   2026-10-23T16:25:44.7777930Z fine\n\
                   2026-10-23T16:25:45.1834946Z ##[group]Run just lint\n\
                   2026-10-23T16:25:45.1834946Z \u{1b}[31merror\u{1b}[0m: unused\n\
                   2026-10-23T16:28:21.2116223Z ##[error]Process completed with exit code 1.\n\
                   2026-10-23T16:28:21.3211105Z ##[group]Post cleanup\n\
                   2026-10-23T16:28:21.3211105Z done";
        assert_eq!(
            excerpt(log),
            "##[group]Run just lint\nerror: unused\n##[error]Process completed with exit code 1."
        );
    }

    #[test]
    fn a_log_without_an_error_keeps_its_tail() {
        let log = (0..300).fold(String::new(), |mut log, n| {
            use std::fmt::Write as _;
            let _ = writeln!(log, "line {n}");
            log
        });
        let cut = excerpt(&log);
        assert_eq!(cut.lines().count(), EXCERPT_LINES);
        assert!(cut.ends_with("line 299"));
    }

    #[test]
    fn plain_lines_are_kept() {
        assert_eq!(clean("not a stamp here"), "not a stamp here");
    }
}
