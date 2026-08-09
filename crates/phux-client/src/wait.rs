//! Poll-floor wait primitive (phux-cfd, ADR-0022 §4).
//!
//! `wait` is the floor of the event surface: rather than a wire
//! subscription, it polls the side-effect-free [`get_screen`] read until a
//! condition holds. This always works — no shell integration, no new wire
//! frames — and because [`get_screen`] never attaches or resizes, polling
//! is safe against a pane another client is using.
//!
//! Conditions are evaluated **client-side** against each fresh
//! [`ScreenState`]. That is deliberate (ADR-0022 §4, "no one-way doors"):
//! the matchable set grows here as ordinary code, never as a frozen wire
//! enum. Server-pushed events (`command_finished`, `bell`, …) are a future
//! *additive* acceleration of this same contract, not a replacement.
//!
//! # What a condition is matched against
//!
//! Never `ScreenState::lines`. Every condition here is evaluated over
//! [`match_lines`], which is [`ScreenState::unwrapped_rows`] (rows as
//! *written* — soft-wrapped runs joined) narrowed by a [`MatchScope`]. A
//! substring that straddles a terminal-edge wrap is absent from the rows as
//! painted, so matching raw rows fails silently and only for long lines:
//! the wait runs to its timeout and the caller sees a hang with no
//! explanation. That is the worst shape a bug can have, which is why the
//! unwrapped projection is the only haystack this module exposes.
//!
//! [`get_screen`]: crate::snapshot::get_screen

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use phux_core::screen::{ROW_WINDOW_ALL, ScreenState, SemanticContent, row_window};
use phux_protocol::ids::TerminalId;
use regex::Regex;
use tokio::time::Instant;

use crate::attach::AttachError;
use crate::snapshot::get_screen_scrollback;

/// Default gap between polls. Below human settle perception, well above
/// the per-poll round-trip cost on a local UDS.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(150);

/// Floor on the adaptive poll interval.
///
/// For [`Condition::Idle`] the loop shrinks its poll gap toward `dwell / 4`
/// so a small `--idle` is honored instead of being rounded up to a full
/// [`DEFAULT_POLL_INTERVAL`]; this is the smallest gap it will use, bounding
/// the per-poll round-trip cost.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Default dwell for [`Condition::Idle`] when the caller gives no explicit
/// duration: the pane must hold still this long to count as settled.
pub const DEFAULT_IDLE_DWELL: Duration = Duration::from_millis(500);

/// A `--regex` pattern, compiled once at argument-parse time.
///
/// The newtype exists so the CLI can spell the flag as
/// `Option<MatchRegex>` and let clap's `FromStr` value parser reject a bad
/// pattern as a **usage error, before the command runs at all**. An invalid
/// regex is a mistake in the invocation, not a condition that might come
/// true on the next poll; discovering it inside the loop would either abort
/// a wait that had already started or — worse — silently never match.
#[derive(Debug, Clone)]
pub struct MatchRegex(Regex);

impl MatchRegex {
    /// The pattern as the caller wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Whether `line` — one logical line — matches.
    ///
    /// Matching is per line, never across the joined stream: `^`/`$` anchor
    /// to the line the caller sees, and no pattern can span two of them.
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

/// How much of the pane a condition is matched against.
///
/// The default — viewport only, every row — is what `wait` has always read,
/// and costs no history request.
#[derive(Debug, Clone, Copy, Default)]
pub struct MatchScope {
    /// Row-count window for the search.
    ///
    /// `None` reads the viewport only. `Some(n)` also requests the most
    /// recent `n` history rows and then narrows the search to the last `n`
    /// **logical** lines; [`ROW_WINDOW_ALL`] (`0`) as the count requests
    /// all retained history, still capped by
    /// [`phux_core::screen::ROW_WINDOW_MAX`].
    ///
    /// Unlike `snapshot --tail`, the viewport is **not** a floor here: this
    /// window scopes a *search*, not a returned grid, so nothing about
    /// `rows` / `cursor` / `cells` is claimed and a window narrower than the
    /// viewport is honest. `--tail 3` really does mean "only the last three
    /// logical lines count".
    ///
    /// The count is over lines with content: blank rows below the cursor
    /// are the unused part of a full-height grid, and counting them would
    /// make a small window match nothing on a pane whose prompt sits near
    /// the top.
    pub tail: Option<u32>,
    /// Skip logical lines carrying an OSC-133 `Input` mark — the shell's
    /// echo of the command the caller just typed.
    ///
    /// Requires per-cell marks in the read (see [`Self::wants_cells`]) and
    /// therefore a shell with OSC-133 integration. With no marks at all
    /// nothing is skipped: see [`has_semantic_marks`], which the caller uses
    /// to say so out loud rather than letting the filter be a silent no-op.
    pub output_only: bool,
}

impl MatchScope {
    /// The `request_scrollback` argument the read needs to cover this scope.
    #[must_use]
    pub const fn history_request(&self) -> Option<u32> {
        self.tail
    }

    /// Whether the read must ask for per-cell semantic marks.
    ///
    /// Only [`Self::output_only`] needs them, so the default poll stays the
    /// cheap viewport read it has always been.
    #[must_use]
    pub const fn wants_cells(&self) -> bool {
        self.output_only
    }
}

/// What a poll loop waits for. Evaluated CLI-side against each fresh
/// screen, so new variants are additive (ADR-0022 §4).
#[derive(Debug, Clone)]
pub enum Condition {
    /// Met once any **logical line** contains this substring — the rows as
    /// written, so a match that straddles a soft wrap is found.
    Contains(String),
    /// Met once any **logical line** matches this pattern. One line at a
    /// time: the pattern is never run against the joined stream, so `^`/`$`
    /// mean the ends of a line the caller can see.
    Matches(MatchRegex),
    /// Met once the matched text holds still for this long — the pane has
    /// "settled" (output stopped, prompt likely back).
    ///
    /// Settling is byte-exact over the same [`match_lines`] projection the
    /// text conditions use, so a [`MatchScope`] narrows what counts as
    /// activity too: `--idle` with `--tail 3` settles when the last three
    /// logical lines hold still, whatever a spinner further up is doing.
    ///
    /// Idle resolution is bounded by the poll interval, which for this
    /// condition is shrunk adaptively toward `dwell / 4` (down to
    /// [`MIN_POLL_INTERVAL`]) so a small dwell is honored rather than
    /// rounded up to a full [`DEFAULT_POLL_INTERVAL`]; see
    /// [`effective_interval`].
    ///
    /// Limitation: a pane whose visible content changes on *every* poll —
    /// a spinner, a live clock, a progress bar — never holds still and so
    /// never satisfies `Idle`. There is no heuristic here that ignores
    /// "churning" rows. For such panes, wait on specific output with
    /// `--until` instead, and always pass a `--timeout` as a backstop.
    Idle(Duration),
}

/// The poll gap [`poll_until`] should use for `condition`, given the
/// caller's `requested` ceiling.
///
/// For the text conditions the gap is just `requested` (no benefit to
/// polling faster than the caller asked). For [`Condition::Idle`] the gap
/// shrinks to `clamp(dwell / 4, MIN_POLL_INTERVAL, requested)` so a dwell
/// smaller than `requested` is still observed at sub-dwell granularity
/// instead of being rounded up to one full poll. Sampling at roughly a
/// quarter of the dwell means the tracker sees several unchanged reads
/// before the dwell elapses, so the reported settle time tracks the
/// request rather than overshooting by a whole interval.
#[must_use]
pub fn effective_interval(condition: &Condition, requested: Duration) -> Duration {
    match condition {
        Condition::Contains(_) | Condition::Matches(_) => requested,
        // `max(MIN_POLL_INTERVAL)` first, then cap at `requested`: this is
        // `clamp` without its `min > max` panic should a caller pass a
        // `requested` below the floor.
        Condition::Idle(dwell) => (*dwell / 4).max(MIN_POLL_INTERVAL).min(requested),
    }
}

/// Whether `screen` carries any OSC-133 semantic mark at all.
///
/// `false` means the pane's shell has no OSC-133 integration (or the read
/// did not ask for cells), so [`MatchScope::output_only`] has nothing to
/// filter on and is a no-op. It cannot distinguish that from "a marked
/// screen that happens to show no prompt or input right now" — which is
/// exactly why this is reported to the operator rather than being turned
/// into a refusal: refusing would hang a wait that is otherwise fine, and
/// hanging is the failure this whole module exists to remove.
#[must_use]
pub fn has_semantic_marks(screen: &ScreenState) -> bool {
    screen
        .cells
        .as_ref()
        .is_some_and(|cells| cells.iter().any(|cell| cell.semantic.is_some()))
}

/// Per **viewport row**: does the row carry an OSC-133 `Input` mark?
///
/// Cells are viewport-relative and sparse, so history rows are never marked
/// (the caller treats them as output, which is what scrolled-off rows are).
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

/// Tag for a row carrying an `Input` mark, in the shadow stream below.
const INPUT_TAG: &str = "i";
/// Tag for any other row, in the shadow stream below.
const OUTPUT_TAG: &str = "o";

/// Per **logical line** of [`ScreenState::unwrapped_rows`]: did any row of
/// the run that produced it carry an `Input` mark?
///
/// A typed command longer than the terminal is wide occupies several rows
/// and unwraps into one logical line, so the answer has to be per run, not
/// per row — otherwise `--output-only` would drop the first half of an
/// echoed command and keep the tail, which is worse than not filtering.
///
/// Rather than re-deriving where the runs fall (a second unwrapper, which
/// would drift from the real one), this unwraps a **shadow stream**: the
/// same row counts and the same wrap bits, with each row replaced by a
/// one-character tag. The joined tags land one-for-one on the joined text,
/// by construction of the very function that joins it.
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

/// The lines a [`Condition`] is matched against: rows as **written**,
/// narrowed by `scope`.
///
/// The order is deliberate. The whole returned stream is unwrapped first,
/// because a run can straddle the history/viewport seam and windowing before
/// joining would cut a logical line in half — reintroducing the straddling
/// miss this function exists to prevent. The row window then applies to
/// logical lines, and `output_only` filters last, so the window means "the
/// last N lines of the pane" and not "the last N lines that survived the
/// filter".
#[must_use]
pub fn match_lines(screen: &ScreenState, scope: &MatchScope) -> Vec<String> {
    let mut rows = screen.unwrapped_rows();
    let trimmed = if scope.tail.is_some() {
        // A grid is always full height, so the rows below the cursor are
        // blank padding, not content. Counting them would make `--tail 5`
        // against a pane whose prompt sits near the top match nothing at
        // all — the flag would be broken rather than merely surprising.
        // Only a caller who asked for a window pays this, so the default
        // projection (and with it `--idle`'s settle comparison) is
        // byte-identical to reading the grid.
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
    // The mask is built over the untrimmed stream. Blank padding came off
    // the end and the window came off the front, so the surviving lines
    // start `dropped` in — element-for-element aligned from there.
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

/// Tracks whether the matched lines have held still long enough to count as
/// "settled" for [`Condition::Idle`]. Pulled out of the poll loop so the
/// dwell logic is unit-testable with an injected clock.
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

    /// Record the latest `lines` observed at `now`; return `true` once they
    /// have been unchanged for at least `dwell`. The first observation, and
    /// any observation that differs from the previous one, resets the dwell
    /// clock and returns `false`.
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

/// Evaluate `condition` against the already-projected `lines`.
///
/// Split out of the poll loop so the predicate the loop runs is the exact
/// predicate the tests run: a test that re-spells the match arm proves
/// nothing about the loop.
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

/// The result of a poll loop: why it stopped, the last screen observed,
/// and how many reads it took (useful for `--json` and diagnostics).
#[derive(Debug, Clone)]
pub struct WaitResult {
    /// Why polling stopped.
    pub outcome: WaitOutcome,
    /// The most recent screen read.
    pub screen: ScreenState,
    /// Number of screen reads performed.
    pub polls: u32,
}

/// Poll `terminal_id` until `condition` holds or `timeout` elapses.
///
/// Reads the screen via the side-effect-free `GET_SCREEN`. The `interval`
/// argument is the *requested ceiling*; the effective poll gap is derived
/// per condition by [`effective_interval`], so [`Condition::Idle`] with a
/// small dwell polls faster (down to [`MIN_POLL_INTERVAL`]) and resolves at
/// sub-dwell granularity rather than rounding up to a full interval.
///
/// Returns [`WaitOutcome::TimedOut`] (not an error) when the deadline passes
/// first, so callers map the two outcomes to their own exit codes.
/// `timeout = None` waits indefinitely.
///
/// # Errors
///
/// Propagates [`AttachError`] from the underlying screen read (no server,
/// transport failure, unknown terminal).
pub async fn poll_until(
    socket: &Path,
    terminal_id: TerminalId,
    condition: &Condition,
    timeout: Option<Duration>,
    interval: Duration,
) -> Result<WaitResult, AttachError> {
    poll_until_scoped(
        socket,
        terminal_id,
        condition,
        timeout,
        interval,
        &MatchScope::default(),
    )
    .await
}

/// [`poll_until`] with an explicit [`MatchScope`].
///
/// The scope decides what each poll reads (history rows, per-cell marks) and
/// what the condition is matched against; [`poll_until`] is this with the
/// default scope — viewport only, every line — which is the cheapest read
/// and the historical behavior.
///
/// # Errors
///
/// See [`poll_until`].
pub async fn poll_until_scoped(
    socket: &Path,
    terminal_id: TerminalId,
    condition: &Condition,
    timeout: Option<Duration>,
    interval: Duration,
    scope: &MatchScope,
) -> Result<WaitResult, AttachError> {
    let start = Instant::now();
    let mut polls: u32 = 0;
    let mut idle = IdleTracker::new(start);
    let interval = effective_interval(condition, interval);

    loop {
        let screen = get_screen_scrollback(
            socket,
            terminal_id.clone(),
            scope.history_request(),
            scope.wants_cells(),
        )
        .await?;
        polls = polls.saturating_add(1);
        let lines = match_lines(&screen, scope);
        let met = condition_met(condition, &lines, &mut idle, Instant::now());
        if met {
            return Ok(WaitResult {
                outcome: WaitOutcome::Met,
                screen,
                polls,
            });
        }
        if timeout.is_some_and(|t| start.elapsed() >= t) {
            return Ok(WaitResult {
                outcome: WaitOutcome::TimedOut,
                screen,
                polls,
            });
        }
        tokio::time::sleep(interval).await;
    }
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
            cursor: None,
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            scrollback: Vec::new(),
            cells: None,
            ..ScreenState::default()
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

    /// A cell carrying an OSC-133 mark on `row`. Only `row` and `semantic`
    /// matter to the filter; the rest is the sparse projection's baseline.
    fn mark(row: u16, semantic: SemanticContent) -> CellInfo {
        CellInfo {
            col: 0,
            row,
            semantic: Some(semantic),
            style: CellStyle::default(),
        }
    }

    #[test]
    fn contains_matches_any_line() {
        let s = screen(&["building", "step 2", "all DONE here"]);
        assert!(met(&contains("DONE"), &s, &MatchScope::default()));
        assert!(!met(&contains("MISSING"), &s, &MatchScope::default()));
    }

    /// The bug this module's projection exists to fix: a needle that falls
    /// across a terminal-edge wrap is in NO painted row, so the old
    /// `screen.lines` match ran to timeout and the operator saw a hang.
    #[test]
    fn contains_matches_across_a_soft_wrap() {
        let s = ScreenState {
            soft_wrap: Some(SoftWrap {
                lines: vec![0],
                scrollback: Vec::new(),
            }),
            ..screen(&["cargo test: 41 passed, 1 fai", "led, 0 skipped"])
        };
        assert!(
            !s.lines.iter().any(|l| l.contains("1 failed")),
            "the needle must genuinely straddle the wrap, or this proves nothing"
        );
        assert!(met(&contains("1 failed"), &s, &MatchScope::default()));
    }

    /// A run straddling the history/viewport seam joins too — which is why
    /// the whole stream is unwrapped before the window is applied.
    #[test]
    fn contains_matches_across_the_history_seam() {
        let s = ScreenState {
            scrollback: lines(&["quiet", "BUILD SUCC"]),
            soft_wrap: Some(SoftWrap {
                lines: Vec::new(),
                scrollback: vec![1],
            }),
            ..screen(&["ESSFUL in 4s"])
        };
        assert!(met(
            &contains("BUILD SUCCESSFUL"),
            &s,
            &MatchScope {
                tail: Some(0),
                ..MatchScope::default()
            }
        ));
    }

    /// With no wrap bits (an older server) rows come back verbatim: the
    /// match degrades to the historical behavior rather than guessing.
    #[test]
    fn without_wrap_info_rows_are_matched_verbatim() {
        let s = screen(&["all DONE here"]);
        assert!(!s.has_soft_wrap_info());
        assert!(met(&contains("DONE"), &s, &MatchScope::default()));
        assert!(!met(
            &contains("here and there"),
            &s,
            &MatchScope::default()
        ));
    }

    #[test]
    fn regex_matches_one_logical_line_at_a_time() {
        let s = screen(&["running 12 tests", "test result: ok. 12 passed"]);
        assert!(met(
            &regex(r"^test result: ok\. \d+ passed$"),
            &s,
            &MatchScope::default()
        ));
        // A pattern spanning two lines must NOT match: lines are matched
        // individually, never as one joined blob.
        assert!(!met(
            &regex(r"tests\s*test result"),
            &s,
            &MatchScope::default()
        ));
    }

    #[test]
    fn regex_matches_across_a_soft_wrap() {
        let s = ScreenState {
            soft_wrap: Some(SoftWrap {
                lines: vec![0],
                scrollback: Vec::new(),
            }),
            ..screen(&["test result: FAILED. 3 pas", "sed; 2 failed"])
        };
        assert!(met(
            &regex(r"FAILED\. \d+ passed; \d+ failed"),
            &s,
            &MatchScope::default()
        ));
    }

    /// An invalid pattern is rejected at parse time. The CLI hangs this off
    /// clap's value parser, so the process exits 2 before it ever connects.
    #[test]
    fn an_invalid_regex_fails_to_compile() {
        let err = "(unclosed".parse::<MatchRegex>().unwrap_err();
        assert!(
            err.contains("unclosed") || err.contains("regex parse error"),
            "the usage error should carry the regex diagnostic, got {err:?}"
        );
        assert_eq!(
            r"\d+ failed"
                .parse::<MatchRegex>()
                .expect("a valid pattern compiles")
                .as_str(),
            r"\d+ failed"
        );
    }

    /// The command-echo footgun: a caller waiting for text that also appears
    /// in the command it just typed matches its own echo instantly.
    #[test]
    fn output_only_skips_the_echoed_command() {
        let s = ScreenState {
            cells: Some(vec![
                mark(0, SemanticContent::Prompt),
                mark(0, SemanticContent::Input),
            ]),
            ..screen(&["$ cargo test | grep 'test result: ok'", "running 12 tests"])
        };
        let default = MatchScope::default();
        let output_only = MatchScope {
            output_only: true,
            ..MatchScope::default()
        };
        assert!(
            met(&contains("test result: ok"), &s, &default),
            "without the flag the echoed command still matches (the footgun)"
        );
        assert!(!met(&contains("test result: ok"), &s, &output_only));
        assert!(!met(&regex(r"test result: (ok|FAILED)"), &s, &output_only));
        // Output on an unmarked row is untouched by the filter.
        assert!(met(&contains("running 12 tests"), &s, &output_only));
    }

    /// A typed command longer than the terminal is wide occupies several
    /// rows: the WHOLE logical line is input, not just its first row.
    #[test]
    fn output_only_skips_a_wrapped_echoed_command() {
        let s = ScreenState {
            soft_wrap: Some(SoftWrap {
                lines: vec![0],
                scrollback: Vec::new(),
            }),
            cells: Some(vec![mark(0, SemanticContent::Input)]),
            ..screen(&["$ cargo test 2>&1 | tee log # test re", "sult: ok"])
        };
        assert!(!met(
            &contains("test result: ok"),
            &s,
            &MatchScope {
                output_only: true,
                ..MatchScope::default()
            }
        ));
    }

    /// Prompt-marked text is not input: a caller may legitimately wait for
    /// the prompt string itself to come back.
    #[test]
    fn output_only_keeps_prompt_marked_rows() {
        let s = ScreenState {
            cells: Some(vec![mark(1, SemanticContent::Prompt)]),
            ..screen(&["done", "user@host ~/src $"])
        };
        assert!(met(
            &contains("user@host"),
            &s,
            &MatchScope {
                output_only: true,
                ..MatchScope::default()
            }
        ));
    }

    /// No OSC-133 integration means nothing to filter on. The filter fails
    /// OPEN — a wait that would otherwise succeed must not hang — and
    /// `has_semantic_marks` is how the caller says so out loud.
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
        assert!(met(
            &contains("cargo test"),
            &s,
            &MatchScope {
                output_only: true,
                ..MatchScope::default()
            }
        ));
    }

    #[test]
    fn has_semantic_marks_sees_a_marked_screen() {
        let s = ScreenState {
            cells: Some(vec![mark(0, SemanticContent::Input)]),
            ..screen(&["$ cargo test"])
        };
        assert!(has_semantic_marks(&s));
    }

    #[test]
    fn tail_scopes_the_search_to_the_last_n_logical_lines() {
        let s = ScreenState {
            scrollback: lines(&["MARKER far above", "noise"]),
            ..screen(&["still noise", "tail line"])
        };
        let all = MatchScope {
            tail: Some(0),
            ..MatchScope::default()
        };
        let last_two = MatchScope {
            tail: Some(2),
            ..MatchScope::default()
        };
        assert!(met(&contains("MARKER"), &s, &all));
        assert!(!met(&contains("MARKER"), &s, &last_two));
        assert!(met(&contains("tail line"), &s, &last_two));
        assert_eq!(
            match_lines(&s, &last_two),
            lines(&["still noise", "tail line"])
        );
    }

    /// The window counts LOGICAL lines, so it can never bisect a wrapped
    /// run and reintroduce the straddling miss.
    /// A terminal grid is always full height. If the blank rows under the
    /// cursor counted, `--tail 2` against a pane whose prompt sits near the
    /// top would match nothing, ever.
    #[test]
    fn tail_does_not_count_the_blank_rows_under_the_cursor() {
        let s = screen(&["MARKER here", "$", "", "", ""]);
        let last_two = MatchScope {
            tail: Some(2),
            ..MatchScope::default()
        };
        assert_eq!(match_lines(&s, &last_two), lines(&["MARKER here", "$"]));
        assert!(met(&contains("MARKER"), &s, &last_two));
        // Without a window nothing is trimmed: the default projection stays
        // the grid, so `--idle`'s byte-exact settle comparison is unchanged.
        assert_eq!(match_lines(&s, &MatchScope::default()).len(), s.lines.len());
    }

    /// Trimming happens off the end and the window off the front; the input
    /// mask has to survive both at once.
    #[test]
    fn tail_trimming_keeps_the_input_mask_aligned() {
        let s = ScreenState {
            cells: Some(vec![mark(2, SemanticContent::Input)]),
            ..screen(&["oldest", "output line", "$ echo MARKER", "", ""])
        };
        let scope = MatchScope {
            tail: Some(2),
            output_only: true,
        };
        assert_eq!(match_lines(&s, &scope), lines(&["output line"]));
    }

    #[test]
    fn tail_counts_logical_lines_not_painted_rows() {
        let s = ScreenState {
            soft_wrap: Some(SoftWrap {
                lines: vec![0],
                scrollback: Vec::new(),
            }),
            ..screen(&["BUILD SUCC", "ESSFUL", "next"])
        };
        let last_two = MatchScope {
            tail: Some(2),
            ..MatchScope::default()
        };
        assert_eq!(
            match_lines(&s, &last_two),
            lines(&["BUILD SUCCESSFUL", "next"])
        );
        assert!(met(&contains("BUILD SUCCESSFUL"), &s, &last_two));
    }

    /// The window is applied before the input filter, so `--tail N` means
    /// "the last N lines of the pane" and not "the last N that survived".
    #[test]
    fn tail_and_output_only_compose_on_the_same_window() {
        let s = ScreenState {
            cells: Some(vec![mark(1, SemanticContent::Input)]),
            ..screen(&["older output", "$ echo MARKER", "fresh output"])
        };
        let scope = MatchScope {
            tail: Some(2),
            output_only: true,
        };
        assert_eq!(match_lines(&s, &scope), lines(&["fresh output"]));
        assert!(!met(&contains("MARKER"), &s, &scope));
        assert!(!met(&contains("older output"), &s, &scope));
    }

    #[test]
    fn scope_drives_what_each_poll_reads() {
        let default = MatchScope::default();
        assert_eq!(default.history_request(), None);
        assert!(!default.wants_cells());
        let scoped = MatchScope {
            tail: Some(200),
            output_only: true,
        };
        assert_eq!(scoped.history_request(), Some(200));
        assert!(scoped.wants_cells());
    }

    #[test]
    fn idle_observes_the_scoped_projection() {
        // A change confined to a row outside the window is not activity.
        let before = ScreenState {
            scrollback: lines(&["spinner |"]),
            ..screen(&["settled"])
        };
        let after = ScreenState {
            scrollback: lines(&["spinner /"]),
            ..screen(&["settled"])
        };
        let scope = MatchScope {
            tail: Some(1),
            ..MatchScope::default()
        };
        assert_eq!(match_lines(&before, &scope), match_lines(&after, &scope));
    }

    #[test]
    fn effective_interval_regex_uses_requested() {
        assert_eq!(
            effective_interval(&regex("x"), DEFAULT_POLL_INTERVAL),
            DEFAULT_POLL_INTERVAL
        );
    }

    #[test]
    fn idle_first_observation_never_settles() {
        let t0 = Instant::now();
        let mut idle = IdleTracker::new(t0);
        // Even with a generous gap, the first read just records a baseline.
        assert!(!idle.observe(
            &lines(&["a"]),
            t0 + Duration::from_secs(10),
            Duration::from_millis(50)
        ));
    }

    #[test]
    fn idle_settles_after_dwell_without_change() {
        let t0 = Instant::now();
        let mut idle = IdleTracker::new(t0);
        let dwell = Duration::from_millis(100);
        assert!(!idle.observe(&lines(&["a"]), t0, dwell)); // baseline
        // Unchanged but not yet dwelled.
        assert!(!idle.observe(&lines(&["a"]), t0 + Duration::from_millis(60), dwell));
        // Unchanged and past the dwell.
        assert!(idle.observe(&lines(&["a"]), t0 + Duration::from_millis(160), dwell));
    }

    #[test]
    fn effective_interval_contains_uses_requested() {
        let cond = Condition::Contains("x".to_owned());
        assert_eq!(
            effective_interval(&cond, DEFAULT_POLL_INTERVAL),
            DEFAULT_POLL_INTERVAL
        );
    }

    #[test]
    fn effective_interval_small_dwell_clamps_to_min() {
        // dwell/4 = 10ms, below the 25ms floor.
        let cond = Condition::Idle(Duration::from_millis(40));
        assert_eq!(
            effective_interval(&cond, DEFAULT_POLL_INTERVAL),
            MIN_POLL_INTERVAL
        );
    }

    #[test]
    fn effective_interval_mid_dwell_tracks_quarter() {
        // dwell/4 = 100ms, inside [25ms, 150ms].
        let cond = Condition::Idle(Duration::from_millis(400));
        assert_eq!(
            effective_interval(&cond, DEFAULT_POLL_INTERVAL),
            Duration::from_millis(100)
        );
    }

    #[test]
    fn effective_interval_large_dwell_clamps_to_requested() {
        // dwell/4 = 2.5s, capped at the requested 150ms ceiling.
        let cond = Condition::Idle(Duration::from_secs(10));
        assert_eq!(
            effective_interval(&cond, DEFAULT_POLL_INTERVAL),
            DEFAULT_POLL_INTERVAL
        );
    }

    #[test]
    fn effective_interval_handles_requested_below_floor() {
        // A pathological requested ceiling below MIN_POLL_INTERVAL must not
        // panic; the cap wins.
        let cond = Condition::Idle(Duration::from_millis(400));
        let tiny = Duration::from_millis(10);
        assert_eq!(effective_interval(&cond, tiny), tiny);
    }

    #[test]
    fn idle_resets_dwell_on_any_change() {
        let t0 = Instant::now();
        let mut idle = IdleTracker::new(t0);
        let dwell = Duration::from_millis(100);
        assert!(!idle.observe(&lines(&["a"]), t0, dwell));
        // Content changes well after the dwell would have elapsed — must NOT
        // settle, and must restart the clock from this moment.
        assert!(!idle.observe(&lines(&["b"]), t0 + Duration::from_millis(500), dwell));
        // 60ms after the change: still not settled.
        assert!(!idle.observe(&lines(&["b"]), t0 + Duration::from_millis(560), dwell));
        // 110ms after the change: settled.
        assert!(idle.observe(&lines(&["b"]), t0 + Duration::from_millis(610), dwell));
    }
}
