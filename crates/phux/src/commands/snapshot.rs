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

/// `phux snapshot [TARGET]` — read a pane as structured data (ADR-0022) via
/// the side-effect-free `GET_SCREEN`, emitting JSON or a boxed text view.
///
/// `--rendered` instead drives the headless client render (and ATTACHES).
/// `--tail` / `--unwrap` (ADR-0077) are client-side projections of the reply;
/// with `--format` (D9) the reply has no lines, so `--tail N` becomes the
/// request's scrollback bound and `--unwrap` rides `format`'s high bit.
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

        // `scrollback` maps onto the wire request; `format` asks the server to
        // render through libghostty's Formatter, `--unwrap` on its high bit.
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
            crate::output::json(&screen)
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

/// `--format html|vt`: write the rendered capture straight to stdout (no
/// newline added). `--json` emits the whole `ScreenState` instead.
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
    rt.block_on(async move {
        let target = match rendered_attach_target(session, socket_path, json).await {
            Ok(target) => target,
            Err(code) => return code,
        };
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
            crate::output::json(&frame)
        } else {
            print_rendered_box(&frame);
            ExitCode::SUCCESS
        }
    })
}

/// The session a `--rendered` view attaches to. A bare session name (or no
/// TARGET) attaches as written; any other selector (`@N`, `name:N.M`, `.`)
/// resolves to its pane first and attaches that pane's session, instead of
/// asking the server for a session literally named `@1`.
async fn rendered_attach_target(
    session: Option<&str>,
    socket_path: &std::path::Path,
    json: bool,
) -> Result<AttachTarget, ExitCode> {
    use phux_client::selector::Selector;
    let Some(raw) = session else {
        return Ok(AttachTarget::Last);
    };
    let selector = parse_selector(Some(raw))?;
    if let Selector::Session(name) = selector {
        return Ok(AttachTarget::ByName(name));
    }
    let pane = resolve_target(socket_path, &selector, "snapshot", json).await?;
    let snapshot = phux_client::state::get_state(socket_path)
        .await
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, "snapshot"))?
        // Only sessions and windows are read: they never aggregate.
        .into_snapshot_ignoring_degradation();
    session_of_pane(&snapshot, &pane).map_or_else(
        || {
            eprintln!("phux: {raw} is not in a local session that can be attached");
            Err(ExitCode::FAILURE)
        },
        |name| Ok(AttachTarget::ByName(name)),
    )
}

/// The name of the session whose window holds `pane`.
fn session_of_pane(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    pane: &phux_protocol::ids::ResourceId,
) -> Option<String> {
    let (_, session) = phux_client::spawn::ownership_for_terminal(snapshot, pane)?;
    snapshot
        .sessions
        .iter()
        .find(|candidate| candidate.id == session)
        .map(|session| session.name.clone())
}

/// Boxed text view of a composited [`RenderedFrame`]; wide-glyph tails are
/// empty, so a joined row is already `cols` wide.
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

/// Boxed rendering of a captured screen, with any scrollback dimmed above a
/// `╌` rule.
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

/// Plain row-per-line rendering for `--unwrap` (joined lines exceed the box):
/// history rows, then the viewport.
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

    /// `phux snapshot @1 --rendered` asked the server to attach a session
    /// named "@1". A pane selector attaches the pane's own session; a bare
    /// name is still taken as written, with no lookup.
    #[tokio::test]
    async fn rendered_view_attaches_the_session_that_holds_the_pane() {
        use phux_protocol::ids::{ResourceId, SessionId, WindowId};
        use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

        let (work, play) = (SessionId::new(1), SessionId::new(2));
        let snapshot = SessionSnapshot::new(work, WindowId::new(1), ResourceId::local(1))
            .with_sessions(vec![
                SessionInfo::new(work, "work"),
                SessionInfo::new(play, "play"),
            ])
            .with_windows(vec![
                WindowInfo::new(WindowId::new(1), work, "w"),
                WindowInfo::new(WindowId::new(2), play, "p"),
            ])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24),
                ResourceInfo::new(ResourceId::local(7), WindowId::new(2), 80, 24),
            ]);
        assert_eq!(
            session_of_pane(&snapshot, &ResourceId::local(7)).as_deref(),
            Some("play")
        );
        assert_eq!(session_of_pane(&snapshot, &ResourceId::local(9)), None);

        let unused = std::path::Path::new("/nonexistent/phux-rendered.sock");
        assert_eq!(
            rendered_attach_target(Some("work"), unused, false).await,
            Ok(AttachTarget::ByName("work".to_owned()))
        );
        assert_eq!(
            rendered_attach_target(None, unused, false).await,
            Ok(AttachTarget::Last)
        );
    }

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
