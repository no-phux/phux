//! The `Panes` verbs that read a pane: `snapshot`, `wait`, and `watch`.

use crate::commands::{JsonOpt, SnapshotFormat};
use usage::Args;

/// Read a pane's screen as text or JSON
///
/// The agent "floor": read what's on screen as JSON (`--json`) or a
/// boxed text view, without a TTY or tmux. The read is side-effect-free
/// — the server walks its own grid, so this neither attaches nor
/// resizes the pane, and is safe to poll against a pane another client
/// is using.
///
/// TARGET is a selector (see the top-level help); omit it for the
/// most-recently-focused session.
#[derive(Debug, Args)]
#[usage(about = "Read a pane's screen as text or JSON")]
pub(crate) struct SnapshotArgs {
    /// Target selector. Omit for the most-recently-focused session.
    #[usage(value_name = "TARGET")]
    pub(crate) session: Option<String>,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,

    /// Include scrollback history above the viewport.
    /// Bare `--scrollback` requests all retained history; `--scrollback
    /// N` requests the most-recent N rows. History appears in the JSON
    /// `scrollback` field; the boxed view shows it above the viewport.
    #[usage(long, value_name = "N", num_args = 0..=1, default_missing = "0")]
    pub(crate) scrollback: Option<u32>,

    /// Include per-cell OSC-133 semantic marks + styles.
    /// Populates the JSON `cells` array (sparse: only cells with a
    /// non-default style or a semantic mark). No effect on the boxed
    /// view, which is plain text.
    #[usage(long)]
    pub(crate) cells: bool,

    /// Return the last N rendered rows (history above the viewport,
    /// then the viewport). Bare `--tail` returns 80; `--tail 0` returns
    /// all, capped at 10000. The viewport is a floor — a grid is never
    /// returned in part — and `truncated` reports any dropped rows.
    /// With `--format`, this instead bounds how far back the rendered
    /// capture reaches (same wire request as `--scrollback N`); the
    /// server applies the same 10000-row cap regardless.
    // The literals are `phux_core::screen::ROW_WINDOW_DEFAULT` and
    // `ROW_WINDOW_MAX`; clap needs a `&'static str` here, so
    // `commands::snapshot`'s tests pin the two spellings together.
    #[usage(long, value_name = "N", num_args = 0..=1, default_missing = "80")]
    pub(crate) tail: Option<u32>,

    /// Join soft-wrapped rows into logical lines (rows as written, not
    /// as painted). Cannot be combined with `--cells`: cell coordinates
    /// are grid coordinates and do not survive the join. With
    /// `--format`, this instead asks the SERVER's Formatter to join
    /// soft-wrapped rows in the rendered capture.
    #[usage(long, conflicts("--cells"))]
    pub(crate) unwrap: bool,

    /// Emit the CLIENT's composited multi-pane view — the assembled
    /// frame (layout tiling + dividers + status bar) as the human's glass
    /// shows it — as dense structured cells. Unlike the
    /// default side-effect-free read this ATTACHES (drives the headless
    /// client render path), though without a window-size vote, so the
    /// session's panes keep their size. Mutually exclusive with `--cells` /
    /// `--scrollback` / `--tail` / `--unwrap`; sizes the composite via
    /// `--cols` / `--rows`.
    #[usage(long, conflicts("--cells", "--scrollback", "--tail", "--unwrap"))]
    pub(crate) rendered: bool,

    /// Render through the SERVER's libghostty-vt Formatter instead of
    /// the lines/cells JSON: `html` for inline-styled markup, `vt` for
    /// re-playable VT escape sequences. Text output writes the capture
    /// verbatim to stdout (HTML as UTF-8, VT as its raw decoded byte
    /// stream); `--json` instead emits the whole `ScreenState`
    /// document with the `rendered` field populated. Mutually
    /// exclusive with `--cells` and `--rendered`.
    #[usage(
        long,
        value_enum,
        value_name = "FMT",
        conflicts("--cells", "--rendered")
    )]
    pub(crate) format: Option<SnapshotFormat>,

    /// Composited viewport width for `--rendered` (no TTY to measure).
    #[usage(long, value_name = "COLS", default = "80", default_value_t = 80)]
    pub(crate) cols: u16,

    /// Composited viewport height for `--rendered`.
    #[usage(long, value_name = "ROWS", default = "24", default_value_t = 24)]
    pub(crate) rows: u16,
}

/// Block until a pane meets a condition.
#[derive(Debug, Args)]
#[usage(
    about = "Block until a pane meets a condition",
    long_about = "Block until a pane meets a condition.\n\n\
            Polls the side-effect-free screen read — the poll \
            floor of the event surface: always works, no shell integration. \
            Exits 0 when the condition is met, and 124 when `--timeout` expires \
            first. The timeout is one budget for the whole wait — connecting, \
            target resolution, and every screen read — so a server that stops \
            answering still ends the wait on time. The first read always gets \
            at least 2 seconds, so `--timeout 0` checks the condition once. \
            TARGET is a selector (see the \
            top-level help); omit it for the most-recently-focused session.\n\n\
            Matching is against the lines as WRITTEN: rows the terminal \
            soft-wrapped at its right edge are joined first, so text that \
            straddles a wrap is found rather than silently never matching.\n\n\
            Flags may come before or after TARGET; a bare `--tail` reads the \
            next word as N, so spell N out when TARGET follows it.\n\n\
            Examples:\n  \
            phux wait --until \"BUILD SUCCESSFUL\" build\n  \
            phux wait --regex \"test result: (ok|FAILED)\" --output-only build\n  \
            phux wait --idle 750 repl"
)]
pub(crate) struct WaitArgs {
    /// Target selector. Omit for the most-recently-focused session.
    #[usage(value_name = "TARGET")]
    pub(crate) session: Option<String>,

    /// Succeed once any line contains this substring. NOTE: this matches
    /// ANY line, including the shell's echo of a command you just typed
    /// — match on text that appears only in OUTPUT, or pass
    /// `--output-only`.
    #[usage(long, value_name = "TEXT", conflicts("--regex"))]
    pub(crate) until: Option<String>,

    /// Succeed once any line matches this Rust regular expression. One
    /// line at a time, so `^` and `$` anchor to a line you can see. An
    /// invalid pattern is a usage error (exit 2) reported before the
    /// wait starts, never a wait that quietly never matches.
    #[usage(long, value_name = "PATTERN")]
    pub(crate) regex: Option<phux_client::wait::MatchRegex>,

    /// Match only within the last N lines, and read that much history to
    /// do it. Bare `--tail` uses 80; `--tail 0` uses all retained
    /// history, capped at 10000. Without it, only the viewport is read.
    /// N counts logical lines AFTER wrapped rows are joined and ignores
    /// the blank rows under the cursor, and unlike `snapshot --tail` the
    /// viewport is not a floor: `--tail 3` really does mean only the
    /// last three lines with content count — including the prompt block
    /// already back on screen, so leave room for it. A bare `--tail` reads
    /// the next word as N, so spell N out when you also pass TARGET
    /// (`--tail 80 build`, not `--tail build`).
    // The literals are `phux_core::screen::ROW_WINDOW_DEFAULT` and
    // `ROW_WINDOW_MAX`; clap needs a `&'static str` here, so
    // `commands::wait`'s tests pin the two spellings together.
    #[usage(long, value_name = "N", num_args = 0..=1, default_missing = "80")]
    pub(crate) tail: Option<u32>,

    /// Ignore lines the shell marked as your own typed input, so a wait
    /// cannot be satisfied by the echo of the command that started the
    /// work. Needs a shell with OSC-133 integration; with none, nothing
    /// is filtered and phux says so on stderr rather than pretending.
    #[usage(long)]
    pub(crate) output_only: bool,

    /// Succeed once the matched lines hold still for this many
    /// milliseconds (the pane has settled). Default when neither
    /// `--until` nor `--regex` is given. With `--tail N`, only those
    /// lines have to hold still — a spinner further up does not count.
    #[usage(long, value_name = "MS")]
    pub(crate) idle: Option<u64>,

    /// Give up after this many seconds (exit 124), counted from the start
    /// of the command: connecting, resolving TARGET, and every screen
    /// read share the one budget. The first read always gets at least
    /// 2s, so 0 checks the condition once. Default: wait forever.
    #[usage(long, value_name = "SECS")]
    pub(crate) timeout: Option<u64>,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}

/// Stream a pane's live events (the push half of the agent surface).
///
/// Subscribes to the server's event stream and prints one event per
/// line until EOF or Ctrl-C. The
/// subscription neither attaches nor resizes the pane — safe to watch
/// a pane a human or another agent is actively using. This is the
/// latency-cutting accelerator of `phux wait`'s poll floor: events
/// (bell, title change, output dirty/idle, pane spawn/close) arrive as
/// they happen rather than on a poll tick.
///
/// TARGET is a selector (see the top-level help); omit it for the
/// most-recently-focused session. With `--json`, each line is a JSON
/// object (stdout stays pure JSON); otherwise each line is a compact
/// human form.
///
/// `--until EVENT` and `--timeout SECS` bound the stream, so a script
/// need not background the watch and kill it on a sleep.
///
///   phux watch build
///   phux watch --json work:1.0
///   phux watch --until asked --timeout 120 reviewer
#[derive(Debug, Args)]
#[usage(
    about = "Stream a pane's events as they happen",
    long_about = "Stream a pane's live events (the push half of the agent surface).\n\n\
            Subscribes to the server's event stream and prints one event per line. The \
            subscription neither attaches nor resizes the pane — safe to watch a pane a human \
            or another agent is actively using. TARGET is a selector (see the top-level help); \
            omit it for the most-recently-focused session.\n\n\
            With no bounds the stream runs until EOF or Ctrl-C. `--until EVENT` makes it a \
            gate: the first matching event is printed and `watch` exits 0. `--timeout SECS` \
            gives up and exits 124, the same code `phux wait` uses. If the server closes the \
            stream before an `--until` event arrives, that is exit 1 — the event did not \
            happen and can no longer happen.\n\n\
            With `--json` each line is one JSON object and nothing else is written to stdout: \
            no per-line schema_version, and no summary line on timeout.\n\n\
            Examples:\n  \
            phux watch build\n  \
            phux watch --json work:1.0\n  \
            phux watch --until asked --timeout 120 reviewer"
)]
pub(crate) struct WatchArgs {
    /// Target selector. Omit for the most-recently-focused session.
    #[usage(value_name = "TARGET")]
    pub(crate) session: Option<String>,

    /// Exit 0 as soon as an event with this name arrives. Repeatable;
    /// any one of them satisfies the watch. The vocabulary is the one
    /// this stream prints: `agent_state`, `asked`, `bell`,
    /// `command_finished`, `command_started`, `cwd_changed`, `dirty`,
    /// `idle`, `journal_gap`, `pane_closed`, `pane_spawned`,
    /// `source_gap`, `terminal_control`, `title_changed`, `unknown`.
    /// `unknown` also matches `cwd_changed`, `terminal_control`,
    /// `journal_gap`, and `source_gap`, which printed as `unknown` before
    /// they had names, so an existing `--until unknown` keeps working. An
    /// unrecognized name is a usage error (exit 2) reported before the
    /// watch starts, never a watch that quietly never matches.
    #[usage(long, value_name = "EVENT")]
    pub(crate) until: Vec<String>,

    /// Give up after this many seconds (exit 124). Applies with or
    /// without `--until`. Subscribing always gets at least 2 seconds, so
    /// `--timeout 0` subscribes once and still reports the cursor.
    /// Default: stream until EOF or Ctrl-C.
    #[usage(long, value_name = "SECS")]
    pub(crate) timeout: Option<u64>,

    /// Resume from the cursor a previous run printed: events the server
    /// still holds since then are replayed before live ones. A cursor
    /// from another server run is ignored, and said so. The cursor this
    /// run reached is the last line on stderr.
    #[usage(long, value_name = "CURSOR")]
    pub(crate) after: Option<String>,

    #[usage(flatten)]
    pub(crate) json: JsonOpt,
}
