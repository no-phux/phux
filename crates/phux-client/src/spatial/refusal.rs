use crate::layout_ops::LayoutOpsError;
use crate::pane_move::PaneMoveError;

use super::plan::same_pane_error;
use super::{SpatialError, SpatialRefusal, codes};

pub(super) fn move_error(error: PaneMoveError) -> SpatialError {
    match error {
        PaneMoveError::Transport(err) => SpatialError::Transport(err),
        other => SpatialError::Refused(move_refusal(&other)),
    }
}

/// The contract code, remedy, and exit status for a failed move: `2` for a
/// preflight refusal, `1` once ownership work has begun.
fn move_refusal(error: &PaneMoveError) -> SpatialRefusal {
    let (code, remedy, exit_code) = match error {
        PaneMoveError::ServerTooOld => (
            codes::SERVER_TOO_OLD,
            "upgrade it with `phux upgrade`, then retry",
            1,
        ),
        PaneMoveError::SatellitePane => (
            codes::SATELLITE_TARGET,
            "pick a hub-local pane for layout edits",
            1,
        ),
        PaneMoveError::DestinationChanged { .. } => (
            codes::DESTINATION_CHANGED,
            "re-run `phux ls` and retry with current selectors",
            1,
        ),
        PaneMoveError::PostMoveState { .. } => (
            codes::POST_MOVE_STATE_FAILED,
            "run `phux ls` to verify where the pane landed",
            1,
        ),
        PaneMoveError::DestinationLayout { .. } => (
            codes::DESTINATION_LAYOUT_FAILED,
            "run `phux ls` to verify pane ownership, then retry the move",
            1,
        ),
        PaneMoveError::SourceLayout(_) => (
            codes::SOURCE_LAYOUT_FAILED,
            "retry the layout edit before relying on either session's topology",
            1,
        ),
        PaneMoveError::SamePane => (
            codes::SAME_PANE,
            "pass two selectors that name different panes",
            1,
        ),
        PaneMoveError::UnknownPane { .. } => (
            codes::SELECTOR_MISS,
            "run `phux ls` to see live sessions and panes",
            1,
        ),
        PaneMoveError::MoveRefused(_) | PaneMoveError::Org(_) => (
            codes::MOVE_REFUSED,
            "run `phux ls` to re-check both panes, then retry",
            1,
        ),
        PaneMoveError::Layout(_) => (
            codes::LAYOUT_REJECTED,
            "run `phux ls` to inspect the winning layout, then retry",
            1,
        ),
        PaneMoveError::ProjectionArityMismatch => (
            codes::PROJECTION_ARITY,
            "pass --projection twice (source and destination keys) or not at all",
            2,
        ),
        PaneMoveError::ProjectionArityTooMany => (
            codes::PROJECTION_ARITY,
            "this move touches one session layout; pass at most one --projection",
            2,
        ),
        // Routed to `SpatialError::Transport` before this table.
        PaneMoveError::Transport(_) => {
            (codes::TRANSPORT, "run `phux doctor` for a health check", 1)
        }
    };
    SpatialRefusal::new(code, error.to_string(), remedy, exit_code)
}

/// The contract code for a refused same-session layout write.
pub(super) fn layout_error(err: LayoutOpsError) -> SpatialError {
    if let LayoutOpsError::Transport(transport) = err {
        return SpatialError::Transport(transport);
    }
    let refusal = match &err {
        LayoutOpsError::MissingLayout => SpatialRefusal::new(
            codes::LAYOUT_MISSING,
            "session has no persisted layout; attach a TUI before editing topology",
            "attach once with `phux attach SESSION` to seed the layout, then retry",
            2,
        ),
        LayoutOpsError::ForeignTarget(_) => SpatialRefusal::new(
            codes::PANE_NOT_IN_LAYOUT,
            "a selected pane is not present in this session's persisted layout",
            "insert it first with `phux insert-pane`",
            2,
        ),
        LayoutOpsError::DuplicatePane(_) => SpatialRefusal::new(
            codes::PANE_ALREADY_IN_LAYOUT,
            "the pane being inserted is already present in the persisted layout",
            "use `phux mv` to relocate a pane the layout already holds",
            2,
        ),
        LayoutOpsError::SamePane => same_pane_error(),
        LayoutOpsError::InvalidProjectionKey(_) => SpatialRefusal::new(
            codes::PROJECTION_INVALID,
            err.to_string(),
            "pass a key shaped `<prefix>.layout/v1/<session-id>` for this session",
            2,
        ),
        other => SpatialRefusal::new(
            codes::LAYOUT_REJECTED,
            other.to_string(),
            "run `phux doctor` for a health check",
            2,
        ),
    };
    SpatialError::Refused(refusal)
}

pub(super) fn internal_error(message: &str) -> SpatialRefusal {
    SpatialRefusal::new(
        codes::INTERNAL_ERROR,
        message,
        "this is a phux bug; run `phux doctor` and report it",
        2,
    )
}
