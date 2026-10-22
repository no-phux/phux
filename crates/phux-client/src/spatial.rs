//! Existing-pane layout edits over the shared L3 workspace envelope:
//! `insert-pane`, `move-pane`, `swap-pane` (ADR-0049, ADR-0056, ADR-0129).
//!
//! Shared by the CLI verbs and the MCP tools, so both return the same
//! document and refusal codes. These edits never spawn a Terminal, every
//! selector must resolve to exactly one local Terminal, and only a move may
//! cross sessions.

use std::path::Path;

use phux_protocol::ids::{ResourceId, SessionId};
use serde_json::Value;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::SplitDir;
use crate::layout_ops::LayoutMutation;
use crate::pane_move;

mod plan;
mod refusal;

use plan::{build_plan, read_snapshot, validate_ratio};
use refusal::{layout_error, move_error};

const JSON_SCHEMA_VERSION: u8 = 1;

/// The refusal codes a spatial edit reports, the `--json` error contract's
/// vocabulary (`docs/consumers/agents.md` §7).
pub mod codes {
    /// A selector did not parse.
    pub const INVALID_SELECTOR: &str = "invalid_selector";
    /// `ratio` was not finite and strictly inside `(0, 1)`.
    pub const INVALID_RATIO: &str = "invalid_ratio";
    /// A selector matched no pane.
    pub const SELECTOR_MISS: &str = "selector_miss";
    /// A selector matched more than one pane.
    pub const SELECTOR_NOT_SINGLE: &str = "selector_not_single";
    /// A selector resolved to a satellite pane.
    pub const SATELLITE_TARGET: &str = "satellite_target";
    /// Two selectors resolved to the same pane.
    pub const SAME_PANE: &str = "same_pane";
    /// The panes of a same-session edit live in different sessions.
    pub const CROSS_SESSION: &str = "cross_session";
    /// A resolved pane has no session in the snapshot.
    pub const UNKNOWN_TERMINAL_SESSION: &str = "unknown_terminal_session";
    /// The session has no persisted layout.
    pub const LAYOUT_MISSING: &str = "layout_missing";
    /// A selected pane is not in the persisted layout.
    pub const PANE_NOT_IN_LAYOUT: &str = "pane_not_in_layout";
    /// The pane being inserted is already in the layout.
    pub const PANE_ALREADY_IN_LAYOUT: &str = "pane_already_in_layout";
    /// The layout engine or the server refused the edit.
    pub const LAYOUT_REJECTED: &str = "layout_rejected";
    /// The server predates cross-session moves.
    pub const SERVER_TOO_OLD: &str = "server_too_old";
    /// The server refused the ownership move.
    pub const MOVE_REFUSED: &str = "move_refused";
    /// Ownership moved but the resulting state could not be read.
    pub const POST_MOVE_STATE_FAILED: &str = "post_move_state_failed";
    /// The destination changed windows during the move.
    pub const DESTINATION_CHANGED: &str = "destination_changed";
    /// The destination layout did not publish.
    pub const DESTINATION_LAYOUT_FAILED: &str = "destination_layout_failed";
    /// The source layout could not be cleaned up after the move.
    pub const SOURCE_LAYOUT_FAILED: &str = "source_layout_failed";
    /// A `projection` key did not name the addressed session.
    pub const PROJECTION_INVALID: &str = "projection_invalid";
    /// The wrong number of `projection` keys for the edit.
    pub const PROJECTION_ARITY: &str = "projection_arity";
    /// A defect in this module.
    pub const INTERNAL_ERROR: &str = "internal_error";
    /// A transport failure after the edit began.
    pub const TRANSPORT: &str = "transport";
}

/// The user-facing divider direction of a split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// A horizontal divider: the panes stack.
    Horizontal,
    /// A vertical divider: the panes sit side by side.
    Vertical,
}

impl Direction {
    /// Map the user-facing divider direction onto the internal child axis.
    /// A horizontal divider stacks panes (`SplitDir::Vertical`); a vertical
    /// divider places them side-by-side (`SplitDir::Horizontal`).
    #[must_use]
    pub const fn wire(self) -> SplitDir {
        match self {
            Self::Horizontal => SplitDir::Vertical,
            Self::Vertical => SplitDir::Horizontal,
        }
    }

    /// The divider label the document carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

/// One requested edit, with its selectors as the caller typed them.
#[derive(Debug, Clone)]
pub enum SpatialOp {
    /// Insert an already-created pane beside `target`.
    Insert {
        /// The pane the new leaf goes beside.
        target: String,
        /// The existing pane to insert.
        new_pane: String,
        /// Divider direction.
        direction: Direction,
        /// Split ratio, strictly inside `(0, 1)`.
        ratio: f32,
        /// Named projection keys (ADR-0129); at most one.
        projection: Vec<String>,
    },
    /// Relocate `source` beside `target`, across sessions when they differ.
    Move {
        /// The pane to move.
        source: String,
        /// The pane it lands beside.
        target: String,
        /// Divider direction.
        direction: Direction,
        /// Split ratio, strictly inside `(0, 1)`.
        ratio: f32,
        /// Named projection keys: at most one for a same-session move, zero
        /// or two (source, destination) for a cross-session one.
        projection: Vec<String>,
    },
    /// Exchange two leaves in one session's layout.
    Swap {
        /// One pane.
        first: String,
        /// The other pane.
        second: String,
        /// Named projection keys (ADR-0129); at most one.
        projection: Vec<String>,
    },
}

/// A completed edit: the `--json` document and the one-line human summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpatialOutcome {
    /// The versioned result document (`schema_version` 1).
    pub document: Value,
    /// The human confirmation line.
    pub summary: String,
}

/// A refusal on the `--json` error contract: a stable code, the message,
/// the remedy, and the exit code the CLI reports it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpatialRefusal {
    /// Stable code from [`codes`].
    pub code: &'static str,
    /// What went wrong.
    pub message: String,
    /// What to do about it.
    pub remedy: &'static str,
    /// `2` for a preflight refusal, `1` once ownership work has begun.
    pub exit_code: u8,
}

impl SpatialRefusal {
    fn new(
        code: &'static str,
        message: impl Into<String>,
        remedy: &'static str,
        exit_code: u8,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            remedy,
            exit_code,
        }
    }
}

/// Why a spatial edit did not complete.
#[derive(Debug)]
pub enum SpatialError {
    /// No server, or the connection failed.
    Transport(AttachError),
    /// The edit was refused; nothing, or only what the message says, changed.
    Refused(SpatialRefusal),
}

impl From<SpatialRefusal> for SpatialError {
    fn from(refusal: SpatialRefusal) -> Self {
        Self::Refused(refusal)
    }
}

#[derive(Debug)]
struct Plan {
    session: SessionId,
    mutation: LayoutMutation,
    /// Named projection key (ADR-0129); `None` is the default key.
    projection_key: Option<String>,
    outcome: SpatialOutcome,
}

/// A move across sessions (ADR-0056), executed by [`pane_move::move_pane`],
/// which also resolves the raw `projection` values.
#[derive(Debug)]
struct CrossMovePlan {
    source: ResourceId,
    target: ResourceId,
    dir: SplitDir,
    ratio: f32,
    projection: Vec<String>,
    outcome: SpatialOutcome,
}

#[derive(Debug)]
enum PlanKind {
    Local(Plan),
    CrossMove(CrossMovePlan),
}

/// Validate, resolve, plan, and execute one spatial edit on a fresh
/// connection, appending snapshot degradation notices to `notices`.
///
/// # Errors
///
/// [`SpatialError::Refused`] with a stable code, or
/// [`SpatialError::Transport`].
pub async fn run(
    socket: &Path,
    operation: SpatialOp,
    notices: &mut Vec<String>,
) -> Result<SpatialOutcome, SpatialError> {
    if let Some(ratio) = operation.ratio() {
        validate_ratio(ratio)?;
    }
    let selectors = operation.parse_selectors()?;
    let mut conn = Connection::connect(socket)
        .await
        .map_err(SpatialError::Transport)?;
    let snapshot = read_snapshot(&mut conn, 0, notices)
        .await
        .map_err(SpatialError::Transport)?;
    let plan = build_plan(socket, &snapshot, operation, selectors).await?;
    let result = match plan {
        PlanKind::Local(plan) => execute_local(&mut conn, plan).await,
        PlanKind::CrossMove(plan) => execute_cross_move(&mut conn, plan).await,
    };
    drop(conn);
    result
}

/// Apply a same-session plan through the shared `LayoutOps` CAS.
async fn execute_local(conn: &mut Connection, plan: Plan) -> Result<SpatialOutcome, SpatialError> {
    let Plan {
        session,
        mutation,
        projection_key,
        outcome,
    } = plan;
    let mut layout =
        pane_move::layout_ops(conn, session, projection_key, 100).map_err(layout_error)?;
    let mutated = layout.mutate(mutation).await;
    drop(layout);
    mutated.map_err(layout_error)?;
    Ok(outcome)
}

async fn execute_cross_move(
    conn: &mut Connection,
    plan: CrossMovePlan,
) -> Result<SpatialOutcome, SpatialError> {
    pane_move::move_pane(
        conn,
        plan.source,
        plan.target,
        plan.dir,
        plan.ratio,
        &plan.projection,
    )
    .await
    .map_err(move_error)?;
    Ok(plan.outcome)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests;
