use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::{AttachError, run_headless_rendered};
use phux_client::snapshot::{
    ROW_WINDOW_ALL, RenderedFrame, ScreenState, SoftWrap, TRUNCATED_ROW_WINDOW, row_window,
};
use phux_protocol::wire::frame::AttachTarget;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

/// Options for the structured pane read (ADR-0022 §2, ADR-0077).
///
/// Bundled so `run_snapshot` keeps a readable arg list as the read surface
/// grows orthogonal modifiers rather than a named source vocabulary
/// (ADR-0077 §1).
pub(crate) struct ReadOpts {
    /// History window: `None` viewport only, `Some(0)` all retained
    /// history, `Some(n)` the most-recent `n` rows (`phux-o1v`).
    pub scrollback: Option<u32>,
    /// Request the sparse per-cell semantic/style projection (`phux-8yl`).
    pub cells: bool,
    /// Row-count window over the rendered rows: `None` off, `Some(0)` all,
    /// `Some(n)` the most recent `n` (ADR-0077 §3).
    pub tail: Option<u32>,
    /// Join soft-wrapped rows into logical lines (ADR-0077 §2).
    pub unwrap: bool,
}

/// Options for the composited `--rendered` view (`phux-l5xa`). Bundled so the
/// `run_snapshot` arg list stays readable.
pub(crate) struct RenderedOpts {
    /// Emit the client's composited multi-pane frame instead of a per-pane
    /// grid read.
    pub rendered: bool,
    /// Composite viewport width (no TTY to measure).
    pub cols: u16,
    /// Composite viewport height.
    pub rows: u16,
}

/// `phux snapshot [TARGET]` — read a pane as structured data (ADR-0022).
///
/// Resolves `TARGET` (a selector; default: the focused session) to a pane
/// client-side, then issues the side-effect-free `GET_SCREEN` command —
/// the server walks its own grid, so this neither attaches nor resizes the
/// pane (unlike the old attach-walk path; ADR-0022 §5, `phux-oki`). Emits
/// JSON or a boxed text view, then exits.
///
/// `--rendered` ([`RenderedOpts`]) instead drives the headless client render
/// path and emits the assembled multi-pane composite (`phux-l5xa`); that
/// branch ATTACHES rather than reading side-effect-free.
///
/// `--tail` / `--unwrap` ([`ReadOpts`], ADR-0077) are **client-side
/// projections** of the same reply: there is no new wire field, and the
/// server's own read stays exactly the side-effect-free `GET_SCREEN` it
/// already was.
pub(crate) fn run_snapshot(
    session: Option<&str>,
    json: bool,
    read: &ReadOpts,
    rendered: &RenderedOpts,
    socket: Option<PathBuf>,
) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    if rendered.rendered {
        return run_rendered(session, json, rendered, &socket_path, &rt);
    }

    let selector = match parse_selector(session) {
        Ok(sel) => sel,
        Err(code) => return code,
    };

    let request_scrollback = history_request(read);
    let cells = read.cells;
    let unwrap = read.unwrap;
    let tail = read.tail;

    rt.block_on(async move {
        let terminal_id = match resolve_target(&socket_path, &selector, "snapshot", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };

        // Read the screen — side-effect-free, safe to poll. `scrollback`
        // maps straight onto the wire request: None/Some(0=all)/Some(n);
        // `cells` requests the per-cell semantic/style projection.
        let screen = match phux_client::snapshot::get_screen_scrollback(
            &socket_path,
            terminal_id,
            request_scrollback,
            cells,
        )
        .await
        {
            Ok(screen) => screen,
            Err(err @ AttachError::Io(_)) => {
                return json_err::report_no_server(json, &err, &socket_path, "snapshot");
            }
            Err(err) => {
                eprintln!("phux: snapshot failed: {err}");
                return ExitCode::FAILURE;
            }
        };
        let screen = project(screen, unwrap, tail);

        if json {
            match serde_json::to_string_pretty(&screen) {
                Ok(s) => {
                    outln!("{s}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("phux: failed to serialize snapshot: {err}");
                    ExitCode::FAILURE
                }
            }
        } else if unwrap {
            print_screen_rows(&screen);
            ExitCode::SUCCESS
        } else {
            print_screen_box(&screen);
            ExitCode::SUCCESS
        }
    })
}

/// The history window to ask the server for.
///
/// `--scrollback` wins when both are given: it is the explicit statement
/// about history, and `--tail` then clamps whatever came back. Otherwise
/// `--tail N` asks for `N` history rows — a superset of what the window
/// keeps, since the viewport also counts toward `N` — and `--tail 0` asks
/// for all retained history, which [`project`] then clamps to
/// `ROW_WINDOW_MAX`.
fn history_request(read: &ReadOpts) -> Option<u32> {
    read.scrollback.or(read.tail)
}

/// Apply the client-side ADR-0077 projections to a reply.
///
/// Order is deliberate: unwrapping first, then the row window. Unwrapping
/// changes how many rows there are, so a window applied before it would
/// count painted rows and report a different number than it returned.
fn project(mut screen: ScreenState, unwrap: bool, tail: Option<u32>) -> ScreenState {
    if unwrap {
        let (history, viewport) = screen.unwrapped_split();
        screen.scrollback = history;
        screen.lines = viewport;
        // Nothing in the returned projection continues onto the next row
        // any more. Keep it `Some` — present-and-empty is "reported, none",
        // and dropping to `None` would read as "server said nothing".
        screen.soft_wrap = Some(SoftWrap::default());
    }

    let Some(want) = tail else {
        return screen;
    };

    // The window counts rendered rows, history and viewport together, but a
    // `ScreenState` describes a grid and a grid is never returned in part:
    // `rows`, `cursor`, and `cells` are all grid coordinates. So the
    // viewport is a floor and only history is clipped. A window narrower
    // than the viewport therefore returns more rows than asked for, never
    // fewer, and truncation still reports what it dropped.
    let ceiling = usize::try_from(phux_client::snapshot::ROW_WINDOW_MAX).unwrap_or(usize::MAX);
    let want = if want == ROW_WINDOW_ALL {
        ceiling
    } else {
        usize::try_from(want).unwrap_or(usize::MAX).min(ceiling)
    };
    let keep = want.saturating_sub(screen.lines.len());
    let before = screen.scrollback.len();
    let clipped = if keep == 0 {
        screen.scrollback.clear();
        before > 0
    } else {
        let (kept, clipped) = row_window(
            std::mem::take(&mut screen.scrollback),
            u32::try_from(keep).unwrap_or(u32::MAX),
        );
        screen.scrollback = kept;
        clipped
    };

    // History indices in `soft_wrap.scrollback` are relative to the
    // returned array, so dropping D rows off the front shifts them by D. A
    // wrap that pointed into a dropped row simply goes away: the surviving
    // first row may be a continuation of something no longer present, which
    // is exactly what `truncated` is telling the caller.
    let dropped = u32::try_from(before - screen.scrollback.len()).unwrap_or(u32::MAX);
    if dropped > 0
        && let Some(wrap) = screen.soft_wrap.as_mut()
    {
        wrap.scrollback = wrap
            .scrollback
            .iter()
            .filter_map(|index| index.checked_sub(dropped))
            .collect();
    }

    if clipped {
        screen.truncated = true;
        screen.truncated_reason = Some(TRUNCATED_ROW_WINDOW.to_owned());
    }
    screen
}

/// `--rendered`: attach headless, compose the client's multi-pane frame, and
/// emit it as JSON ([`RenderedFrame`]) or a boxed text view (`phux-l5xa`).
fn run_rendered(
    session: Option<&str>,
    json: bool,
    opts: &RenderedOpts,
    socket_path: &std::path::Path,
    rt: &tokio::runtime::Runtime,
) -> ExitCode {
    let target = session.map_or(AttachTarget::Last, |s| AttachTarget::ByName(s.to_owned()));
    rt.block_on(async move {
        let frame = match run_headless_rendered(socket_path, target, opts.cols, opts.rows).await {
            Ok(frame) => frame,
            Err(err @ AttachError::Io(_)) => {
                return json_err::report_no_server(json, &err, socket_path, "snapshot");
            }
            Err(err) => {
                eprintln!("phux: rendered snapshot failed: {err}");
                return ExitCode::FAILURE;
            }
        };
        if json {
            match serde_json::to_string_pretty(&frame) {
                Ok(s) => {
                    outln!("{s}");
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("phux: failed to serialize rendered frame: {err}");
                    ExitCode::FAILURE
                }
            }
        } else {
            print_rendered_box(&frame);
            ExitCode::SUCCESS
        }
    })
}

/// Boxed text view of a composited [`RenderedFrame`].
///
/// Each row's graphemes are joined left-to-right. A wide glyph's empty tail
/// (`""`) contributes nothing and its base glyph occupies two display
/// columns, so a joined row's display width already equals `cols` — no
/// padding needed. The composited cursor is reported below the box.
pub(crate) fn print_rendered_box(frame: &RenderedFrame) {
    let bar = "─".repeat(usize::from(frame.cols));
    outln!("┌{bar}┐");
    for row in 0..frame.rows {
        let mut line = String::new();
        for col in 0..frame.cols {
            if let Some(cell) = frame.cell(row, col) {
                line.push_str(&cell.grapheme);
            }
        }
        outln!("│{line}│");
    }
    outln!("└{bar}┘");
    let cursor = frame.cursor.as_ref().map_or_else(
        || "none".to_owned(),
        |c| {
            let vis = if c.visible { "visible" } else { "hidden" };
            format!("{},{} {vis}", c.x, c.y)
        },
    );
    outln!("{}x{} cursor={cursor}", frame.cols, frame.rows);
}

/// Human-readable boxed rendering of a captured screen (no tmux, no TTY).
///
/// Scrollback history, when present (`--scrollback`), is printed above the
/// viewport, dimmed and separated by a `╌` rule so it reads as "older
/// content above the live screen" (`phux-o1v`).
pub(crate) fn print_screen_box(screen: &ScreenState) {
    let bar = "─".repeat(usize::from(screen.cols));
    let pad_line = |line: &str| {
        let pad = usize::from(screen.cols).saturating_sub(line.chars().count());
        " ".repeat(pad)
    };
    if screen.scrollback.is_empty() {
        outln!("┌{bar}┐");
    } else {
        let rule = "╌".repeat(usize::from(screen.cols));
        outln!("┌{rule}┐");
        for line in &screen.scrollback {
            outln!("┊{line}{}┊", pad_line(line));
        }
        outln!("├{bar}┤");
    }
    for line in &screen.lines {
        outln!("│{line}{}│", pad_line(line));
    }
    outln!("└{bar}┘");
    outln!("{}", footer(screen));
}

/// Plain row-per-line rendering, used by `--unwrap`.
///
/// The box view pads every row to `cols`, which a joined logical line
/// exceeds by construction — so unwrapped output drops the box rather than
/// draw a broken one. History rows come first, then the viewport, matching
/// the JSON arrays.
pub(crate) fn print_screen_rows(screen: &ScreenState) {
    for line in screen.rendered_rows() {
        outln!("{line}");
    }
    outln!("{}", footer(screen));
}

/// The trailing status line shared by both text views.
///
/// `truncated` is called out in words: a caller reading a clipped window
/// should not have to notice a missing row to learn that rows are missing.
fn footer(screen: &ScreenState) -> String {
    let cursor = screen
        .cursor
        .as_ref()
        .map_or_else(|| "none".to_owned(), |c| format!("{},{}", c.x, c.y));
    let mut out = format!(
        "pane={} {}x{} cursor={cursor}",
        screen.pane, screen.cols, screen.rows
    );
    if let Some(title) = screen.title.as_deref() {
        let _ = write!(out, " title={title:?}");
    }
    if screen.truncated {
        let reason = screen.truncated_reason.as_deref().unwrap_or("unknown");
        let _ = write!(out, " truncated={reason}");
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn opts(scrollback: Option<u32>, tail: Option<u32>, unwrap: bool) -> ReadOpts {
        ReadOpts {
            scrollback,
            cells: false,
            tail,
            unwrap,
        }
    }

    fn screen(scrollback: &[&str], lines: &[&str], wrapped_lines: &[u32]) -> ScreenState {
        ScreenState {
            pane: 1,
            cols: 10,
            rows: u16::try_from(lines.len()).unwrap_or(0),
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            scrollback: scrollback.iter().map(|s| (*s).to_owned()).collect(),
            soft_wrap: Some(SoftWrap {
                lines: wrapped_lines.to_vec(),
                scrollback: Vec::new(),
            }),
            ..ScreenState::default()
        }
    }

    /// `--tail N` asks the server for `N` history rows; an explicit
    /// `--scrollback` wins over it.
    #[test]
    fn history_request_prefers_an_explicit_scrollback() {
        assert_eq!(history_request(&opts(None, None, false)), None);
        assert_eq!(history_request(&opts(None, Some(80), false)), Some(80));
        assert_eq!(history_request(&opts(None, Some(0), false)), Some(0));
        assert_eq!(history_request(&opts(Some(5), Some(80), false)), Some(5));
    }

    /// `--unwrap` joins wrapped rows and reports that nothing in the
    /// returned projection continues — `Some(empty)`, never `None`.
    #[test]
    fn unwrap_joins_rows_and_keeps_reporting_wrap_info() {
        let out = project(screen(&[], &["the quick", "brown fox"], &[0]), true, None);
        assert_eq!(out.lines, vec!["the quickbrown fox".to_owned()]);
        assert_eq!(out.soft_wrap, Some(SoftWrap::default()));
        assert!(
            out.has_soft_wrap_info(),
            "an unwrapped projection still reported wrap info",
        );
        assert!(!out.truncated);
    }

    /// The row window counts the viewport and clips only history, and it
    /// says so.
    #[test]
    fn tail_clips_history_and_reports_truncation() {
        let base = screen(&["h1", "h2", "h3"], &["v1", "v2"], &[]);

        let out = project(base.clone(), false, Some(4));
        assert_eq!(
            out.scrollback,
            vec!["h2".to_owned(), "h3".to_owned()],
            "a window of 4 is 2 viewport rows + the 2 most-recent history rows",
        );
        assert_eq!(out.lines, vec!["v1".to_owned(), "v2".to_owned()]);
        assert!(out.truncated);
        assert_eq!(out.truncated_reason.as_deref(), Some(TRUNCATED_ROW_WINDOW));

        let out = project(base.clone(), false, Some(5));
        assert_eq!(out.scrollback.len(), 3, "a window that fits clips nothing");
        assert!(!out.truncated);
        assert!(out.truncated_reason.is_none());

        let out = project(base.clone(), false, Some(ROW_WINDOW_ALL));
        assert_eq!(out.scrollback.len(), 3);
        assert!(!out.truncated);

        // A window narrower than the viewport: the grid is a floor, so the
        // viewport survives whole and only history goes.
        let out = project(base, false, Some(1));
        assert!(out.scrollback.is_empty());
        assert_eq!(out.lines.len(), 2, "the viewport is never returned in part");
        assert!(out.truncated);
    }

    /// Clipping history shifts the history wrap indices, which are relative
    /// to the returned array.
    #[test]
    fn tail_shifts_history_wrap_indices() {
        let mut base = screen(&["h1", "h2", "h3"], &["v1"], &[]);
        base.soft_wrap = Some(SoftWrap {
            lines: Vec::new(),
            scrollback: vec![0, 2],
        });
        let out = project(base, false, Some(3));
        assert_eq!(out.scrollback, vec!["h2".to_owned(), "h3".to_owned()]);
        let wrap = out.soft_wrap.expect("wrap info survives the window");
        assert_eq!(
            wrap.scrollback,
            vec![1],
            "index 2 became 1; index 0 pointed at a dropped row and went away",
        );
    }

    /// clap's `default_missing_value` must be a string literal, so bare
    /// `--tail`'s count and the `--help` text can drift from the constant
    /// that documents them. Pin all three here.
    #[test]
    fn bare_tail_default_matches_the_documented_constant() {
        assert_eq!(
            phux_client::snapshot::ROW_WINDOW_DEFAULT,
            80,
            "clap spells this as default_missing_value = \"80\" in commands::mod",
        );
        assert_eq!(
            phux_client::snapshot::ROW_WINDOW_MAX,
            10_000,
            "the --tail help text spells this as 10000",
        );
    }

    /// Nothing requested, nothing changed: the projection is the identity.
    #[test]
    fn no_modifiers_leaves_the_reply_untouched() {
        let base = screen(&["h1"], &["v1"], &[0]);
        assert_eq!(project(base.clone(), false, None), base);
    }
}
