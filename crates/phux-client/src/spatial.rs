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
mod slot;

use plan::{build_plan, read_snapshot, validate_ratio};
use refusal::{layout_error, move_error};

const JSON_SCHEMA_VERSION: u8 = 1;

/// The refusal codes a spatial edit reports: its slice of the one closed
/// `--json` vocabulary in [`crate::contract::codes`]
/// (`docs/scripting.md#json-output-and-errors`).
pub mod codes {
    pub use crate::contract::codes::{
        CROSS_SESSION, DESTINATION_CHANGED, DESTINATION_LAYOUT_FAILED, INTERNAL_ERROR,
        INVALID_RATIO, INVALID_SELECTOR, LAYOUT_MISSING, LAYOUT_REJECTED, MOVE_REFUSED, NOT_A_TAB,
        PANE_ALREADY_IN_LAYOUT, PANE_NOT_IN_LAYOUT, POST_MOVE_STATE_FAILED, PROJECTION_ARITY,
        PROJECTION_INVALID, SAME_PANE, SATELLITE_TARGET, SELECTOR_MISS, SELECTOR_NOT_SINGLE,
        SLOT_TABS_UNSUPPORTED, SOURCE_LAYOUT_FAILED, TRANSPORT, UNKNOWN_TERMINAL_SESSION,
    };
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
    /// Make `pane` a tab of the slot showing `to`
    /// (`docs/organization.md#slot-tabs`).
    SlotAdd {
        /// The pane to make a tab.
        pane: String,
        /// Any tab of the slot it joins.
        to: String,
    },
    /// Give the tab `pane` a leaf of its own beside its slot.
    SlotOut {
        /// The tab.
        pane: String,
        /// Divider direction.
        direction: Direction,
        /// Split ratio left to the slot, strictly inside `(0, 1)`.
        ratio: f32,
    },
    /// Move the stored default of `pane`'s slot `delta` places.
    SlotStep {
        /// Any tab of the slot.
        pane: String,
        /// `1` for next, `-1` for previous.
        delta: isize,
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
    if operation.is_slot()
        && !conn
            .server_features_ext()
            .contains(phux_protocol::caps::ServerFeatureExt::SlotTabs)
    {
        return Err(SpatialError::Refused(slot::unsupported()));
    }
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
