//! `phux run` — run a command in a pane and capture its exit code, output,
//! and duration (phux-ab8, ADR-0022 §3).
//!
//! The exit code is the load-bearing value, and it cannot come from a grid
//! walk: libghostty records OSC-133 *semantic marks* per cell but does not
//! retain the `OSC 133;D;<code>` exit status. So `run` uses the portable,
//! shell-integration-free floor — it brackets the command with two printed
//! **sentinels** and parses the real `$?` out of the screen:
//!
//! ```text
//! printf '<BEGIN>\n'; <cmd>; printf '\n<RC>=%d=END\n' $?
//! ```
//!
//! Both sentinels print on their *own fresh rows*, so they never wrap (a
//! long command's echo can wrap arbitrarily — we never depend on matching
//! that echo). Output is exactly the rows between the printed `BEGIN` and
//! `RC` markers. The exit code is parsed from the `RC` marker; the typed
//! echo of that marker carries a literal `%d` (printf's directive), so its
//! parse fails and it is skipped — only the printed digits match.
//!
//! The `nonce` must be unique per invocation (pid alone is not — PIDs are
//! recycled), so a stale marker from an earlier `run` left in the viewport
//! cannot be mistaken for this run's. We additionally scan for the *last*
//! marker, so the freshest emission always wins.
//!
//! `run` assumes a POSIX shell (sh/bash/zsh): it relies on `;`, `$?`, and
//! `printf`. Fish and other non-POSIX shells are out of scope for v0, as is
//! a command that is not a well-formed single statement (an unbalanced
//! quote leaves the shell at a continuation prompt and the sentinels never
//! print — the `--timeout` then bounds the wait).

use std::path::Path;
use std::time::Duration;

use phux_core::screen::ScreenState;
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::AttachTarget;
use serde::Serialize;
use tokio::time::Instant;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::deadline::Deadline;
use crate::send_keys;
use crate::snapshot::get_screen;
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
    /// Wall-clock from the start of the run's time budget (before target
    /// resolution, when the caller started it there) to sentinel-seen, in
    /// milliseconds. Includes connection, submission, and poll latency, so it
    /// is an upper bound on the child's own runtime, not a precise
    /// measurement.
    pub duration_ms: u64,
    /// `true` when the `BEGIN` marker had scrolled out of the viewport, so
    /// `output` is best-effort visible context rather than a clean capture.
    /// Full capture needs scrollback (phux-o1v).
    pub truncated: bool,
}

/// Why [`run`] returned.
#[derive(Debug, Clone)]
pub enum RunOutcome {
    /// The sentinel was seen; the command finished.
    Completed(RunResult),
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
        /// The last completed screen read, or an empty default screen if
        /// the deadline expired during submission or the first read.
        screen: ScreenState,
    },
}

/// How much of a run's command line reached the pane before it gave up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submission {
    /// Nothing was sent: the budget ran out while connecting, resolving, or
    /// before the first input event.
    NotSent,
    /// Sending began but did not finish, even with [`SUBMIT_GRACE`]: the pane
    /// may hold part of the command line, unsubmitted, and the next input
    /// typed into it would be appended to that line.
    Partial,
    /// The whole command line and its Enter were acknowledged.
    Complete,
}

/// Extra time a submission that has already started gets past the deadline.
///
/// The command line and its Enter are separate `ROUTE_INPUT`s; cutting
/// between them would leave a typed but unsubmitted line at the prompt. So
/// input is never *started* after the deadline, but once started it gets
/// this long to finish before the run reports [`Submission::Partial`].
pub const SUBMIT_GRACE: Duration = Duration::from_secs(2);

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
/// returning `(row_index, code)`. Last-match wins so the freshest emission
/// beats any residual one. The command-echo row carries a literal `%d`
/// between the tags, so its parse fails and it is skipped.
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

/// Extract the command's output from the viewport given the `RC` marker's
/// row, returning `(output, truncated)`. Output is the rows strictly
/// between the printed `BEGIN` marker and the `RC` marker. When `BEGIN`
/// scrolled off, returns best-effort visible context with `truncated`.
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

/// Run `cmd` in the focused pane of `target`, capturing its exit code.
///
/// Submits the command (bracketed by sentinels) via the side-effect-free
/// `ROUTE_INPUT` path, like `send-keys` — so it neither attaches nor
/// resizes the pane — then polls the side-effect-free screen read until the
/// `RC` sentinel appears or `timeout` elapses.
///
/// The focused-pane convenience over [`run_in`]: it resolves `target`'s
/// focused pane via [`send_keys::send`] before polling. Callers that have
/// already resolved a selector to a concrete pane (the CLI's full `TARGET`
/// grammar, phux-n95) call [`run_in`] directly.
///
/// # Errors
///
/// Propagates [`AttachError`] from the input send or the screen reads.
pub async fn run(
    socket: &Path,
    target: AttachTarget,
    cmd: &str,
    nonce: &str,
    timeout: Option<Duration>,
) -> Result<RunOutcome, AttachError> {
    let deadline = Deadline::new(timeout);
    // Learn the exact pane the command will land in so we poll the same one
    // we write to, on the connection that then carries the input.
    let Some(connected) = deadline.run(connect_focused(socket, &target)).await else {
        return Ok(not_sent(cmd, deadline));
    };
    let (conn, pane) = connected?;
    submit_and_poll(conn, socket, pane, cmd, nonce, deadline).await
}

/// Connect and resolve `target`'s focused pane on that connection.
async fn connect_focused(
    socket: &Path,
    target: &AttachTarget,
) -> Result<(Connection, ResourceId), AttachError> {
    let mut conn = Connection::connect(socket).await?;
    let pane = send_keys::focused_pane(&mut conn, target).await?;
    Ok((conn, pane))
}

/// Run `cmd` in a pre-resolved `pane`, capturing its exit code.
///
/// The pane-targeted core of [`run`]: the caller has already resolved a
/// selector (the CLI's full `TARGET` grammar; phux-n95) to a concrete
/// [`ResourceId`], so the command lands on exactly that pane with no focus
/// heuristic. Submits via the side-effect-free `ROUTE_INPUT` path and polls
/// the side-effect-free screen read until the `RC` sentinel appears or
/// `timeout` elapses.
///
/// # Errors
///
/// Propagates [`AttachError`] from the input send or the screen reads.
pub async fn run_in(
    socket: &Path,
    pane: ResourceId,
    cmd: &str,
    nonce: &str,
    timeout: Option<Duration>,
) -> Result<RunOutcome, AttachError> {
    run_in_with_deadline(socket, pane, cmd, nonce, Deadline::new(timeout)).await
}

/// Like [`run_in`], with a budget started before target resolution.
///
/// The deadline bounds the connection and handshake, every screen read, and
/// the final sleep. Input is never started after it expires; once started,
/// the rest of the command line gets [`SUBMIT_GRACE`] to finish (see
/// [`Submission`]). Expiry does not kill the command or retract input
/// already delivered to the pane. Durations count from
/// [`Deadline::started_at`].
///
/// # Errors
///
/// See [`run_in`].
pub async fn run_in_with_deadline(
    socket: &Path,
    pane: ResourceId,
    cmd: &str,
    nonce: &str,
    deadline: Deadline,
) -> Result<RunOutcome, AttachError> {
    let Some(conn) = deadline.run(Connection::connect(socket)).await else {
        return Ok(not_sent(cmd, deadline));
    };
    submit_and_poll(conn?, socket, pane, cmd, nonce, deadline).await
}

/// Submit the sentinel-bracketed command over `conn`, then poll for its
/// `RC` sentinel. Shared tail of [`run`] and [`run_in_with_deadline`].
async fn submit_and_poll(
    mut conn: Connection,
    socket: &Path,
    pane: ResourceId,
    cmd: &str,
    nonce: &str,
    deadline: Deadline,
) -> Result<RunOutcome, AttachError> {
    let keys = [command_line(cmd, nonce), "Enter".to_owned()];
    let submission = submit(&mut conn, &pane, &keys, deadline).await?;
    drop(conn);
    if submission != Submission::Complete {
        return Ok(timed_out(cmd, deadline, submission, ScreenState::default()));
    }
    poll_for_rc(socket, pane, cmd, nonce, deadline).await
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
            let (output, truncated) = extract_output(&screen.lines, idx, nonce);
            return Ok(RunOutcome::Completed(RunResult {
                command: cmd.to_owned(),
                exit_code: code,
                output,
                duration_ms: duration_ms(start),
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
        screen,
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
        let line = command_line("ls -la", "42");
        assert_eq!(
            line,
            "printf 'PHUXrun42BEGIN\\n'; ls -la; printf '\\nPHUXrun42RC=%d=END\\n' $?"
        );
    }

    #[test]
    fn parses_exit_code_from_output_line_only() {
        // The echo row carries the literal %d; the RC output row carries 1.
        let lines = vec![
            "❯ printf 'PHUXrun42BEGIN\\n'; false; printf '\\nPHUXrun42RC=%d=END\\n' $?".to_owned(),
            "PHUXrun42BEGIN".to_owned(),
            "PHUXrun42RC=1=END".to_owned(),
        ];
        assert_eq!(parse_rc(&lines, "42"), Some((2, 1)));
    }

    #[test]
    fn ignores_echo_line_when_output_absent() {
        // Only the echo is visible (command still running): no parse.
        let lines = vec![
            "❯ printf 'PHUXrun42BEGIN\\n'; sleep 5; printf '\\nPHUXrun42RC=%d=END\\n' $?"
                .to_owned(),
        ];
        assert_eq!(parse_rc(&lines, "42"), None);
    }

    #[test]
    fn last_rc_marker_wins_over_a_stale_one() {
        // A residual marker from an earlier run with the SAME nonce must not
        // shadow the freshest one (defense-in-depth beyond unique nonces).
        let lines = vec![
            "PHUXrun7RC=0=END".to_owned(), // stale, from a prior run
            "PHUXrun7BEGIN".to_owned(),
            "new output".to_owned(),
            "PHUXrun7RC=3=END".to_owned(), // this run
        ];
        assert_eq!(parse_rc(&lines, "7"), Some((3, 3)));
    }

    #[test]
    fn extracts_output_between_begin_and_rc() {
        let lines = vec![
            "❯ printf 'PHUXrun7BEGIN\\n'; echo hi; printf '\\nPHUXrun7RC=%d=END\\n' $?".to_owned(),
            "PHUXrun7BEGIN".to_owned(),
            "hi".to_owned(),
            "PHUXrun7RC=0=END".to_owned(),
        ];
        let (idx, code) = parse_rc(&lines, "7").unwrap();
        let (output, truncated) = extract_output(&lines, idx, "7");
        assert_eq!(code, 0);
        assert_eq!(output, "hi");
        assert!(!truncated);
    }

    #[test]
    fn flags_truncated_when_begin_scrolled_off() {
        let lines = vec![
            "line that scrolled".to_owned(),
            "more output".to_owned(),
            "PHUXrun7RC=0=END".to_owned(),
        ];
        let (idx, _) = parse_rc(&lines, "7").unwrap();
        let (_output, truncated) = extract_output(&lines, idx, "7");
        assert!(truncated);
    }
}

/// phux-69pq.10: a stalled peer ends the run at the deadline, not never.
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

    async fn run_with_budget(socket: &Path) -> (RunOutcome, Duration) {
        let start = Instant::now();
        let deadline = Deadline::new(Some(BUDGET));
        let run = run_in_with_deadline(socket, ResourceId::local(1), "true", "n1", deadline);
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
        screen
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
        // The command line and its Enter are two acknowledged ROUTE_INPUTs.
        // A server that swallows the second one leaves the line typed but
        // unsubmitted; the run must say so rather than claim the command
        // was running, and must still give up (after the grace).
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
