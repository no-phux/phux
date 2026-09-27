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
//! scanning keeps stale markers out. Assumes a POSIX shell.
//!
//! Because this is a typed command line, `run` first checks that a shell is
//! in the foreground ([`crate::agent_meta::pane_shell_availability`]) and
//! refuses fail-closed otherwise; `force` opts out.

use std::path::Path;
use std::time::Duration;

use phux_core::screen::{ROW_WINDOW_ALL, ScreenState};
use phux_protocol::ResourceId;
use serde::Serialize;
use tokio::time::Instant;

use crate::agent_meta::{ShellAvailability, pane_shell_availability};
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

/// Why [`run`] returned.
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

/// The printed `BEGIN` marker for `nonce` (own row, short, never wraps).
fn begin_marker(nonce: &str) -> String {
    format!("PHUXrun{nonce}BEGIN")
}

/// The stable prefix of the printed `RC` marker. Output form:
/// `<prefix><code>=END`.
fn rc_prefix(nonce: &str) -> String {
    format!("PHUXrun{nonce}RC=")
}

/// Build the shell line to submit: a `BEGIN` sentinel, the user command,
/// then an `RC` sentinel carrying the exit code — each `printf` on its own
/// fresh row.
fn command_line(cmd: &str, nonce: &str) -> String {
    format!(
        "printf '{}\\n'; {cmd}; printf '\\n{}%d=END\\n' $?",
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
    if let Some(refusal) = check_shell(socket, &pane, cmd, deadline, force).await {
        drop(conn);
        return Ok(refusal);
    }
    let keys = [command_line(cmd, nonce), "Enter".to_owned()];
    let submission = submit(&mut conn, &pane, &keys, deadline).await?;
    drop(conn);
    if submission != Submission::Complete {
        return Ok(timed_out(cmd, deadline, submission, ScreenState::default()));
    }
    poll_for_rc(socket, pane, cmd, nonce, deadline).await
}

/// The available-shell precondition, as an outcome to return instead of
/// typing (`None` means go ahead). Fails closed: an unevaluable check or an
/// expired budget never lets the line through.
async fn check_shell(
    socket: &Path,
    pane: &ResourceId,
    cmd: &str,
    deadline: Deadline,
    force: bool,
) -> Option<RunOutcome> {
    if force {
        return None;
    }
    let Some(availability) = deadline.run(pane_shell_availability(socket, pane)).await else {
        return Some(not_sent(cmd, deadline));
    };
    match availability {
        ShellAvailability::Available => None,
        availability => Some(RunOutcome::Refused {
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
        if let Some((idx, code)) = parse_rc(&screen.lines, nonce) {
            // Stamped before the capture read, which is not the child's time.
            let duration_ms = duration_ms(start);
            let (output, truncated) = capture_span(socket, &pane, nonce, deadline)
                .await
                .unwrap_or_else(|| extract_output(&screen.lines, idx, nonce));
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
    let rows = capture_rows(&read);
    let (rc_idx, _) = parse_rc(&rows, nonce)?;
    Some(extract_output(&rows, rc_idx, nonce))
}

/// A capture read's rows as one stream: retained history, then viewport.
fn capture_rows(screen: &ScreenState) -> Vec<String> {
    let mut rows = screen.scrollback.clone();
    rows.extend_from_slice(&screen.lines);
    rows
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
        duration_ms: duration_ms(deadline.started_at()),
        submission,
        screen: Box::new(screen),
    }
}

/// Milliseconds elapsed since `start`, saturating into `u64`.
fn duration_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_brackets_with_begin_and_rc_sentinels() {
        assert_eq!(
            command_line("ls -la", "42"),
            "printf 'PHUXrun42BEGIN\\n'; ls -la; printf '\\nPHUXrun42RC=%d=END\\n' $?"
        );
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
mod deadline_tests {
    use std::path::Path;
    use std::time::Duration;

    use phux_protocol::ResourceId;
    use tokio::net::UnixListener;
    use tokio::time::Instant;

    use phux_core::screen::ScreenState;

    use super::{RunOutcome, SUBMIT_GRACE, Submission, run_in_with_deadline};
    use crate::deadline::Deadline;
    use crate::testkit::{self, ScriptSpec};

    const BUDGET: Duration = Duration::from_millis(300);
    /// Slack for a loaded machine; a regression overruns by far more.
    const TOLERANCE: Duration = Duration::from_secs(3);
    /// A regression fails here instead of hanging the test run.
    const WEDGE: Duration = Duration::from_secs(10);

    /// `force`: a stalled peer cannot answer the precondition's reads, and
    /// these cases exercise the submit/poll budget.
    async fn run_with_budget(socket: &Path) -> (RunOutcome, Duration) {
        let start = Instant::now();
        let deadline = Deadline::new(Some(BUDGET));
        let run = run_in_with_deadline(socket, ResourceId::local(1), "true", "n1", deadline, true);
        let outcome = tokio::time::timeout(WEDGE, run)
            .await
            .expect("the run must return; a timeout here is the wedge itself")
            .expect("a stalled peer is a timeout, not an error");
        (outcome, start.elapsed())
    }

    /// Assert a timeout no sooner than `min` that reports `expected`, and
    /// hand back the screen it carried.
    fn assert_timed_out(
        outcome: RunOutcome,
        elapsed: Duration,
        min: Duration,
        expected: Submission,
    ) -> ScreenState {
        let RunOutcome::TimedOut {
            submission, screen, ..
        } = outcome
        else {
            panic!("expected a timeout, got {outcome:?}");
        };
        assert_eq!(submission, expected);
        assert!(elapsed >= min, "gave up early after {elapsed:?}");
        assert!(elapsed < min + TOLERANCE, "overran: {elapsed:?}");
        *screen
    }

    #[tokio::test]
    async fn a_peer_that_never_answers_hello_ends_the_run_on_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("silent.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let peer = tokio::spawn(testkit::hold_silent(listener));

        let (outcome, elapsed) = run_with_budget(&socket).await;
        assert_timed_out(outcome, elapsed, BUDGET, Submission::NotSent);
        peer.abort();
    }

    #[tokio::test]
    async fn a_peer_that_never_answers_get_screen_ends_the_run_on_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("wedged.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let peer = tokio::spawn(testkit::serve_every(listener, || {
            ScriptSpec::new().wedge_screen_reads()
        }));

        let (outcome, elapsed) = run_with_budget(&socket).await;
        let screen = assert_timed_out(outcome, elapsed, BUDGET, Submission::Complete);
        assert!(
            screen.lines.is_empty(),
            "no read completed, so the reported screen is the empty default"
        );
        peer.abort();
    }

    #[tokio::test]
    async fn a_peer_that_acks_the_line_but_never_the_enter_reports_partial_input() {
        // The line is acked but its Enter never is: partial, after the grace.
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("half.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let peer = tokio::spawn(testkit::serve_every(listener, || {
            ScriptSpec::new().wedge_input_after(1)
        }));

        let (outcome, elapsed) = run_with_budget(&socket).await;
        assert_timed_out(outcome, elapsed, BUDGET + SUBMIT_GRACE, Submission::Partial);
        peer.abort();
    }
}

/// The available-shell precondition and the scrollback-aware capture, end
/// to end against the scripted server.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic, reason = "tests")]
mod precondition_tests {
    use std::future::Future;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use phux_core::screen::{CellInfo, CellStyle, CursorState, ScreenState, SemanticContent};
    use phux_protocol::ResourceId;
    use phux_protocol::wire::frame::Scope;
    use tokio::net::UnixListener;

    use super::{RunOutcome, run_in_with_deadline};
    use crate::agent_meta::{RESOURCE_PANE_OCCUPANT_KEY, ShellAvailability};
    use crate::deadline::Deadline;
    use crate::testkit::{self, ScriptSpec};

    const PANE: u32 = 1;
    const NONCE: &str = "n1";
    /// Generous: the scripted server answers every read immediately, so a
    /// regression fails on an assertion rather than on this bound.
    const BUDGET: Duration = Duration::from_secs(10);

    fn pane() -> ResourceId {
        ResourceId::local(PANE)
    }

    /// A pane whose cursor sits on an OSC-133 `Prompt` row — the shape the
    /// precondition accepts — carrying `rows` as its viewport.
    fn at_prompt(rows: &[&str]) -> ScreenState {
        let cursor_row = u16::try_from(rows.len().saturating_sub(1)).unwrap_or(0);
        ScreenState {
            pane: PANE,
            cols: 80,
            rows: 24,
            cursor: Some(CursorState {
                x: 0,
                y: cursor_row,
                visible: true,
            }),
            lines: rows.iter().map(|row| (*row).to_owned()).collect(),
            cells: Some(vec![CellInfo {
                col: 0,
                row: cursor_row,
                semantic: Some(SemanticContent::Prompt),
                style: CellStyle::default(),
            }]),
            ..ScreenState::default()
        }
    }

    fn occupant(foreground: &str, is_pane_shell: bool) -> Vec<u8> {
        format!(r#"{{"foreground":"{foreground}","is_pane_shell":{is_pane_shell}}}"#).into_bytes()
    }

    async fn run_against(socket: &Path, force: bool) -> RunOutcome {
        run_in_with_deadline(
            socket,
            pane(),
            "cargo build",
            NONCE,
            Deadline::new(Some(BUDGET)),
            force,
        )
        .await
        .expect("a scripted server answers every read")
    }

    /// Serve a fresh `make_spec` on each of the run's connections for the
    /// whole of `body`.
    async fn with_server<F, Fut, T>(
        make_spec: impl Fn() -> ScriptSpec + Send + 'static,
        body: F,
    ) -> T
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: Future<Output = T>,
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("run.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let peer = tokio::spawn(testkit::serve_every(listener, make_spec));
        let out = body(socket).await;
        peer.abort();
        out
    }

    /// A pane running `vim` is refused, naming the foreground, rather than
    /// typed a shell line as normal-mode commands.
    #[tokio::test]
    async fn a_pane_running_vim_is_refused_before_anything_is_typed() {
        let outcome = with_server(
            || {
                ScriptSpec::new()
                    .stored_metadata(
                        Scope::Resource(pane()),
                        RESOURCE_PANE_OCCUPANT_KEY,
                        occupant("vim", false),
                    )
                    .screen(&at_prompt(&["~", "~"]))
            },
            |socket| async move { run_against(&socket, false).await },
        )
        .await;

        let RunOutcome::Refused {
            command,
            availability,
        } = outcome
        else {
            panic!("expected a refusal, got {outcome:?}");
        };
        assert_eq!(command, "cargo build");
        assert_eq!(
            availability,
            ShellAvailability::BusyProcess("vim".to_owned())
        );
    }

    /// No occupant record and no OSC-133 marks: fail closed.
    #[tokio::test]
    async fn an_unanswerable_pane_is_refused_rather_than_typed_into() {
        let outcome = with_server(
            || ScriptSpec::new().screen(&ScreenState::default()),
            |socket| async move { run_against(&socket, false).await },
        )
        .await;
        assert!(
            matches!(
                outcome,
                RunOutcome::Refused {
                    availability: ShellAvailability::Unanswerable,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }

    /// `force` types the command line without consulting the precondition.
    #[tokio::test]
    async fn force_types_the_command_line_into_a_busy_pane() {
        let outcome = with_server(
            || {
                ScriptSpec::new()
                    .stored_metadata(
                        Scope::Resource(pane()),
                        RESOURCE_PANE_OCCUPANT_KEY,
                        occupant("vim", false),
                    )
                    .screen(&at_prompt(&[
                        &format!("PHUXrun{NONCE}BEGIN"),
                        "compiling",
                        &format!("PHUXrun{NONCE}RC=0=END"),
                    ]))
            },
            |socket| async move { run_against(&socket, true).await },
        )
        .await;

        let RunOutcome::Completed(result) = outcome else {
            panic!("expected a completed run, got {outcome:?}");
        };
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.output, "compiling");
        assert!(!result.truncated);
    }

    /// With `BEGIN` scrolled out of the viewport, the scrollback-aware
    /// capture still returns the whole span, untruncated.
    #[tokio::test]
    async fn output_is_captured_across_the_scroll_boundary() {
        let mut scrolled = at_prompt(&["row 98", "row 99", &format!("PHUXrun{NONCE}RC=0=END")]);
        scrolled.scrollback = std::iter::once(format!("PHUXrun{NONCE}BEGIN"))
            .chain((0..98).map(|n| format!("row {n}")))
            .collect();

        let outcome = with_server(
            move || {
                ScriptSpec::new()
                    .stored_metadata(
                        Scope::Resource(pane()),
                        RESOURCE_PANE_OCCUPANT_KEY,
                        occupant("zsh", true),
                    )
                    .screen(&scrolled)
            },
            |socket| async move { run_against(&socket, false).await },
        )
        .await;

        let RunOutcome::Completed(result) = outcome else {
            panic!("expected a completed run, got {outcome:?}");
        };
        let lines: Vec<&str> = result.output.lines().collect();
        assert_eq!(lines.len(), 100, "the whole BEGIN..RC span, not the tail");
        assert_eq!(lines.first(), Some(&"row 0"));
        assert_eq!(lines.last(), Some(&"row 99"));
        assert!(
            !result.truncated,
            "the span was captured in full, so nothing was truncated"
        );
    }
}
