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
//!
//! # The available-shell precondition
//!
//! That whole scheme is a *typed shell command line*, so it is only a
//! command at all when a shell is what reads it. Against a pane running
//! `vim`, `less`, or a wedged process the same bytes are keystrokes into
//! that application — normal-mode editing commands against someone's buffer
//! — and the poll then runs to its timeout with nothing to report. So `run`
//! evaluates the same precondition `phux agent start` does
//! ([`phux_client::agent_meta::pane_shell_availability`]) before typing
//! anything, refuses fail-closed when a shell is not in the foreground, and
//! takes `force` as the opt-out for a caller who knows better.
//!
//! [`phux_client::agent_meta::pane_shell_availability`]: crate::agent_meta::pane_shell_availability

use std::path::Path;
use std::time::Duration;

use phux_core::screen::{ROW_WINDOW_ALL, ScreenState};
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::AttachTarget;
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
    /// Wall-clock from the start of the run's time budget (before target
    /// resolution, when the caller started it there) to sentinel-seen, in
    /// milliseconds. Includes connection, submission, and poll latency, so it
    /// is an upper bound on the child's own runtime, not a precise
    /// measurement.
    pub duration_ms: u64,
    /// `true` when the `BEGIN` marker was not in the captured span at all,
    /// so `output` is best-effort trailing context rather than a clean
    /// capture.
    ///
    /// The capture reads retained scrollback, not just the viewport, so this
    /// is no longer "the command outscrolled the viewport" — it means the
    /// span genuinely exceeded what the server still retains (or what the row
    /// window would carry).
    pub truncated: bool,
}

/// Why [`run`] returned.
#[derive(Debug, Clone)]
pub enum RunOutcome {
    /// The sentinel was seen; the command finished.
    Completed(RunResult),
    /// The available-shell precondition failed, so **nothing was typed**.
    ///
    /// Distinct from every timeout: the pane is intact, the command never
    /// ran, and the caller is told what is in the foreground instead.
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
        /// The last completed screen read, or an empty default screen if
        /// the deadline expired during submission or the first read.
        /// Boxed: `ScreenState` grew past `RunResult`'s size once the D9
        /// `rendered` field landed, and this variant is the timeout path —
        /// no reason to pay for it on the common `Completed` one.
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

/// The history window the post-sentinel capture asks for.
///
/// [`ROW_WINDOW_ALL`] — every retained row, clamped server-side to
/// [`phux_core::screen::ROW_WINDOW_MAX`]. Only the one read that follows the
/// `RC` sentinel pays for it: the poll itself stays the cheap viewport read,
/// so a long-running command is not billed a full transcript every 150 ms.
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

/// Extract the command's output from `lines` given the `RC` marker's row,
/// returning `(output, truncated)`. Output is the rows strictly between the
/// printed `BEGIN` marker and the `RC` marker. When `BEGIN` is not in the
/// captured span at all, returns best-effort trailing context with
/// `truncated`.
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
/// `force` skips the available-shell precondition; see the module docs.
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
    force: bool,
) -> Result<RunOutcome, AttachError> {
    let deadline = Deadline::new(timeout);
    // Learn the exact pane the command will land in so we poll the same one
    // we write to, on the connection that then carries the input.
    let Some(connected) = deadline.run(connect_focused(socket, &target)).await else {
        return Ok(not_sent(cmd, deadline));
    };
    let (conn, pane) = connected?;
    submit_and_poll(conn, socket, pane, cmd, nonce, deadline, force).await
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
/// `force` skips the available-shell precondition; see the module docs.
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
    force: bool,
) -> Result<RunOutcome, AttachError> {
    run_in_with_deadline(socket, pane, cmd, nonce, Deadline::new(timeout), force).await
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
    force: bool,
) -> Result<RunOutcome, AttachError> {
    let Some(conn) = deadline.run(Connection::connect(socket)).await else {
        return Ok(not_sent(cmd, deadline));
    };
    submit_and_poll(conn?, socket, pane, cmd, nonce, deadline, force).await
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
/// typing. `None` means "go ahead".
///
/// Fails CLOSED in both directions: an unevaluable precondition is a
/// refusal ([`ShellAvailability::Unanswerable`]), and a budget that expires
/// during the two side-effect-free reads ends the run as
/// [`Submission::NotSent`] rather than letting the command line through
/// unchecked.
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
            // Stamped at sentinel-seen, before the capture read: the command
            // is already over, and billing its transcript fetch to the
            // child's runtime would make the number say something else.
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
/// span, returning `None` when that read cannot be made or cannot be
/// trusted.
///
/// The poll that spotted the sentinel only ever saw the viewport, so a
/// command that printed more than a screenful had its `BEGIN` marker above
/// the top row — historically reported as `truncated` output with the last
/// ~24 rows in it, for no better reason than that the poll read was the
/// only read. The command has finished by the time this runs, so the extra
/// round trip costs one read and the rows are no longer moving.
///
/// `None` (a read that failed, expired, or came back without this run's
/// sentinel) leaves the caller on the viewport extraction: a capture that
/// did not arrive must not cost the exit code that did. `truncated` then
/// means what it says — the span outran even the retained history.
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

/// A capture read's rows as one stream: retained history first, then the
/// viewport. The `BEGIN` marker lands in whichever half it scrolled into,
/// and `extract_output`'s indices are over this joined stream.
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

    /// `force`: these fixtures are about the submit/poll budget, and a
    /// stalled peer cannot answer the precondition's reads either — leaving
    /// it on would make every case below a precondition timeout instead of
    /// the one being exercised. The precondition has its own tests.
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

    /// The defect: a pane running `vim` was typed a shell command line —
    /// normal-mode commands against someone's buffer — and the run then
    /// polled to its timeout with nothing to report. The precondition
    /// refuses instead, naming what IS in the foreground.
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

    /// No occupant record and no OSC-133 marks is not evidence of safety:
    /// the precondition fails CLOSED rather than typing blind.
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

    /// `force` is the escape hatch, and it preserves today's behaviour
    /// exactly: the command line is typed and the sentinel parsed, with the
    /// precondition never consulted.
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

    /// The second defect: with `BEGIN` scrolled out of the viewport the
    /// viewport-only capture returned the last few rows and claimed
    /// `truncated`. The scrollback-aware capture returns the whole span.
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
