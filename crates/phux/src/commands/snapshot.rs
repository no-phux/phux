use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use base64::Engine as _;
use phux_client::attach::AttachError;
use phux_client::snapshot::{RenderedFrame, ScreenState};
use phux_protocol::wire::frame::AttachTarget;
use phux_server::runtime::default_socket_path;
use phux_tui::attach::run_headless_rendered;

use crate::commands::{SnapshotFormat, cli_runtime, json_err, parse_selector, resolve_target};

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
    /// Render through the server's libghostty-vt Formatter instead of the
    /// lines/cells JSON (`--format html|vt`, D9).
    pub format: Option<SnapshotFormat>,
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
/// projections** of the plain `lines`/`scrollback` reply: there is no new
/// wire field for `--tail`, and the server's own read stays exactly the
/// side-effect-free `GET_SCREEN` it already was. With `--format`
/// (D9), the server omits `lines`/`scrollback` from the reply (review
/// item 2(c)), so those projections have nothing to act on; `--tail N`
/// still reaches the server as `request_scrollback` (bounding what the
/// *rendered* capture covers, same as `--scrollback N`), and `--unwrap`
/// rides `format`'s high bit so the engine's own Formatter joins
/// soft-wrapped capture rows instead.
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
    let format = read.format;

    rt.block_on(async move {
        let terminal_id = match resolve_target(&socket_path, &selector, "snapshot", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };

        // Read the screen — side-effect-free, safe to poll. `scrollback`
        // maps straight onto the wire request: None/Some(0=all)/Some(n);
        // `cells` requests the per-cell semantic/style projection; `format`
        // additionally asks the server to render through libghostty-vt's
        // Formatter (D9), with `--unwrap` riding its high bit.
        let format_byte = format.map_or(0, |f| {
            let mut byte = f.wire_byte();
            if unwrap {
                byte |= phux_client::snapshot::SCREEN_FORMAT_UNWRAP;
            }
            byte
        });
        let screen = match phux_client::snapshot::get_screen_scrollback_format(
            &socket_path,
            terminal_id,
            request_scrollback,
            cells,
            format_byte,
        )
        .await
        {
            Ok(screen) => screen,
            Err(err @ AttachError::Io(_)) => {
                return json_err::report_no_server(json, &err, &socket_path, "snapshot");
            }
            Err(AttachError::FormatUnsupported(message)) => {
                return json_err::emit(
                    json,
                    &json_err::CliError::new(
                        json_err::codes::FORMAT_UNSUPPORTED,
                        message,
                        "retry without --format, or upgrade the phux server",
                    ),
                    2,
                );
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
        } else if format.is_some() {
            print_rendered_capture(&screen)
        } else if unwrap {
            print_screen_rows(&screen);
            ExitCode::SUCCESS
        } else {
            print_screen_box(&screen);
            ExitCode::SUCCESS
        }
    })
}

/// `--format html|vt`: write the server's rendered capture straight to
/// stdout — HTML as UTF-8 text, VT as the raw decoded byte stream — rather
/// than through `outln!`, which would insert a newline the capture does
/// not own. `--json` bypasses this entirely and emits the whole
/// `ScreenState` document instead, `rendered` field included.
fn print_rendered_capture(screen: &ScreenState) -> ExitCode {
    let Some(rendered) = screen.rendered.as_ref() else {
        eprintln!("phux: snapshot: server returned no rendered capture");
        return ExitCode::FAILURE;
    };
    if rendered.format == phux_client::snapshot::RENDERED_FORMAT_VT {
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&rendered.data) else {
            eprintln!("phux: snapshot: malformed base64 in rendered VT capture");
            return ExitCode::FAILURE;
        };
        crate::output::bytes(&bytes);
    } else {
        crate::output::bytes(rendered.data.as_bytes());
    }
    ExitCode::SUCCESS
}

/// The history window to ask the server for
/// ([`phux_client::snapshot::history_request`]).
const fn history_request(read: &ReadOpts) -> Option<u32> {
    phux_client::snapshot::history_request(read.scrollback, read.tail)
}

/// Apply the client-side ADR-0077 projections to a reply: the one
/// implementation the MCP `phux_snapshot` tool also calls
/// ([`phux_client::snapshot::project`]).
fn project(screen: ScreenState, unwrap: bool, tail: Option<u32>) -> ScreenState {
    phux_client::snapshot::project(screen, unwrap, tail)
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
mod tests {
    use super::*;

    /// The CLI flags reach the library's history rule unchanged: `--tail N`
    /// asks for `N` history rows, and an explicit `--scrollback` wins. The
    /// projection itself is pinned beside its one implementation in
    /// `phux_client::snapshot`.
    #[test]
    fn history_request_prefers_an_explicit_scrollback() {
        let opts = |scrollback, tail| ReadOpts {
            scrollback,
            cells: false,
            tail,
            unwrap: false,
            format: None,
        };
        assert_eq!(history_request(&opts(None, None)), None);
        assert_eq!(history_request(&opts(None, Some(80))), Some(80));
        assert_eq!(history_request(&opts(Some(5), Some(80))), Some(5));
    }

    /// clap's `default_missing_value` must be a string literal, so bare
    /// `--tail`'s count and the `--help` text can drift from the constant
    /// that documents them. Pin all three here.
    #[test]
    fn bare_tail_default_matches_the_documented_constant() {
        assert_eq!(
            phux_client::snapshot::ROW_WINDOW_DEFAULT,
            80,
            "clap spells this as default_missing = \"80\" in commands::mod",
        );
        assert_eq!(
            phux_client::snapshot::ROW_WINDOW_MAX,
            10_000,
            "the --tail help text spells this as 10000",
        );
    }
}
