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
    /// Wall-clock from submit to sentinel-seen, in milliseconds. Includes
    /// poll latency, so it is an upper bound on the child's own runtime,
    /// not a precise measurement.
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
        /// Wall-clock waited before giving up, in milliseconds.
        duration_ms: u64,
        /// The last screen observed.
        screen: ScreenState,
    },
}

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
    let line = command_line(cmd, nonce);
    let start = Instant::now();
    // Submit the command; learn the exact pane it landed in so we poll the
    // same one we wrote to.
    let pane = send_keys::send(socket, target, &[line, "Enter".to_owned()]).await?;
    poll_for_rc(socket, pane, cmd, nonce, timeout, start).await
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
    let line = command_line(cmd, nonce);
    let start = Instant::now();
    send_keys::send_to(socket, pane.clone(), &[line, "Enter".to_owned()]).await?;
    poll_for_rc(socket, pane, cmd, nonce, timeout, start).await
}

/// Poll `pane`'s screen until the `RC` sentinel for `nonce` appears or
/// `timeout` (measured from `start`) elapses. Shared tail of [`run`] and
/// [`run_in`].
async fn poll_for_rc(
    socket: &Path,
    pane: ResourceId,
    cmd: &str,
    nonce: &str,
    timeout: Option<Duration>,
    start: Instant,
) -> Result<RunOutcome, AttachError> {
    loop {
        let screen = get_screen(socket, pane.clone()).await?;
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
        if timeout.is_some_and(|t| start.elapsed() >= t) {
            return Ok(RunOutcome::TimedOut {
                command: cmd.to_owned(),
                duration_ms: duration_ms(start),
                screen,
            });
        }
        tokio::time::sleep(DEFAULT_POLL_INTERVAL).await;
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
