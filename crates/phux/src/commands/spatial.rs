//! Existing-pane layout edits over the shared L3 workspace envelope.
//!
//! These verbs never spawn a Terminal. `insert-pane` requires a Terminal that
//! already exists in the same session but is not yet present in its persisted
//! layout; implicit spawn-and-place remains a separate placement concern. All
//! selectors must resolve to exactly one local Terminal. The resulting
//! metadata write changes topology only: attached clients preserve their own
//! focus while reconciling it (ADR-0049).
//!
//! Resolution, planning, execution, and the refusal vocabulary live in
//! [`phux_client::spatial`], the one implementation the MCP spatial tools
//! also call; this file parses nothing but the CLI's own flags and reports.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::spatial::{SpatialError, SpatialOp, SpatialOutcome};
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::{SpawnSplit, cli_runtime};

pub(crate) use phux_client::spatial::Direction;

impl From<SpawnSplit> for Direction {
    fn from(split: SpawnSplit) -> Self {
        match split {
            SpawnSplit::Horizontal => Self::Horizontal,
            SpawnSplit::Vertical => Self::Vertical,
        }
    }
}

/// Insert an already-created pane beside `target`.
pub(crate) fn run_insert_pane(
    target: &str,
    new_pane: &str,
    direction: Direction,
    ratio: f32,
    projection: Vec<String>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run(
        SpatialOp::Insert {
            target: target.to_owned(),
            new_pane: new_pane.to_owned(),
            direction,
            ratio,
            projection,
        },
        json,
        socket,
    )
}

/// Relocate an existing pane beside another pane — across sessions when the
/// target lives elsewhere (ADR-0056).
pub(crate) fn run_move_pane(
    source: &str,
    target: &str,
    direction: Direction,
    ratio: f32,
    projection: Vec<String>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run(
        SpatialOp::Move {
            source: source.to_owned(),
            target: target.to_owned(),
            direction,
            ratio,
            projection,
        },
        json,
        socket,
    )
}

/// Exchange two existing pane leaves in one session layout.
pub(crate) fn run_swap_pane(
    first: &str,
    second: &str,
    projection: Vec<String>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run(
        SpatialOp::Swap {
            first: first.to_owned(),
            second: second.to_owned(),
            projection,
        },
        json,
        socket,
    )
}

fn run(operation: SpatialOp, json: bool, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let mut notices = Vec::new();
    let result = rt.block_on(phux_client::spatial::run(
        &socket_path,
        operation,
        &mut notices,
    ));
    for message in &notices {
        eprintln!("phux: warning: partial results — {message}");
    }
    match result {
        Ok(outcome) => print_success(json, &outcome),
        Err(SpatialError::Transport(err)) => {
            json_err::report_no_server(json, &err, &socket_path, "layout")
        }
        Err(SpatialError::Refused(refusal)) => json_err::emit(
            json,
            &CliError::new(refusal.code, refusal.message, refusal.remedy),
            refusal.exit_code,
        ),
    }
}

fn print_success(json: bool, outcome: &SpatialOutcome) -> ExitCode {
    if !json {
        outln!("{}", outcome.summary);
        return ExitCode::SUCCESS;
    }
    match serde_json::to_string_pretty(&outcome.document) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => json_err::emit(
            true,
            &CliError::new(
                codes::JSON_SERIALIZE,
                err.to_string(),
                "this is a phux bug; run `phux doctor` and report it",
            ),
            1,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CLI's divider flag maps onto the library's direction one to one.
    #[test]
    fn cli_split_maps_to_the_library_direction() {
        assert_eq!(
            Direction::from(SpawnSplit::Horizontal),
            Direction::Horizontal
        );
        assert_eq!(Direction::from(SpawnSplit::Vertical), Direction::Vertical);
    }

    /// Spatial errors ride the shared emitter (phux-i0e8.8.2): same
    /// versioned shape as before, now with `remedy` and `exit_code` added.
    #[test]
    fn json_error_documents_are_versioned() {
        let error = json_err::error_document(
            &CliError::new(
                phux_client::spatial::codes::SAME_PANE,
                "the two pane selectors must resolve differently",
                "pass two selectors that name different panes (`phux ls` lists them)",
            ),
            2,
        );
        assert_eq!(error["schema_version"], 1);
        assert_eq!(error["error"]["code"], "same_pane");
        assert!(
            error["remedy"].as_str().is_some_and(|r| !r.is_empty()),
            "spatial errors must carry a remedy: {error}"
        );
        assert_eq!(error["exit_code"], 2);
    }
}
