//! Poll-floor wait primitive (ADR-0022 §4).
//!
//! Polls the side-effect-free [`get_screen`] read until a condition holds:
//! no shell integration, no new wire frames, safe against a pane another
//! client is using. Conditions are evaluated client-side, so new ones are
//! ordinary code rather than a frozen wire enum.
//!
//! Every condition matches [`match_lines`] (rows as *written*, soft wraps
//! joined), never `ScreenState::lines`: a needle straddling a wrap is absent
//! from painted rows, and matching those would hang to the timeout.
//!
//! [`get_screen`]: crate::snapshot::get_screen

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use phux_core::screen::{ROW_WINDOW_ALL, ScreenState, SemanticContent, row_window};
use phux_protocol::ids::ResourceId;
use regex::Regex;
use tokio::time::Instant;

use crate::attach::AttachError;
use crate::deadline::Deadline;
use crate::snapshot::ScreenPollConnection;

/// Default gap between polls.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Floor on the adaptive [`Condition::Idle`] poll interval.
const MIN_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Default dwell for [`Condition::Idle`].
pub const DEFAULT_IDLE_DWELL: Duration = Duration::from_millis(500);

/// A `--regex` pattern, compiled at argument-parse time so a bad pattern is
/// a usage error before the command runs, not a wait that never matches.
#[derive(Debug, Clone)]
pub struct MatchRegex(Regex);

impl MatchRegex {
    /// The pattern as the caller wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Whether one logical `line` matches (`^`/`$` anchor to the line).
    #[must_use]
    pub fn is_match(&self, line: &str) -> bool {
        self.0.is_match(line)
    }
}

impl FromStr for MatchRegex {
    /// The compile diagnostic, rendered by clap as the usage error's cause.
    type Err = String;

    fn from_str(pattern: &str) -> Result<Self, Self::Err> {
        Regex::new(pattern).map(Self).map_err(|err| err.to_string())
    }
}

/// How much of the pane a condition is matched against. The default is the
/// viewport only, every row.
#[derive(Debug, Clone, Copy, Default)]
pub struct MatchScope {
    /// `None` reads the viewport only. `Some(n)` also requests the last `n`
    /// history rows and narrows the search to the last `n` logical lines
    /// with content ([`ROW_WINDOW_ALL`] = all retained history). Unlike
    /// `snapshot --tail`, the viewport is not a floor: this scopes a search.
    pub tail: Option<u32>,
    /// Skip logical lines carrying an OSC-133 `Input` mark (the shell's echo
    /// of the typed command). Needs per-cell marks; without any, nothing is
    /// skipped (see [`has_semantic_marks`]).
    pub output_only: bool,
}

impl MatchScope {
    /// The `request_scrollback` argument the read needs to cover this scope.
    #[must_use]
    pub const fn history_request(&self) -> Option<u32> {
        self.tail
    }

    /// Whether the read must ask for per-cell semantic marks.
    #[must_use]
    pub const fn wants_cells(&self) -> bool {
        self.output_only
    }
}

/// What a poll loop waits for, evaluated against each fresh screen.
#[derive(Debug, Clone)]
pub enum Condition {
    /// Met once any logical line contains this substring.
    Contains(String),
    /// Met once any logical line matches this pattern (one line at a time).
    Matches(MatchRegex),
    /// Met once the matched lines hold still this long. The poll gap shrinks
    /// toward `dwell / 4` so a small dwell is honored. A pane that changes on
    /// every poll (a spinner, a clock) never settles; wait on text instead.
    Idle(Duration),
}

/// The poll gap for `condition` under the caller's `requested` ceiling:
/// `requested` for text conditions, `dwell / 4` clamped to
/// `[MIN_POLL_INTERVAL, requested]` for [`Condition::Idle`].
fn effective_interval(condition: &Condition, requested: Duration) -> Duration {
    match condition {
        Condition::Contains(_) | Condition::Matches(_) => requested,
        // Not `clamp`: that panics when `requested` is below the floor.
        Condition::Idle(dwell) => (*dwell / 4).max(MIN_POLL_INTERVAL).min(requested),
    }
}

/// Whether `screen` carries any OSC-133 semantic mark. `false` means
/// [`MatchScope::output_only`] has nothing to filter on, which callers
/// report rather than refuse.
#[must_use]
pub fn has_semantic_marks(screen: &ScreenState) -> bool {
    screen
        .cells
        .as_ref()
        .is_some_and(|cells| cells.iter().any(|cell| cell.semantic.is_some()))
}

/// Per viewport row: does the row carry an OSC-133 `Input` mark? (History
/// rows are never marked.)
fn input_marked_rows(screen: &ScreenState) -> Vec<bool> {
    let mut marked = vec![false; screen.lines.len()];
    let Some(cells) = screen.cells.as_ref() else {
        return marked;
    };
    for cell in cells {
        if matches!(cell.semantic, Some(SemanticContent::Input))
            && let Some(slot) = marked.get_mut(usize::from(cell.row))
        {
            *slot = true;
        }
    }
    marked
}

const INPUT_TAG: &str = "i";
const OUTPUT_TAG: &str = "o";

/// Per logical line of [`ScreenState::unwrapped_rows`]: did any row of its
/// run carry an `Input` mark? Computed by unwrapping a shadow stream of
/// one-character tags with the same wrap bits, so the runs line up with the
/// real unwrapper by construction.
fn input_marked_lines(screen: &ScreenState) -> Vec<bool> {
    let shadow = ScreenState {
        scrollback: vec![OUTPUT_TAG.to_owned(); screen.scrollback.len()],
        lines: input_marked_rows(screen)
            .into_iter()
            .map(|marked| if marked { INPUT_TAG } else { OUTPUT_TAG }.to_owned())
            .collect(),
        soft_wrap: screen.soft_wrap.clone(),
        ..ScreenState::default()
    };
    shadow
        .unwrapped_rows()
        .iter()
        .map(|tags| tags.contains(INPUT_TAG))
        .collect()
}

/// The lines a [`Condition`] is matched against: rows as written, narrowed
/// by `scope`.
///
/// Unwrap first (a run can straddle the history seam), then
/// window logical lines, then filter input, so `--tail N` means the pane's
/// last N lines.
#[must_use]
pub fn match_lines(screen: &ScreenState, scope: &MatchScope) -> Vec<String> {
    let mut rows = screen.unwrapped_rows();
    let trimmed = if scope.tail.is_some() {
        // Blank rows below the cursor are padding; counting them would make
        // a small window match nothing. Only windowed reads trim, so the
        // default projection stays byte-identical to the grid.
        let content = rows
            .iter()
            .rposition(|row| !row.trim().is_empty())
            .map_or(0, |last| last.saturating_add(1));
        let trimmed = rows.len().saturating_sub(content);
        rows.truncate(content);
        trimmed
    } else {
        0
    };
    let (lines, _truncated) = row_window(rows, scope.tail.unwrap_or(ROW_WINDOW_ALL));
    if !scope.output_only {
        return lines;
    }
    // The mask covers the untrimmed stream: align past the dropped front.
    let marked = input_marked_lines(screen);
    let kept = marked.len().saturating_sub(trimmed);
    let dropped = kept.saturating_sub(lines.len());
    lines
        .into_iter()
        .enumerate()
        .filter(|(i, _)| {
            !marked
                .get(i.saturating_add(dropped))
                .copied()
                .unwrap_or(false)
        })
        .map(|(_, line)| line)
        .collect()
}

/// Tracks whether the matched lines have held still for a dwell.
#[derive(Debug)]
struct IdleTracker {
    last: Option<Vec<String>>,
    stable_since: Instant,
}

impl IdleTracker {
    const fn new(now: Instant) -> Self {
        Self {
            last: None,
            stable_since: now,
        }
    }

    /// Record `lines` at `now`; `true` once unchanged for `dwell`. A first or
    /// changed observation resets the clock.
    fn observe(&mut self, lines: &[String], now: Instant, dwell: Duration) -> bool {
        if self.last.as_deref() == Some(lines) {
            now.duration_since(self.stable_since) >= dwell
        } else {
            self.stable_since = now;
            self.last = Some(lines.to_vec());
            false
        }
    }
}

/// The predicate the poll loop runs (and the tests run).
fn condition_met(
    condition: &Condition,
    lines: &[String],
    idle: &mut IdleTracker,
    now: Instant,
) -> bool {
    match condition {
        Condition::Contains(needle) => lines.iter().any(|line| line.contains(needle.as_str())),
        Condition::Matches(pattern) => lines.iter().any(|line| pattern.is_match(line)),
        Condition::Idle(dwell) => idle.observe(lines, now, *dwell),
    }
}

/// Why [`poll_until`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The [`Condition`] was satisfied.
    Met,
    /// The overall timeout elapsed before the condition held.
    TimedOut,
}

/// The result of a poll loop.
#[derive(Debug, Clone)]
pub struct WaitResult {
    /// Why polling stopped.
    pub outcome: WaitOutcome,
    /// The most recent completed screen read (default if none completed).
    pub screen: ScreenState,
    /// Number of screen reads performed.
    pub polls: u32,
}

/// Poll `terminal_id` until `condition` holds or `timeout` elapses (`None`
/// waits forever), with the default [`MatchScope`]. A timeout is
/// [`WaitOutcome::TimedOut`], not an error.
///
/// # Errors
///
/// Propagates [`AttachError`] from the screen read.
pub async fn poll_until(
    socket: &Path,
    terminal_id: ResourceId,
    condition: &Condition,
    timeout: Option<Duration>,
    interval: Duration,
) -> Result<WaitResult, AttachError> {
    poll_until_scoped_with_deadline(
        socket,
        terminal_id,
        condition,
        Deadline::new(timeout),
        interval,
        &MatchScope::default(),
    )
    .await
}

/// However short a wait's budget, its first screen read gets at least this
/// long from the budget's start, so a zero timeout means "check once, now".
pub const FIRST_READ_FLOOR: Duration = Duration::from_secs(2);

/// [`poll_until`] with an explicit scope and a budget that also bounds
/// connection setup; the first read is bounded by the budget raised to
/// [`FIRST_READ_FLOOR`]. One connection is reused across polls.
///
/// # Errors
///
/// See [`poll_until`].
#[expect(
    clippy::significant_drop_tightening,
    reason = "screen polling deliberately reuses one connection across loop iterations"
)]
pub async fn poll_until_scoped_with_deadline(
    socket: &Path,
    terminal_id: ResourceId,
    condition: &Condition,
    deadline: Deadline,
    interval: Duration,
    scope: &MatchScope,
) -> Result<WaitResult, AttachError> {
    let start = Instant::now();
    let mut polls: u32 = 0;
    let mut idle = IdleTracker::new(start);
    let interval = effective_interval(condition, interval);
    let mut screen = ScreenState::default();
    let mut connection = ScreenPollConnection::new(socket);

    loop {
        let read = connection.read(
            terminal_id.clone(),
            scope.history_request(),
            scope.wants_cells(),
            crate::snapshot::SCREEN_FORMAT_NONE,
        );
        let budget = if polls == 0 {
            deadline.floored(FIRST_READ_FLOOR)
        } else {
            deadline
        };
        let Some(read) = budget.run(read).await else {
            break;
        };
        screen = read?;
        polls = polls.saturating_add(1);
        let lines = match_lines(&screen, scope);
        if condition_met(condition, &lines, &mut idle, Instant::now()) {
            return Ok(WaitResult {
                outcome: WaitOutcome::Met,
                screen,
                polls,
            });
        }
        if deadline.run(tokio::time::sleep(interval)).await.is_none() {
            break;
        }
    }
    Ok(WaitResult {
        outcome: WaitOutcome::TimedOut,
        screen,
        polls,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use phux_core::screen::{CellInfo, CellStyle, SoftWrap};

    use super::*;

    fn screen(lines: &[&str]) -> ScreenState {
        ScreenState {
            schema_version: phux_core::screen::SCHEMA_VERSION,
            pane: 1,
            cols: 80,
            rows: u16::try_from(lines.len()).unwrap_or(0),
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            ..ScreenState::default()
        }
    }

    fn wrapped(rows: &[&str]) -> ScreenState {
        ScreenState {
            soft_wrap: Some(SoftWrap {
                lines: vec![0],
                scrollback: Vec::new(),
            }),
            ..screen(rows)
        }
    }

    fn lines(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Run the loop's own predicate once against `screen` under `scope`.
    fn met(condition: &Condition, screen: &ScreenState, scope: &MatchScope) -> bool {
        let mut idle = IdleTracker::new(Instant::now());
        condition_met(
            condition,
            &match_lines(screen, scope),
            &mut idle,
            Instant::now(),
        )
    }

    fn contains(needle: &str) -> Condition {
        Condition::Contains(needle.to_owned())
    }

    fn regex(pattern: &str) -> Condition {
        Condition::Matches(pattern.parse().expect("test pattern compiles"))
    }

    const fn tail(n: u32) -> MatchScope {
        MatchScope {
            tail: Some(n),
            output_only: false,
        }
    }

    const OUTPUT_ONLY: MatchScope = MatchScope {
        tail: None,
        output_only: true,
    };

    fn mark(row: u16, semantic: SemanticContent) -> CellInfo {
        CellInfo {
            col: 0,
            row,
            semantic: Some(semantic),
            style: CellStyle::default(),
        }
    }

    /// Text conditions match logical lines: across a soft wrap and the
    /// history seam, one line at a time, and verbatim without wrap info.
    #[test]
    fn text_conditions_match_logical_lines() {
        let plain = screen(&["running 12 tests", "test result: ok. 12 passed"]);
        let wrap = wrapped(&["cargo test: 41 passed, 1 fai", "led, 0 skipped"]);
        let seam = ScreenState {
            scrollback: lines(&["quiet", "BUILD SUCC"]),
            soft_wrap: Some(SoftWrap {
                lines: Vec::new(),
                scrollback: vec![1],
            }),
            ..screen(&["ESSFUL in 4s"])
        };
        let default = MatchScope::default();
        assert!(!wrap.lines.iter().any(|l| l.contains("1 failed")));
        assert!(!plain.has_soft_wrap_info());
        for (condition, s, scope, want) in [
            (contains("12 passed"), &plain, default, true),
            (contains("MISSING"), &plain, default, false),
            (contains("tests test result"), &plain, default, false),
            (contains("1 failed"), &wrap, default, true),
            (contains("BUILD SUCCESSFUL"), &seam, tail(0), true),
            (
                regex(r"^test result: ok\. \d+ passed$"),
                &plain,
                default,
                true,
            ),
            (regex(r"tests\s*test result"), &plain, default, false),
            (regex(r"passed, \d+ failed"), &wrap, default, true),
        ] {
            assert_eq!(met(&condition, s, &scope), want, "{condition:?}");
        }
    }

    #[test]
    fn an_invalid_regex_fails_to_compile() {
        let err = "(unclosed".parse::<MatchRegex>().unwrap_err();
        assert!(
            err.contains("unclosed") || err.contains("regex parse error"),
            "{err:?}"
        );
    }

    /// The command-echo footgun: waiting for text that also appears in the
    /// typed command matches its own echo instantly without the filter.
    #[test]
    fn output_only_skips_the_echoed_command() {
        let s = ScreenState {
            cells: Some(vec![
                mark(0, SemanticContent::Prompt),
                mark(0, SemanticContent::Input),
            ]),
            ..screen(&["$ cargo test | grep 'test result: ok'", "running 12 tests"])
        };
        assert!(met(
            &contains("test result: ok"),
            &s,
            &MatchScope::default()
        ));
        assert!(!met(&contains("test result: ok"), &s, &OUTPUT_ONLY));
        assert!(met(&contains("running 12 tests"), &s, &OUTPUT_ONLY));

        // A wrapped command is input across its whole logical line.
        let long = ScreenState {
            cells: Some(vec![mark(0, SemanticContent::Input)]),
            ..wrapped(&["$ cargo test 2>&1 | tee log # test re", "sult: ok"])
        };
        assert!(!met(&contains("test result: ok"), &long, &OUTPUT_ONLY));

        // Prompt-marked text is not input.
        let prompt = ScreenState {
            cells: Some(vec![mark(1, SemanticContent::Prompt)]),
            ..screen(&["done", "user@host ~/src $"])
        };
        assert!(met(&contains("user@host"), &prompt, &OUTPUT_ONLY));
    }

    /// Without OSC-133 marks the filter fails open, and the caller can tell.
    #[test]
    fn output_only_without_semantic_marks_filters_nothing() {
        let s = ScreenState {
            cells: Some(vec![CellInfo {
                col: 0,
                row: 0,
                semantic: None,
                style: CellStyle::default(),
            }]),
            ..screen(&["$ cargo test", "running"])
        };
        assert!(!has_semantic_marks(&s));
        assert!(!has_semantic_marks(&screen(&["$ cargo test"])));
        assert!(met(&contains("cargo test"), &s, &OUTPUT_ONLY));
        let marked = ScreenState {
            cells: Some(vec![mark(0, SemanticContent::Input)]),
            ..screen(&["$ cargo test"])
        };
        assert!(has_semantic_marks(&marked));
    }

    #[test]
    fn tail_scopes_the_search_to_the_last_n_logical_lines() {
        let s = ScreenState {
            scrollback: lines(&["MARKER far above", "noise"]),
            ..screen(&["still noise", "tail line"])
        };
        assert!(met(&contains("MARKER"), &s, &tail(0)));
        assert!(!met(&contains("MARKER"), &s, &tail(2)));
        assert_eq!(
            match_lines(&s, &tail(2)),
            lines(&["still noise", "tail line"])
        );
        // Logical lines, not painted rows.
        let wrap = wrapped(&["BUILD SUCC", "ESSFUL", "next"]);
        assert_eq!(
            match_lines(&wrap, &tail(2)),
            lines(&["BUILD SUCCESSFUL", "next"])
        );
    }

    /// Blank rows under the cursor do not count toward a window, and the
    /// default projection is untrimmed.
    #[test]
    fn tail_does_not_count_the_blank_rows_under_the_cursor() {
        let s = screen(&["MARKER here", "$", "", "", ""]);
        assert_eq!(match_lines(&s, &tail(2)), lines(&["MARKER here", "$"]));
        assert_eq!(match_lines(&s, &MatchScope::default()).len(), s.lines.len());
    }

    /// The window applies before the input filter, and the mask survives
    /// both the trimmed end and the windowed front.
    #[test]
    fn tail_and_output_only_compose_on_the_same_window() {
        let both = MatchScope {
            tail: Some(2),
            output_only: true,
        };
        let s = ScreenState {
            cells: Some(vec![mark(1, SemanticContent::Input)]),
            ..screen(&["older output", "$ echo MARKER", "fresh output"])
        };
        assert_eq!(match_lines(&s, &both), lines(&["fresh output"]));
        let padded = ScreenState {
            cells: Some(vec![mark(2, SemanticContent::Input)]),
            ..screen(&["oldest", "output line", "$ echo MARKER", "", ""])
        };
        assert_eq!(match_lines(&padded, &both), lines(&["output line"]));
    }

    /// A change confined to a row outside the window is not activity.
    #[test]
    fn idle_observes_the_scoped_projection() {
        let before = ScreenState {
            scrollback: lines(&["spinner |"]),
            ..screen(&["settled"])
        };
        let after = ScreenState {
            scrollback: lines(&["spinner /"]),
            ..screen(&["settled"])
        };
        assert_eq!(
            match_lines(&before, &tail(1)),
            match_lines(&after, &tail(1))
        );
    }

    #[test]
    fn effective_interval_clamps_idle_polling() {
        let ms = Duration::from_millis;
        for (condition, requested, want) in [
            (regex("x"), DEFAULT_POLL_INTERVAL, DEFAULT_POLL_INTERVAL),
            (contains("x"), DEFAULT_POLL_INTERVAL, DEFAULT_POLL_INTERVAL),
            (
                Condition::Idle(ms(40)),
                DEFAULT_POLL_INTERVAL,
                MIN_POLL_INTERVAL,
            ),
            (Condition::Idle(ms(400)), DEFAULT_POLL_INTERVAL, ms(100)),
            (
                Condition::Idle(ms(10_000)),
                DEFAULT_POLL_INTERVAL,
                DEFAULT_POLL_INTERVAL,
            ),
            // A ceiling below the floor must not panic; the cap wins.
            (Condition::Idle(ms(400)), ms(10), ms(10)),
        ] {
            assert_eq!(
                effective_interval(&condition, requested),
                want,
                "{condition:?}"
            );
        }
    }

    #[test]
    fn idle_settles_only_after_an_unchanged_dwell() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let dwell = ms(100);
        let mut idle = IdleTracker::new(t0);
        // The first read only records a baseline, however late.
        assert!(!idle.observe(&lines(&["a"]), t0 + ms(10_000), dwell));
        assert!(!idle.observe(&lines(&["a"]), t0 + ms(10_060), dwell));
        assert!(idle.observe(&lines(&["a"]), t0 + ms(10_160), dwell));
        // Any change restarts the clock.
        assert!(!idle.observe(&lines(&["b"]), t0 + ms(10_500), dwell));
        assert!(!idle.observe(&lines(&["b"]), t0 + ms(10_560), dwell));
        assert!(idle.observe(&lines(&["b"]), t0 + ms(10_610), dwell));
    }
}

/// A stalled peer ends the wait at the deadline, not never.
#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod deadline_tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use phux_protocol::ResourceId;
    use tokio::net::UnixListener;
    use tokio::time::Instant;

    use phux_core::screen::{SCHEMA_VERSION, ScreenState};

    use super::{
        Condition, DEFAULT_POLL_INTERVAL, FIRST_READ_FLOOR, MatchScope, WaitOutcome, WaitResult,
        poll_until_scoped_with_deadline,
    };
    use crate::deadline::Deadline;
    use crate::testkit::{self, ScriptSpec, ScriptedServer};

    const BUDGET: Duration = Duration::from_millis(300);
    /// Slack for a loaded machine; a regression overruns by far more.
    const TOLERANCE: Duration = Duration::from_secs(3);
    /// A regression fails here instead of hanging the test run.
    const WEDGE: Duration = Duration::from_secs(10);

    async fn wait_for(socket: &Path, budget: Duration, needle: &str) -> (WaitResult, Duration) {
        let condition = Condition::Contains(needle.to_owned());
        let scope = MatchScope::default();
        let start = Instant::now();
        let poll = poll_until_scoped_with_deadline(
            socket,
            ResourceId::local(1),
            &condition,
            Deadline::new(Some(budget)),
            DEFAULT_POLL_INTERVAL,
            &scope,
        );
        let result = tokio::time::timeout(WEDGE, poll)
            .await
            .expect("the wait must return; a timeout here is the wedge itself")
            .expect("a stalled peer is a timeout, not an error");
        (result, start.elapsed())
    }

    fn assert_timed_out_on_time(result: &WaitResult, elapsed: Duration) {
        assert!(matches!(result.outcome, WaitOutcome::TimedOut));
        assert_eq!(result.polls, 0, "no read ever completed");
        // A stall costs the later of the budget and the first-read floor.
        let min = BUDGET.max(FIRST_READ_FLOOR);
        assert!(elapsed >= min, "gave up early after {elapsed:?}");
        assert!(elapsed < min + TOLERANCE, "overran: {elapsed:?}");
    }

    fn showing(text: &str) -> ScreenState {
        ScreenState {
            schema_version: SCHEMA_VERSION,
            pane: 1,
            cols: 80,
            rows: 1,
            lines: vec![text.to_owned()],
            ..ScreenState::default()
        }
    }

    #[tokio::test]
    async fn a_peer_that_never_answers_hello_ends_the_wait_on_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("silent.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let peer = tokio::spawn(testkit::hold_silent(listener));

        let (result, elapsed) = wait_for(&socket, BUDGET, "never printed").await;
        assert_timed_out_on_time(&result, elapsed);
        peer.abort();
    }

    #[tokio::test]
    async fn a_peer_that_never_answers_get_screen_ends_the_wait_on_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("wedged.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        let peer = tokio::spawn(testkit::serve_every(listener, || {
            ScriptSpec::new().wedge_screen_reads()
        }));

        let (result, elapsed) = wait_for(&socket, BUDGET, "never printed").await;
        assert_timed_out_on_time(&result, elapsed);
        peer.abort();
    }

    /// A zero budget checks exactly once: met if the screen matches, else a
    /// prompt timeout.
    #[tokio::test]
    async fn a_zero_budget_checks_once() {
        for (text, outcome) in [
            ("READY", WaitOutcome::Met),
            ("still building", WaitOutcome::TimedOut),
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let socket = dir.path().join("zero.sock");
            let listener = UnixListener::bind(&socket).expect("bind");
            let screen = showing(text);
            let peer = tokio::spawn(testkit::serve_every(listener, move || {
                ScriptSpec::new().screen(&screen)
            }));

            let (result, elapsed) = wait_for(&socket, Duration::ZERO, "READY").await;
            assert_eq!(result.outcome, outcome, "{text}");
            assert_eq!(result.polls, 1, "{text}");
            assert_eq!(result.screen.lines, vec![text.to_owned()]);
            assert!(
                elapsed < TOLERANCE,
                "no polling past the one read: {elapsed:?}"
            );
            peer.abort();
        }
    }

    /// Each wait needs several reads to satisfy `Idle`, yet opens exactly one
    /// connection, however many agents poll at once.
    #[tokio::test]
    async fn each_wait_polls_over_one_persistent_connection() {
        for agents in [1, 8, 32] {
            let dir = tempfile::tempdir().expect("tempdir");
            let socket = dir.path().join(format!("poll-{agents}.sock"));
            let listener = UnixListener::bind(&socket).expect("bind");
            let accepted = Arc::new(AtomicUsize::new(0));
            let server_count = Arc::clone(&accepted);
            let ready = showing("steady");
            let peer = tokio::spawn(async move {
                loop {
                    let (stream, _) = listener.accept().await.expect("accept polling client");
                    server_count.fetch_add(1, Ordering::Relaxed);
                    let spec = ScriptSpec::new().screen(&ready);
                    tokio::spawn(ScriptedServer::on_stream(stream, spec).run());
                }
            });

            let mut waits = Vec::with_capacity(agents);
            for _ in 0..agents {
                let socket = socket.clone();
                waits.push(tokio::spawn(async move {
                    poll_until_scoped_with_deadline(
                        &socket,
                        ResourceId::local(1),
                        &Condition::Idle(Duration::ZERO),
                        Deadline::new(Some(Duration::from_secs(2))),
                        Duration::from_millis(1),
                        &MatchScope::default(),
                    )
                    .await
                    .expect("poll wait")
                }));
            }
            for wait in waits {
                let result = wait.await.expect("poll task");
                assert!(matches!(result.outcome, WaitOutcome::Met));
                assert!(result.polls >= 2, "idle requires repeated reads");
            }
            assert_eq!(accepted.load(Ordering::Relaxed), agents);
            peer.abort();
        }
    }
}
