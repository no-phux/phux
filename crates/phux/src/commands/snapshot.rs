use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::{AttachError, run_headless_rendered};
use phux_client::snapshot::{RenderedFrame, ScreenState};
use phux_protocol::wire::frame::AttachTarget;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, parse_selector, report_no_server, resolve_target};

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
pub(crate) fn run_snapshot(
    session: Option<&str>,
    json: bool,
    scrollback: Option<u32>,
    cells: bool,
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

    rt.block_on(async move {
        let terminal_id = match resolve_target(&socket_path, &selector, "snapshot").await {
            Ok(id) => id,
            Err(code) => return code,
        };

        // Read the screen — side-effect-free, safe to poll. `scrollback`
        // maps straight onto the wire request: None/Some(0=all)/Some(n);
        // `cells` requests the per-cell semantic/style projection.
        let screen = match phux_client::snapshot::get_screen_scrollback(
            &socket_path,
            terminal_id,
            scrollback,
            cells,
        )
        .await
        {
            Ok(screen) => screen,
            Err(err @ AttachError::Io(_)) => {
                return report_no_server(&err, &socket_path, "snapshot");
            }
            Err(err) => {
                eprintln!("phux: snapshot failed: {err}");
                return ExitCode::FAILURE;
            }
        };

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
        } else {
            print_screen_box(&screen);
            ExitCode::SUCCESS
        }
    })
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
                return report_no_server(&err, socket_path, "snapshot");
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
    let cursor = screen
        .cursor
        .as_ref()
        .map_or_else(|| "none".to_owned(), |c| format!("{},{}", c.x, c.y));
    outln!(
        "pane={} {}x{} cursor={cursor}",
        screen.pane,
        screen.cols,
        screen.rows
    );
}
