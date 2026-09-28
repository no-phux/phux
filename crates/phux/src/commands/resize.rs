use std::num::NonZeroU16;
use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::resize::{ResizeOutcome, resize_to};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};

/// A `COLSxROWS` geometry, both axes [`NonZeroU16`]. `x` and `X` are both
/// accepted as the separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Geometry {
    pub(crate) cols: NonZeroU16,
    pub(crate) rows: NonZeroU16,
}

/// Parse `COLSxROWS`, naming the offending half in diagnostics.
impl std::str::FromStr for Geometry {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_geometry(value)
    }
}

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

/// `phux resize TARGET COLSxROWS` — set a pane's grid without a TTY: resolve
/// one pane, send `RESIZE_TERMINAL`, and read the server's geometry back
/// without attaching. Under a `window-size` policy other than `manual`, an
/// attached view may supersede the size, so the exit code comes from the
/// read-back and a mismatch exits nonzero naming the policy.
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
        let pane = match resolve_target(&socket_path, &selector, "resize", json).await {
            Ok(id) => id,
            Err(code) => return code,
        };
        let outcome = match resize_to(&socket_path, &pane, geometry.cols, geometry.rows).await {
            Ok(outcome) => outcome,
            Err(err @ AttachError::Io(_)) => {
                return json_err::report_no_server(json, &err, &socket_path, "resize");
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

/// Emit the outcome and pick the exit code. The report prints on both paths:
/// a mismatch means the command ran and the held size is what the caller
/// needs.
fn report(pane: &phux_protocol::ids::ResourceId, outcome: ResizeOutcome, json: bool) -> ExitCode {
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

    /// Each malformed spelling fails with a diagnostic naming its fix; a zero
    /// axis never reaches libghostty, which would silently clamp it.
    #[test]
    fn rejects_malformed_geometry() {
        for (spec, hint) in [
            ("12040", "COLSxROWS"),
            ("0x40", "at least 1"),
            ("120x0", "at least 1"),
            ("wide x tall", "whole number"),
        ] {
            let err = parse_geometry(spec).expect_err(spec);
            assert!(err.contains(hint), "{spec}: unhelpful diagnostic: {err}");
        }
    }
}
