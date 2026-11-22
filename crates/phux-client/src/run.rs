//! `phux run`: run a command in a pane and capture its exit code, output,
//! and duration (ADR-0022 §3).
//!
//! libghostty does not retain the `OSC 133;D` exit status, so the command is
//! bracketed with two printed sentinels and `$?` is parsed off the screen:
//!
//! ```text
//! printf '<BEGIN>\n'; <cmd>; printf '\n<RC>=%d=END\n' $?
//! ```
//!
//! Each sentinel prints on its own row; the typed echo carries a literal
//! `%d`, so only the printed marker parses. A per-run nonce plus last-match
//! scanning keeps stale markers out. The line is POSIX except in fish, which
//! rejects `$?` at parse time and gets `$status` instead.
//!
//! Markers are matched on [`ScreenState::unwrapped_rows`], never raw `lines`:
//! a sentinel longer than a narrow pane is soft-wrapped across rows and would
//! otherwise never match (ADR-0077 §2).
//!
//! Because this is a typed command line, `run` first checks that a shell is
//! in the foreground ([`crate::agent_meta::pane_shell_availability`]) and
//! refuses fail-closed otherwise; `force` opts out.

use std::path::Path;
use std::time::Duration;

use phux_core::screen::{ROW_WINDOW_ALL, ScreenState};
use phux_protocol::ResourceId;
use serde::Serialize;

use crate::agent_meta::{ShellAvailability, pane_shell_reading};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::deadline::Deadline;
use crate::send_keys;
use crate::snapshot::{get_screen, get_screen_scrollback};
use crate::wait::DEFAULT_POLL_INTERVAL;

/// A completed command's result — the agent-facing contract for `run`
/// (ADR-0022 §3). `exit_code == n` for a child that did `_exit(n)`.
#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    /// The command line as submitted (without the sentinels).
    pub command: String,
    /// The child's exit code, parsed from the sentinel.
    pub exit_code: i32,
    /// Captured stdout/stderr as it appeared on screen, between the
    /// `BEGIN` and `RC` markers. See `truncated`.
    pub output: String,
    /// Milliseconds from the start of the time budget to sentinel-seen: an
    /// upper bound on the child's runtime.
    pub duration_ms: u64,
    /// `true` when the `BEGIN` marker was not in the captured span (even
    /// with retained scrollback), so `output` is best-effort trailing context.
    pub truncated: bool,
}

/// Why [`run_in_with_deadline`] returned.
#[derive(Debug, Clone)]
pub enum RunOutcome {
    /// The sentinel was seen; the command finished.
    Completed(RunResult),
    /// The available-shell precondition failed, so nothing was typed.
    Refused {
        /// The command line that was not submitted.
        command: String,
        /// What the precondition established. Never
        /// [`ShellAvailability::Available`].
        availability: ShellAvailability,
    },
    /// The timeout elapsed before the sentinel appeared. Carries the last
    /// screen so the caller can show what the command was doing.
    TimedOut {
        /// The command line as submitted.
        command: String,
        /// Wall-clock from the start of the budget to giving up, in
        /// milliseconds.
        duration_ms: u64,
        /// How much of the command line reached the pane.
        submission: Submission,
        /// The last completed screen read, or an empty default screen.
        screen: Box<ScreenState>,
    },
}

/// How much of a run's command line reached the pane before it gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submission {
    /// Nothing was sent: the budget ran out while connecting, resolving, or
    /// before the first input event.
    NotSent,
    /// Sending began but did not finish, even with [`SUBMIT_GRACE`]: the pane
    /// may hold part of the command line, unsubmitted.
    Partial,
    /// The whole command line and its Enter were acknowledged.
    Complete,
}

/// Extra time a submission that has already started gets past the deadline.
///
/// Input is never started after the deadline, but once started the line and
/// its Enter get this long, so a typed-but-unsubmitted line is avoided.
pub const SUBMIT_GRACE: Duration = Duration::from_secs(2);

/// The history window of the one read after the `RC` sentinel (every
/// retained row); the poll itself stays the cheap viewport read.
const CAPTURE_HISTORY: Option<u32> = Some(ROW_WINDOW_ALL);

/// The printed `BEGIN` marker for `nonce` (own logical row; it soft-wraps in a
/// pane narrower than the marker, so match it on unwrapped rows).
fn begin_marker(nonce: &str) -> String {
    format!("PHUXrun{nonce}BEGIN")
}

/// The stable prefix of the printed `RC` marker. Output form:
/// `<prefix><code>=END`.
fn rc_prefix(nonce: &str) -> String {
    format!("PHUXrun{nonce}RC=")
}

/// The exit-status expression for the pane shell named `foreground`: fish
/// refuses to parse `$?`, so a line using it would run nothing.
fn status_var(foreground: Option<&str>) -> &'static str {
    if foreground == Some("fish") {
        "$status"
    } else {
        "$?"
    }
}

/// Build the shell line to submit: a `BEGIN` sentinel, the user command,
/// then an `RC` sentinel carrying the exit code (`status`, from
/// [`status_var`]) — each `printf` on its own fresh row.
fn command_line(cmd: &str, nonce: &str, status: &str) -> String {
    format!(
        "printf '{}\\n'; {cmd}; printf '\\n{}%d=END\\n' {status}",
        begin_marker(nonce),
        rc_prefix(nonce),
    )
}

/// Scan `lines` for the *last* `RC` sentinel and parse its exit code,
/// returning `(row_index, code)`; the echo row's literal `%d` never parses.
fn parse_rc(lines: &[String], nonce: &str) -> Option<(usize, i32)> {
    let prefix = rc_prefix(nonce);
    for (i, line) in lines.iter().enumerate().rev() {
        let Some(after) = line.split(prefix.as_str()).nth(1) else {
            continue;
        };
        if let Some(code_str) = after.split("=END").next()
            && let Ok(code) = code_str.parse::<i32>()
        {
            return Some((i, code));
        }
    }
    None
}

/// The rows strictly between the printed `BEGIN` marker and the `RC` row, or
/// best-effort trailing context with `truncated` when `BEGIN` is absent.
fn extract_output(lines: &[String], rc_idx: usize, nonce: &str) -> (String, bool) {
    let begin = begin_marker(nonce);
    // The printed BEGIN row is the *last* BEGIN above the RC row (the typed
    // echo of the `printf '...BEGIN\n'` sits higher and may have wrapped).
    let begin_idx = lines[..rc_idx]
        .iter()
        .rposition(|l| l.contains(begin.as_str()));
    begin_idx.map_or_else(
        || (lines[..rc_idx].join("\n").trim_end().to_owned(), true),
        |b| (lines[b + 1..rc_idx].join("\n").trim_end().to_owned(), false),
    )
}

/// Run `cmd` in `pane` via `ROUTE_INPUT` (no attach, no resize), then poll
/// the screen until the `RC` sentinel appears or `deadline` expires.
///
/// The deadline bounds every connect and read; input is never started after
/// it, and once started gets [`SUBMIT_GRACE`]. Expiry neither kills the
/// command nor retracts delivered input. `force` skips the available-shell
/// precondition.
///
/// # Errors
///
/// [`AttachError`] from the input send or the screen reads.
pub async fn run_in_with_deadline(
    socket: &Path,
    pane: ResourceId,
    cmd: &str,
    nonce: &str,
    deadline: Deadline,
    force: bool,
) -> Result<RunOutcome, AttachError> {
    let Some(conn) = deadline.run(Connection::connect(socket)).await else {
        return Ok(not_sent(cmd, deadline));
    };
    submit_and_poll(conn?, socket, pane, cmd, nonce, deadline, force).await
}

/// Submit the sentinel-bracketed command over `conn`, then poll for its
/// `RC` sentinel.
async fn submit_and_poll(
    mut conn: Connection,
    socket: &Path,
    pane: ResourceId,
    cmd: &str,
    nonce: &str,
    deadline: Deadline,
    force: bool,
) -> Result<RunOutcome, AttachError> {
    let status = match check_shell(socket, &pane, cmd, deadline, force).await {
        Ok(status) => status,
        Err(refusal) => {
            drop(conn);
            return Ok(refusal);
        }
    };
    let keys = [command_line(cmd, nonce, status), "Enter".to_owned()];
    let submission = submit(&mut conn, &pane, &keys, deadline).await?;
    drop(conn);
    if submission != Submission::Complete {
        return Ok(timed_out(cmd, deadline, submission, ScreenState::default()));
    }
    poll_for_rc(socket, pane, cmd, nonce, deadline).await
}

/// The available-shell precondition: the shell's exit-status expression to
/// type, or an outcome to return instead of typing. Fails closed: an
/// unevaluable check or an expired budget never lets the line through.
/// `force` reads nothing and assumes a POSIX shell.
async fn check_shell(
    socket: &Path,
    pane: &ResourceId,
    cmd: &str,
    deadline: Deadline,
    force: bool,
) -> Result<&'static str, RunOutcome> {
    if force {
        return Ok(status_var(None));
    }
    let Some((occupant, availability)) = deadline.run(pane_shell_reading(socket, pane)).await
    else {
        return Err(not_sent(cmd, deadline));
    };
    match availability {
        ShellAvailability::Available => {
            Ok(status_var(occupant.as_ref().map(|o| o.foreground.as_str())))
        }
        availability => Err(RunOutcome::Refused {
            command: cmd.to_owned(),
            availability,
        }),
    }
}

/// Deliver `keys` unless the budget has already run out; once delivery has
/// started, let it finish within [`SUBMIT_GRACE`] past the deadline.
async fn submit(
    conn: &mut Connection,
    pane: &ResourceId,
    keys: &[String],
    deadline: Deadline,
) -> Result<Submission, AttachError> {
    if deadline.expired() {
        return Ok(Submission::NotSent);
    }
    let delivery = send_keys::route_keys(conn, pane, keys);
    // `None` is the grace running out mid-delivery: part of the line may be
    // at the prompt, unsubmitted.
    deadline
        .extended(SUBMIT_GRACE)
        .run(delivery)
        .await
        .map_or(Ok(Submission::Partial), |delivered| {
            delivered.map(|()| Submission::Complete)
        })
}

/// Poll `pane`'s screen until the `RC` sentinel for `nonce` appears or
/// the absolute deadline elapses.
async fn poll_for_rc(
    socket: &Path,
    pane: ResourceId,
    cmd: &str,
    nonce: &str,
    deadline: Deadline,
) -> Result<RunOutcome, AttachError> {
    let start = deadline.started_at();
    let mut screen = ScreenState::default();
    loop {
        let Some(read) = deadline.run(get_screen(socket, pane.clone())).await else {
            break;
        };
        screen = read?;
        let rows = screen.unwrapped_rows();
        if let Some((idx, code)) = parse_rc(&rows, nonce) {
            // Stamped before the capture read, which is not the child's time.
            let duration_ms = phux_perf::duration_ms(start.elapsed());
            let (output, truncated) = capture_span(socket, &pane, nonce, deadline)
                .await
                .unwrap_or_else(|| extract_output(&rows, idx, nonce));
            return Ok(RunOutcome::Completed(RunResult {
                command: cmd.to_owned(),
                exit_code: code,
                output,
                duration_ms,
                truncated,
            }));
        }
        if deadline
            .run(tokio::time::sleep(DEFAULT_POLL_INTERVAL))
            .await
            .is_none()
        {
            break;
        }
    }
    Ok(timed_out(cmd, deadline, Submission::Complete, screen))
}

/// Re-read `pane` with its scrollback and extract the whole `BEGIN..RC`
/// span, so output longer than the viewport is not truncated.
///
/// `None` (the read failed, expired, or lacks this run's sentinel) falls back
/// to the viewport extraction, keeping the exit code.
async fn capture_span(
    socket: &Path,
    pane: &ResourceId,
    nonce: &str,
    deadline: Deadline,
) -> Option<(String, bool)> {
    let read = deadline
        .run(get_screen_scrollback(
            socket,
            pane.clone(),
            CAPTURE_HISTORY,
            false,
        ))
        .await?
        .ok()?;
    let rows = read.unwrapped_rows();
    let (rc_idx, _) = parse_rc(&rows, nonce)?;
    Some(extract_output(&rows, rc_idx, nonce))
}

/// The budget ran out before any input was sent.
fn not_sent(cmd: &str, deadline: Deadline) -> RunOutcome {
    timed_out(cmd, deadline, Submission::NotSent, ScreenState::default())
}

fn timed_out(
    cmd: &str,
    deadline: Deadline,
    submission: Submission,
    screen: ScreenState,
) -> RunOutcome {
    RunOutcome::TimedOut {
        command: cmd.to_owned(),
        duration_ms: phux_perf::duration_ms(deadline.started_at().elapsed()),
        submission,
        screen: Box::new(screen),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_brackets_with_begin_and_rc_sentinels() {
        assert_eq!(
            command_line("ls -la", "42", status_var(Some("zsh"))),
            "printf 'PHUXrun42BEGIN\\n'; ls -la; printf '\\nPHUXrun42RC=%d=END\\n' $?"
        );
    }

    /// fish rejects `$?` when it parses the line, so nothing would run.
    #[test]
    fn a_fish_pane_gets_its_own_status_variable() {
        assert_eq!(
            command_line("ls", "42", status_var(Some("fish"))),
            "printf 'PHUXrun42BEGIN\\n'; ls; printf '\\nPHUXrun42RC=%d=END\\n' $status"
        );
        assert_eq!(status_var(None), "$?");
    }

    /// The echo row (literal `%d`) never parses, and the last printed
    /// marker beats a stale one with the same nonce.
    #[test]
    fn parse_rc_reads_only_the_last_printed_marker() {
        let echo = "❯ printf 'PHUXrun7BEGIN\\n'; false; printf '\\nPHUXrun7RC=%d=END\\n' $?";
        let rows = |lines: &[&str]| lines.iter().map(|&l| l.to_owned()).collect::<Vec<_>>();
        for (lines, expected) in [
            (rows(&[echo]), None),
            (
                rows(&[echo, "PHUXrun7BEGIN", "PHUXrun7RC=1=END"]),
                Some((2, 1)),
            ),
            (
                rows(&[
                    "PHUXrun7RC=0=END",
                    "PHUXrun7BEGIN",
                    "new",
                    "PHUXrun7RC=3=END",
                ]),
                Some((3, 3)),
            ),
        ] {
            assert_eq!(parse_rc(&lines, "7"), expected, "{lines:?}");
        }
    }

    #[test]
    fn extract_output_takes_the_rows_between_begin_and_rc() {
        let rows = |lines: &[&str]| lines.iter().map(|&l| l.to_owned()).collect::<Vec<_>>();
        let whole = rows(&["echo row", "PHUXrun7BEGIN", "hi", "PHUXrun7RC=0=END"]);
        assert_eq!(extract_output(&whole, 3, "7"), ("hi".to_owned(), false));
        let scrolled = rows(&["scrolled", "more output", "PHUXrun7RC=0=END"]);
        assert!(
            extract_output(&scrolled, 2, "7").1,
            "BEGIN absent is truncated"
        );
    }
}

/// A stalled peer ends the run at the deadline, not never.
#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod deadline_tests;

/// The available-shell precondition and the scrollback-aware capture, end
/// to end against the scripted server.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests")]
mod precondition_tests;
