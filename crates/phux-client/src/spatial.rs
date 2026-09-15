//! Existing-pane layout edits over the shared L3 workspace envelope:
//! `insert-pane`, `move-pane`, `swap-pane` (ADR-0049, ADR-0056, ADR-0129).
//!
//! One implementation of selector resolution, the plan, and its execution,
//! shared by the CLI verbs and the MCP spatial tools, so both return the
//! same document and the same refusal codes.
//!
//! These edits never spawn a Terminal. `insert-pane` requires a Terminal
//! that already exists in the same session but is not yet present in its
//! persisted layout; implicit spawn-and-place remains a separate placement
//! concern. All selectors must resolve to exactly one local Terminal. The
//! resulting metadata write changes topology only: attached clients
//! preserve their own focus while reconciling it (ADR-0049).

use std::path::Path;

use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, StateScope};
use phux_protocol::wire::info::SessionSnapshot;
use serde_json::{Value, json};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::SplitDir;
use crate::layout_ops::{LayoutMutation, LayoutOps, LayoutOpsError, validate_projection_key};
use crate::pane_move::{self, PaneMoveError};
use crate::selector::{self, Selector, format_terminal_id};
use crate::state::Degradation;

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
    /// Named projection key (ADR-0129), validated against `session`.
    /// `None` keeps the shared default `phux.tui.layout/v1/<session>`.
    projection_key: Option<String>,
    outcome: SpatialOutcome,
}

/// A move whose source and destination panes live in different sessions
/// (ADR-0056): ownership moves on L1 via `MOVE_RESOURCE`, then geometry is
/// written client-side — a `Split` into the destination envelope and a
/// `Close` out of the source envelope.
#[derive(Debug)]
struct CrossMovePlan {
    source: ResourceId,
    target: ResourceId,
    dir: SplitDir,
    ratio: f32,
    /// Raw `projection` values, resolved and validated by
    /// [`pane_move::move_pane`] against the two sessions it discovers at
    /// execution time.
    projection: Vec<String>,
    outcome: SpatialOutcome,
}

#[derive(Debug)]
enum PlanKind {
    Local(Plan),
    CrossMove(CrossMovePlan),
}

/// Validate, resolve, plan, and execute one spatial edit on a fresh
/// connection to `socket`.
///
/// Degradation notices seen on the snapshot read are appended to `notices`
/// in encounter order, for the caller to print.
///
/// # Errors
///
/// [`SpatialError::Refused`] with the stable code for a malformed request, a
/// selector that does not name exactly one local pane, or a refused layout
/// or ownership write; [`SpatialError::Transport`] when the server cannot be
/// reached or the connection fails.
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
    let mut layout = match projection_key {
        Some(key) => LayoutOps::with_key(conn, session, key, 100).map_err(layout_error)?,
        None => LayoutOps::new(conn, session, 100),
    };
    let mutated = layout.mutate(mutation).await;
    drop(layout);
    mutated.map_err(layout_error)?;
    Ok(outcome)
}

/// Execute a cross-session move (ADR-0056): feature-gate, re-parent on L1,
/// then write geometry — destination first, so a failed placement rolls
/// back with a single inverse `MOVE_RESOURCE` and no layout repair.
///
/// The source envelope's stale leaf is dropped last. If the move reaped the
/// source session, its one-leaf envelope is deleted instead; cleanup failures
/// are reported because the ownership move has already committed.
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

fn move_error(error: PaneMoveError) -> SpatialError {
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
        PaneMoveError::MoveRefused(_) => (
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
        // `move_error` routes transport failures to `SpatialError::Transport`
        // before this table is consulted; the arm keeps the match total.
        PaneMoveError::Transport(_) => {
            (codes::TRANSPORT, "run `phux doctor` for a health check", 1)
        }
    };
    SpatialRefusal::new(code, error.to_string(), remedy, exit_code)
}

/// The contract code for a refused same-session layout write.
fn layout_error(err: LayoutOpsError) -> SpatialError {
    let refusal = match &err {
        LayoutOpsError::Transport(_) => {
            let LayoutOpsError::Transport(transport) = err else {
                return SpatialError::Refused(internal_error("layout transport mismatch"));
            };
            return SpatialError::Transport(transport);
        }
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
            "use `phux move-pane` to relocate a pane the layout already holds",
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

fn internal_error(message: &str) -> SpatialRefusal {
    SpatialRefusal::new(
        codes::INTERNAL_ERROR,
        message,
        "this is a phux bug; run `phux doctor` and report it",
        2,
    )
}

/// The cross-session plan, when `operation` is a move whose two panes
/// resolve to different sessions; `None` keeps the local same-session path.
fn cross_move_plan(
    snapshot: &SessionSnapshot,
    operation: &SpatialOp,
    terminals: &[ResourceId],
) -> Option<PlanKind> {
    let SpatialOp::Move {
        direction,
        ratio,
        projection,
        ..
    } = operation
    else {
        return None;
    };
    let [source, target] = terminals else {
        return None;
    };
    let source_session = session_for(snapshot, source)?;
    let dest_session = session_for(snapshot, target)?;
    if source_session == dest_session {
        return None;
    }
    let (ratio, direction) = (*ratio, *direction);
    Some(PlanKind::CrossMove(CrossMovePlan {
        source: source.clone(),
        target: target.clone(),
        dir: direction.wire(),
        ratio,
        projection: projection.clone(),
        outcome: SpatialOutcome {
            document: json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "move-pane",
                "session_id": dest_session.get(),
                "source_session_id": source_session.get(),
                "source_terminal_id": local_id(source),
                "target_terminal_id": local_id(target),
                "direction": direction.as_str(),
                "ratio": ratio,
                "cross_session": true,
            }),
            summary: format!(
                "moved @{} beside @{} across sessions ({}, ratio {ratio})",
                local_id(source),
                local_id(target),
                direction.as_str(),
            ),
        },
    }))
}

impl SpatialOp {
    const fn ratio(&self) -> Option<f32> {
        match self {
            Self::Insert { ratio, .. } | Self::Move { ratio, .. } => Some(*ratio),
            Self::Swap { .. } => None,
        }
    }

    fn parse_selectors(&self) -> Result<Vec<Selector>, SpatialRefusal> {
        self.raw_selectors()
            .into_iter()
            .map(|(role, raw)| {
                selector::parse(raw).map_err(|err| {
                    SpatialRefusal::new(
                        codes::INVALID_SELECTOR,
                        format!("invalid {role} selector {raw:?}: {err}"),
                        "selector grammar: session, session:window, session:window.pane, @id, `.`",
                        2,
                    )
                })
            })
            .collect()
    }

    fn raw_selectors(&self) -> Vec<(&'static str, &str)> {
        match self {
            Self::Insert {
                target, new_pane, ..
            } => vec![("target", target), ("new-pane", new_pane)],
            Self::Move { source, target, .. } => {
                vec![("source", source), ("target", target)]
            }
            Self::Swap { first, second, .. } => vec![("first", first), ("second", second)],
        }
    }
}

/// `GET_STATE` on `conn`, keeping the historical refusal text: a refusal is
/// reported through `explain_unexpected`, not as `AttachError::Refused`.
async fn read_snapshot(
    conn: &mut Connection,
    request_id: u32,
    notices: &mut Vec<String>,
) -> Result<SessionSnapshot, AttachError> {
    let (result, interleaved) = conn
        .request(
            request_id,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await?
        .into_parts();
    notices.extend_from_slice(Degradation::from_interleaved(&interleaved).notices());
    match result {
        CommandResult::OkWith(CommandValue::State(snapshot)) => Ok(snapshot),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "GET_STATE",
            &other,
        ))),
    }
}

async fn build_plan(
    socket: &Path,
    snapshot: &SessionSnapshot,
    operation: SpatialOp,
    selectors: Vec<Selector>,
) -> Result<PlanKind, SpatialRefusal> {
    let roles = operation.raw_selectors();
    let mut terminals = Vec::with_capacity(selectors.len());
    for ((role, _), selector) in roles.iter().zip(&selectors) {
        let candidates = crate::state::resolve_targets(socket, selector, snapshot).await;
        terminals.push(exactly_one_local(role, &candidates)?);
    }
    if terminals.len() == 2 && terminals[0] == terminals[1] {
        return Err(same_pane_error());
    }

    // Cross-session move (ADR-0056): the one spatial operation that may span
    // sessions. Ownership moves on L1 via MOVE_RESOURCE; the two layout
    // writes stay client-side. Every other operation keeps the same-session
    // requirement below.
    if let Some(plan) = cross_move_plan(snapshot, &operation, &terminals) {
        return Ok(plan);
    }

    let session = same_session(snapshot, &terminals)?;

    match (operation, terminals.as_slice()) {
        (
            SpatialOp::Insert {
                direction,
                ratio,
                projection,
                ..
            },
            [target, new_pane],
        ) => build_insert_plan(session, target, new_pane, direction, ratio, &projection),
        (
            SpatialOp::Move {
                direction,
                ratio,
                projection,
                ..
            },
            [source, target],
        ) => build_move_plan(session, source, target, direction, ratio, &projection),
        (SpatialOp::Swap { projection, .. }, [first, second]) => {
            build_swap_plan(session, first, second, &projection)
        }
        _ => Err(internal_error("spatial operation argument mismatch")),
    }
}

fn build_insert_plan(
    session: SessionId,
    target: &ResourceId,
    new_pane: &ResourceId,
    direction: Direction,
    ratio: f32,
    projection: &[String],
) -> Result<PlanKind, SpatialRefusal> {
    let projection_key = resolve_local_projection(projection, session)?;
    Ok(PlanKind::Local(Plan {
        session,
        mutation: LayoutMutation::Split {
            target: target.clone(),
            new_pane: new_pane.clone(),
            dir: direction.wire(),
            ratio,
        },
        projection_key,
        outcome: SpatialOutcome {
            document: json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "insert-pane",
                "session_id": session.get(),
                "target_terminal_id": local_id(target),
                "new_terminal_id": local_id(new_pane),
                "direction": direction.as_str(),
                "ratio": ratio,
            }),
            summary: format!(
                "inserted @{} beside @{} ({}, ratio {ratio})",
                local_id(new_pane),
                local_id(target),
                direction.as_str(),
            ),
        },
    }))
}

fn build_move_plan(
    session: SessionId,
    source: &ResourceId,
    target: &ResourceId,
    direction: Direction,
    ratio: f32,
    projection: &[String],
) -> Result<PlanKind, SpatialRefusal> {
    let projection_key = resolve_local_projection(projection, session)?;
    Ok(PlanKind::Local(Plan {
        session,
        mutation: LayoutMutation::Move {
            source: source.clone(),
            target: target.clone(),
            dir: direction.wire(),
            ratio,
        },
        projection_key,
        outcome: SpatialOutcome {
            document: json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "move-pane",
                "session_id": session.get(),
                "source_terminal_id": local_id(source),
                "target_terminal_id": local_id(target),
                "direction": direction.as_str(),
                "ratio": ratio,
            }),
            summary: format!(
                "moved @{} beside @{} ({}, ratio {ratio})",
                local_id(source),
                local_id(target),
                direction.as_str(),
            ),
        },
    }))
}

fn build_swap_plan(
    session: SessionId,
    first: &ResourceId,
    second: &ResourceId,
    projection: &[String],
) -> Result<PlanKind, SpatialRefusal> {
    let projection_key = resolve_local_projection(projection, session)?;
    Ok(PlanKind::Local(Plan {
        session,
        mutation: LayoutMutation::Swap {
            first: first.clone(),
            second: second.clone(),
        },
        projection_key,
        outcome: SpatialOutcome {
            document: json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "swap-pane",
                "session_id": session.get(),
                "first_terminal_id": local_id(first),
                "second_terminal_id": local_id(second),
            }),
            summary: format!("swapped @{} and @{}", local_id(first), local_id(second)),
        },
    }))
}

/// Resolve `projection` for an operation that addresses exactly one
/// session layout (`insert-pane`, `swap-pane`, and a same-session
/// `move-pane`): `[]` keeps the shared default key, `[key]` is validated
/// against `session`, and anything else is a usage error (ADR-0129).
fn resolve_local_projection(
    projection: &[String],
    session: SessionId,
) -> Result<Option<String>, SpatialRefusal> {
    match projection {
        [] => Ok(None),
        [key] => validate_projection_key(key, session)
            .map(|()| Some(key.clone()))
            .map_err(|err| {
                SpatialRefusal::new(
                    codes::PROJECTION_INVALID,
                    err.to_string(),
                    "pass a key shaped `<prefix>.layout/v1/<session-id>` for this session",
                    2,
                )
            }),
        _ => Err(SpatialRefusal::new(
            codes::PROJECTION_ARITY,
            "this operation touches one session layout; pass at most one --projection",
            "drop the extra --projection flags",
            2,
        )),
    }
}

/// The shared "two selectors, one pane" refusal (raised both client-side and
/// by the server's layout engine).
fn same_pane_error() -> SpatialRefusal {
    SpatialRefusal::new(
        codes::SAME_PANE,
        "the two pane selectors must resolve differently",
        "pass two selectors that name different panes (`phux ls` lists them)",
        2,
    )
}

fn validate_ratio(ratio: f32) -> Result<(), SpatialRefusal> {
    if ratio.is_finite() && ratio > 0.0 && ratio < 1.0 {
        Ok(())
    } else {
        Err(SpatialRefusal::new(
            codes::INVALID_RATIO,
            format!("ratio must be finite and strictly between 0 and 1; got {ratio}"),
            "pass e.g. --ratio 0.5",
            2,
        ))
    }
}

fn exactly_one_local(role: &str, candidates: &[ResourceId]) -> Result<ResourceId, SpatialRefusal> {
    let [terminal] = candidates else {
        return Err(selector_count_error(role, candidates.len()));
    };
    match terminal {
        ResourceId::Local { .. } => Ok(terminal.clone()),
        ResourceId::Satellite { .. } => Err(SpatialRefusal::new(
            codes::SATELLITE_TARGET,
            format!("{role} must resolve to a local pane; satellite panes are not supported"),
            "pick a hub-local pane for layout edits",
            2,
        )),
    }
}

fn selector_count_error(role: &str, matched: usize) -> SpatialRefusal {
    if matched == 0 {
        return SpatialRefusal::new(
            codes::SELECTOR_MISS,
            format!("{role} selector matched no panes"),
            "run `phux ls` to see live sessions and panes",
            2,
        );
    }
    SpatialRefusal::new(
        codes::SELECTOR_NOT_SINGLE,
        format!("{role} selector matched {matched} panes; use an exact pane selector"),
        "address exactly one pane, e.g. @N or session:window.pane",
        2,
    )
}

fn same_session(
    snapshot: &SessionSnapshot,
    terminals: &[ResourceId],
) -> Result<SessionId, SpatialRefusal> {
    let unknown_session = |terminal: &ResourceId| {
        SpatialRefusal::new(
            codes::UNKNOWN_TERMINAL_SESSION,
            format!(
                "cannot determine the session containing {}",
                format_terminal_id(terminal)
            ),
            "run `phux ls` to see live sessions and panes",
            2,
        )
    };
    let Some(first) = terminals.first() else {
        return Err(internal_error("no pane selectors"));
    };
    let session = session_for(snapshot, first).ok_or_else(|| unknown_session(first))?;
    for terminal in &terminals[1..] {
        let other = session_for(snapshot, terminal).ok_or_else(|| unknown_session(terminal))?;
        if other != session {
            return Err(SpatialRefusal::new(
                codes::CROSS_SESSION,
                "all panes in a spatial operation must belong to the same session",
                "pick panes from one session (`phux ls` shows the grouping)",
                2,
            ));
        }
    }
    Ok(session)
}

fn session_for(snapshot: &SessionSnapshot, terminal: &ResourceId) -> Option<SessionId> {
    let window = window_for(snapshot, terminal)?;
    snapshot
        .windows
        .iter()
        .find(|candidate| candidate.id == window)
        .map(|candidate| candidate.session_id)
}

fn window_for(snapshot: &SessionSnapshot, terminal: &ResourceId) -> Option<WindowId> {
    snapshot
        .resources
        .iter()
        .find(|pane| &pane.id == terminal)
        .map(|pane| pane.window_id)
}

fn local_id(terminal: &ResourceId) -> u32 {
    terminal.local_id().unwrap_or(0)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use super::*;
    use phux_protocol::ids::SatelliteHost;
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, WindowInfo};

    fn snapshot() -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(10), ResourceId::local(1))
            .with_sessions(vec![
                SessionInfo::new(SessionId::new(1), "one"),
                SessionInfo::new(SessionId::new(2), "two"),
            ])
            .with_windows(vec![
                WindowInfo::new(WindowId::new(10), SessionId::new(1), "a"),
                WindowInfo::new(WindowId::new(20), SessionId::new(2), "b"),
            ])
            .with_resources(vec![
                ResourceInfo::new(ResourceId::local(1), WindowId::new(10), 80, 24),
                ResourceInfo::new(ResourceId::local(2), WindowId::new(10), 80, 24),
                ResourceInfo::new(ResourceId::local(3), WindowId::new(20), 80, 24),
            ])
    }

    fn local(plan: PlanKind) -> Plan {
        match plan {
            PlanKind::Local(plan) => plan,
            PlanKind::CrossMove(other) => panic!("expected a local plan, got {other:?}"),
        }
    }

    async fn plan(op: SpatialOp) -> Result<PlanKind, SpatialRefusal> {
        let path = Path::new("/unused-for-local-selectors");
        let selectors = op.parse_selectors().unwrap();
        build_plan(path, &snapshot(), op, selectors).await
    }

    #[tokio::test]
    async fn cross_session_move_takes_the_shared_l1_path() {
        // @1 (session 1) -> beside @3 (session 2): the plan switches to the
        // shared MOVE_RESOURCE path.
        let op = SpatialOp::Move {
            source: "@1".to_owned(),
            target: "@3".to_owned(),
            direction: Direction::Horizontal,
            ratio: 0.5,
            projection: Vec::new(),
        };
        match plan(op).await.unwrap() {
            PlanKind::CrossMove(plan) => {
                assert_eq!(plan.source, ResourceId::local(1));
                assert_eq!(plan.target, ResourceId::local(3));
                assert_eq!(plan.outcome.document["cross_session"], true);
            }
            PlanKind::Local(other) => panic!("expected a cross-session plan, got {other:?}"),
        }

        // Insert and swap keep the same-session requirement.
        let op = SpatialOp::Insert {
            target: "@1".to_owned(),
            new_pane: "@3".to_owned(),
            direction: Direction::Horizontal,
            ratio: 0.5,
            projection: Vec::new(),
        };
        assert_eq!(plan(op).await.unwrap_err().code, "cross_session");
    }

    #[test]
    fn ratio_must_be_finite_and_strictly_inside_unit_interval() {
        assert!(validate_ratio(0.3).is_ok());
        for ratio in [0.0, 1.0, -0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert_eq!(validate_ratio(ratio).unwrap_err().code, "invalid_ratio");
        }
    }

    #[test]
    fn selectors_must_resolve_to_exactly_one_local_terminal() {
        assert_eq!(
            exactly_one_local("target", &[]).unwrap_err().code,
            "selector_miss"
        );
        assert_eq!(
            exactly_one_local("target", &[ResourceId::local(1), ResourceId::local(2)])
                .unwrap_err()
                .code,
            "selector_not_single"
        );
        let satellite = ResourceId::satellite(SatelliteHost::new("edge"), 7);
        assert_eq!(
            exactly_one_local("target", &[satellite]).unwrap_err().code,
            "satellite_target"
        );
        assert_eq!(
            exactly_one_local("target", &[ResourceId::local(7)]).unwrap(),
            ResourceId::local(7)
        );
    }

    #[test]
    fn panes_must_belong_to_one_session() {
        let snapshot = snapshot();
        assert_eq!(
            same_session(&snapshot, &[ResourceId::local(1), ResourceId::local(2)]).unwrap(),
            SessionId::new(1)
        );
        assert_eq!(
            same_session(&snapshot, &[ResourceId::local(1), ResourceId::local(3)])
                .unwrap_err()
                .code,
            "cross_session"
        );
    }

    #[tokio::test]
    async fn plans_map_arguments_to_all_layout_mutations() {
        let plan_insert = local(
            plan(SpatialOp::Insert {
                target: "@1".to_owned(),
                new_pane: "@2".to_owned(),
                direction: Direction::Vertical,
                ratio: 0.3,
                projection: Vec::new(),
            })
            .await
            .unwrap(),
        );
        assert!(matches!(
            plan_insert.mutation,
            LayoutMutation::Split {
                target,
                new_pane,
                dir: SplitDir::Horizontal,
                ratio,
            } if target == ResourceId::local(1)
                && new_pane == ResourceId::local(2)
                && (ratio - 0.3).abs() < f32::EPSILON
        ));
        assert_eq!(plan_insert.outcome.document["schema_version"], 1);
        assert_eq!(plan_insert.outcome.document["operation"], "insert-pane");
        assert_eq!(
            plan_insert.outcome.document["direction"], "vertical",
            "JSON retains the user-facing divider label"
        );

        let plan_move = local(
            plan(SpatialOp::Move {
                source: "@1".to_owned(),
                target: "@2".to_owned(),
                direction: Direction::Horizontal,
                ratio: 0.5,
                projection: Vec::new(),
            })
            .await
            .unwrap(),
        );
        assert!(matches!(
            plan_move.mutation,
            LayoutMutation::Move {
                dir: SplitDir::Vertical,
                ..
            }
        ));
        assert_eq!(
            plan_move.outcome.document["direction"], "horizontal",
            "JSON retains the user-facing divider label"
        );

        let plan_swap = local(
            plan(SpatialOp::Swap {
                first: "@1".to_owned(),
                second: "@2".to_owned(),
                projection: Vec::new(),
            })
            .await
            .unwrap(),
        );
        assert!(matches!(plan_swap.mutation, LayoutMutation::Swap { .. }));

        let same = SpatialOp::Swap {
            first: "@1".to_owned(),
            second: "@1".to_owned(),
            projection: Vec::new(),
        };
        assert_eq!(plan(same).await.unwrap_err().code, "same_pane");
    }

    /// Every refusal carries a remedy and a preflight exit code.
    #[test]
    fn refusals_carry_a_remedy_and_an_exit_code() {
        let refusal = same_pane_error();
        assert_eq!(refusal.code, "same_pane");
        assert!(!refusal.remedy.is_empty());
        assert_eq!(refusal.exit_code, 2);
        assert_eq!(move_refusal(&PaneMoveError::ServerTooOld).exit_code, 1);
        assert_eq!(
            move_refusal(&PaneMoveError::ProjectionArityMismatch).exit_code,
            2
        );
    }
}
