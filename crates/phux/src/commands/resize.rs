use std::num::NonZeroU16;
use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::resize::{ResizeOutcome, resize_to};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, parse_selector, report_no_server, resolve_target};

/// A `COLSxROWS` geometry argument, parsed and range-checked.
///
/// Both axes are [`NonZeroU16`] so the "a grid has at least one cell" rule
/// is carried by the type all the way down into
/// [`phux_client::resize::resize_to`], instead of being a check each caller
/// has to remember. `x` and `X` are both accepted as the separator because
/// `120X40` is what a shell autocompletion or a copy-paste from a `stty`
/// transcript will occasionally produce, and rejecting it teaches nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Geometry {
    pub(crate) cols: NonZeroU16,
    pub(crate) rows: NonZeroU16,
}

/// Parse `COLSxROWS` for clap's `value_parser`.
///
/// Diagnostics name the offending half rather than re-printing the whole
/// argument, because the overwhelmingly common mistakes are a transposed
/// separator (`120,40`) and a zero (`0x40`), and both are one word to fix.
pub(crate) fn parse_geometry(value: &str) -> Result<Geometry, String> {
    let (cols, rows) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected COLSxROWS (e.g. 120x40), got '{value}'"))?;
    Ok(Geometry {
        cols: parse_axis(cols, "cols")?,
        rows: parse_axis(rows, "rows")?,
    })
}

/// Parse one axis of a geometry, rejecting zero with the reason rather than
/// a bare range error: a zero-dimension grid is not a small grid, it is a
/// grid libghostty refuses to build.
fn parse_axis(value: &str, axis: &str) -> Result<NonZeroU16, String> {
    let parsed: u16 = value
        .parse()
        .map_err(|_| format!("{axis} must be a whole number of cells, got '{value}'"))?;
    NonZeroU16::new(parsed)
        .ok_or_else(|| format!("{axis} must be at least 1; a 0-cell grid does not exist"))
}

/// `phux resize TARGET COLSxROWS` — set a pane's grid without a TTY.
///
/// Resolves `TARGET` client-side to exactly one pane, then sends the
/// `TERMINAL_RESIZE` frame the wire has always carried and reads the
/// server's own post-resize geometry back. This neither attaches nor
/// subscribes, so the caller never becomes a view whose 80x24 no-TTY
/// viewport would fight the size it just asked for.
///
/// The resize applies immediately whether or not a human is attached. What
/// it does not do is *win* against the `defaults.window-size` policy
/// forever: under `smallest` / `largest` / `latest` the next attach,
/// detach, or `SIGWINCH` recomputes the Terminal's geometry from the
/// attached views and supersedes it, and under `manual` nothing ever does.
/// That is why the exit code is derived from the read-back and not from the
/// send — a script that resizes a pane a human is looking at gets a nonzero
/// exit and a diagnostic naming the policy, never a confident success and an
/// 80x24 pane.
pub(crate) fn run_resize(
    target: &str,
    geometry: Geometry,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let pane = match resolve_target(&socket_path, &selector, "resize").await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let outcome = match resize_to(&socket_path, &pane, geometry.cols, geometry.rows).await {
            Ok(outcome) => outcome,
            Err(err @ AttachError::Io(_)) => {
                return report_no_server(&err, &socket_path, "resize");
            }
            Err(AttachError::Refused(msg)) => {
                eprintln!("phux: cannot resize '{target}': {msg} (try `phux ls`)");
                return ExitCode::FAILURE;
            }
            Err(err) => {
                eprintln!("phux: resize failed: {err}");
                return ExitCode::FAILURE;
            }
        };
        report(&pane, outcome, json)
    })
}

/// Emit the outcome and pick the exit code from it.
///
/// The report is printed on *both* paths, unlike the verbs whose `--json`
/// failures leave stdout empty. Those failures mean "the command did not
/// run"; this one means "the command ran and the server holds a different
/// size than you asked for", and the size it holds is precisely the thing a
/// caller needs in order to react.
fn report(pane: &phux_protocol::ids::TerminalId, outcome: ResizeOutcome, json: bool) -> ExitCode {
    let (cols, rows) = outcome.applied;
    if json {
        let doc = serde_json::json!({
            "schema_version": 1,
            "terminal_id": pane.local_id(),
            "requested": { "cols": outcome.requested.0, "rows": outcome.requested.1 },
            "applied": { "cols": cols, "rows": rows },
            "held": outcome.held(),
        });
        outln!("{doc}");
    } else {
        outln!("{cols}x{rows}");
    }
    if outcome.held() {
        return ExitCode::SUCCESS;
    }
    let (want_cols, want_rows) = outcome.requested;
    eprintln!(
        "phux: pane holds {cols}x{rows}, not the requested {want_cols}x{want_rows}. \
         An attached client's viewport owns this Terminal's size under the \
         current `defaults.window-size` policy; set `window-size = \"manual\"` \
         to make explicit resizes authoritative."
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests"
    )]

    use super::*;

    #[test]
    fn parses_a_well_formed_geometry() {
        let geom = parse_geometry("120x40").expect("120x40 parses");
        assert_eq!(geom.cols.get(), 120);
        assert_eq!(geom.rows.get(), 40);
        assert_eq!(parse_geometry("120X40").expect("uppercase X parses"), geom);
    }

    #[test]
    fn rejects_a_missing_separator() {
        let err = parse_geometry("12040").expect_err("no separator must fail");
        assert!(err.contains("COLSxROWS"), "unhelpful diagnostic: {err}");
    }

    #[test]
    fn rejects_zero_on_either_axis() {
        // The whole reason `Geometry` holds NonZeroU16: a zero here would
        // otherwise travel all the way to libghostty, which clamps it, and
        // the caller would be told it got a size no pane has.
        for spec in ["0x40", "120x0"] {
            let Err(err) = parse_geometry(spec) else {
                panic!("{spec} must be rejected");
            };
            assert!(err.contains("at least 1"), "unhelpful diagnostic: {err}");
        }
    }

    #[test]
    fn rejects_non_numeric_axes() {
        let err = parse_geometry("wide x tall").expect_err("words must fail");
        assert!(err.contains("whole number"), "unhelpful diagnostic: {err}");
    }
}
