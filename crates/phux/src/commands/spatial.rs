//! Existing-pane layout edits over the shared L3 workspace envelope.
//!
//! These verbs never spawn a Terminal. `insert-pane` requires a Terminal that
//! already exists in the same session but is not yet present in its persisted
//! layout; implicit spawn-and-place remains a separate placement concern. All
//! selectors must resolve to exactly one local Terminal. The resulting
//! metadata write changes topology only: attached clients preserve their own
//! focus while reconciling it (ADR-0049).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_client::layout::{LayoutNode, SplitDir, Workspace, leaves};
use phux_client::layout_ops::{
    DEFAULT_LAYOUT_GROUP_ID, LayoutMutation, LayoutOps, LayoutOpsError, layout_key,
};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, Layer, LayerSet, ServerFeature};
use phux_protocol::ids::{SessionId, TerminalId, WindowId};
use phux_protocol::wire::frame::{
    Command as WireCommand, CommandResult, CommandValue, FrameKind, MoveError, MoveResult, Scope,
    StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::runtime::default_socket_path;

use crate::commands::json_err::{self, CliError, codes};
use crate::commands::{SpawnSplit, cli_runtime, command_on, resolve_targets};
use crate::selector;

const JSON_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Horizontal,
    Vertical,
}

impl From<SpawnSplit> for Direction {
    fn from(split: SpawnSplit) -> Self {
        match split {
            SpawnSplit::Horizontal => Self::Horizontal,
            SpawnSplit::Vertical => Self::Vertical,
        }
    }
}

/// Resolve the unified `--split` axis against the hidden deprecated boolean
/// spellings (`--horizontal` / `--vertical`).
///
/// The booleans conflict with an explicit `--split` at the clap level, so
/// when one is set, `split` necessarily holds its default and the boolean
/// wins. Returns the direction plus at most one deprecation line for the
/// caller to print on stderr — pinned to exactly one line so scripts see a
/// single, greppable warning (phux-i0e8.8.4).
pub(crate) fn resolve_split(
    split: SpawnSplit,
    horizontal: bool,
    vertical: bool,
) -> (Direction, Option<String>) {
    if vertical {
        (
            Direction::Vertical,
            Some(deprecated_split_flag_line("--vertical", "vertical")),
        )
    } else if horizontal {
        (
            Direction::Horizontal,
            Some(deprecated_split_flag_line("--horizontal", "horizontal")),
        )
    } else {
        (split.into(), None)
    }
}

/// The one-line warning for a deprecated boolean split flag.
fn deprecated_split_flag_line(flag: &str, value: &str) -> String {
    format!(
        "phux: {flag} is deprecated and will be removed; use `--split {value}` (or `--split {short}`)",
        short = &value[..1]
    )
}

impl Direction {
    /// Map the user-facing divider direction onto the internal child axis.
    /// A horizontal divider stacks panes (`SplitDir::Vertical`); a vertical
    /// divider places them side-by-side (`SplitDir::Horizontal`).
    const fn wire(self) -> SplitDir {
        match self {
            Self::Horizontal => SplitDir::Vertical,
            Self::Vertical => SplitDir::Horizontal,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
        }
    }
}

#[derive(Debug)]
enum RequestedOperation {
    Insert {
        target: String,
        new_pane: String,
        direction: Direction,
        ratio: f32,
    },
    Move {
        source: String,
        target: String,
        direction: Direction,
        ratio: f32,
    },
    Swap {
        first: String,
        second: String,
    },
}

#[derive(Debug)]
struct Plan {
    session: SessionId,
    mutation: LayoutMutation,
    output: serde_json::Value,
    human: String,
}

/// A move whose source and destination panes live in different sessions
/// (ADR-0056): ownership moves on L1 via `MOVE_TERMINAL`, then geometry is
/// written client-side — a `Split` into the destination envelope and a
/// `Close` out of the source envelope.
#[derive(Debug)]
struct CrossMovePlan {
    source: TerminalId,
    target: TerminalId,
    source_window: WindowId,
    dest_window: WindowId,
    source_session: SessionId,
    dest_session: SessionId,
    dir: SplitDir,
    ratio: f32,
    /// A surviving sibling in the source window, the ownership address the
    /// inverse `MOVE_TERMINAL` needs if the destination layout write fails.
    /// `None` when the source pane was its window's only leaf — rollback is
    /// then impossible (the emptied window is reaped server-side) and the
    /// failure is reported instead (ADR-0056: best-effort).
    rollback_owner: Option<TerminalId>,
    output: serde_json::Value,
    human: String,
}

#[derive(Debug)]
enum PlanKind {
    Local(Plan),
    CrossMove(CrossMovePlan),
}

/// Insert an already-created pane beside `target`.
pub(crate) fn run_insert_pane(
    target: &str,
    new_pane: &str,
    direction: Direction,
    ratio: f32,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run(
        RequestedOperation::Insert {
            target: target.to_owned(),
            new_pane: new_pane.to_owned(),
            direction,
            ratio,
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
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run(
        RequestedOperation::Move {
            source: source.to_owned(),
            target: target.to_owned(),
            direction,
            ratio,
        },
        json,
        socket,
    )
}

/// Exchange two existing pane leaves in one session layout.
pub(crate) fn run_swap_pane(
    first: &str,
    second: &str,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run(
        RequestedOperation::Swap {
            first: first.to_owned(),
            second: second.to_owned(),
        },
        json,
        socket,
    )
}

fn run(operation: RequestedOperation, json: bool, socket: Option<PathBuf>) -> ExitCode {
    if let Some(ratio) = operation.ratio()
        && let Err(err) = validate_ratio(ratio)
    {
        return json_err::emit(json, &err, 2);
    }
    let parsed = match operation.parse_selectors() {
        Ok(parsed) => parsed,
        Err(err) => return json_err::emit(json, &err, 2),
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => return json_err::report_no_server(json, &err, &socket_path, "layout"),
        };
        let snapshot = match read_snapshot(&mut conn, 0).await {
            Ok(snapshot) => snapshot,
            Err(err) => return json_err::report_no_server(json, &err, &socket_path, "layout"),
        };
        let plan = match build_plan(&socket_path, &snapshot, operation, parsed).await {
            Ok(plan) => plan,
            Err(err) => return json_err::emit(json, &err, 2),
        };
        match plan {
            PlanKind::Local(plan) => {
                let mut layout = LayoutOps::new(&mut conn, plan.session, 100);
                match layout.mutate(plan.mutation.clone()).await {
                    Ok(_) => print_success(json, &plan.output, &plan.human),
                    Err(err) => print_layout_error(json, &err, &socket_path),
                }
            }
            PlanKind::CrossMove(plan) => {
                execute_cross_move(&mut conn, &plan, json, &socket_path).await
            }
        }
    })
}

/// Execute a cross-session move (ADR-0056): feature-gate, re-parent on L1,
/// then write geometry — destination first, so a failed placement rolls
/// back with a single inverse `MOVE_TERMINAL` and no layout repair.
///
/// The source envelope's stale leaf is dropped last. If the move reaped the
/// source session, its one-leaf envelope is deleted instead; cleanup failures
/// are reported because the ownership move has already committed.
async fn execute_cross_move(
    conn: &mut Connection,
    plan: &CrossMovePlan,
    json: bool,
    socket_path: &Path,
) -> ExitCode {
    match server_supports_move(conn).await {
        Ok(true) => {}
        Ok(false) => {
            return json_err::emit(
                json,
                &CliError::new(
                    codes::SERVER_TOO_OLD,
                    "this server predates cross-session moves (MOVE_TERMINAL)",
                    "upgrade it with `phux upgrade`, then retry",
                ),
                1,
            );
        }
        Err(err) => return json_err::report_no_server(json, &err, socket_path, "layout"),
    }

    let frame = FrameKind::MoveTerminal {
        request_id: 200,
        terminal: plan.source.clone(),
        owner_terminal: plan.target.clone(),
    };
    let moved = match conn.request_move(&frame).await {
        Ok(reply) => reply.into_parts().0.unwrap_or_else(|refusal| {
            MoveResult::Err(MoveError::MoveFailed(format!(
                "server refused the move: {refusal}"
            )))
        }),
        Err(err) => return json_err::report_no_server(json, &err, socket_path, "layout"),
    };
    if let Err(err) = move_refusal(moved) {
        return json_err::emit(json, &err, 1);
    }

    // Reaping is authoritative server state, not a prediction from the
    // preflight snapshot: another client may have added or removed a pane
    // while the move and feature handshake were in flight.
    let post_move_snapshot = match read_snapshot(conn, 201).await {
        Ok(snapshot) => snapshot,
        Err(err) => {
            let suffix = rollback_suffix(conn, plan).await;
            return json_err::emit(
                json,
                &CliError::new(
                    codes::POST_MOVE_STATE_FAILED,
                    format!(
                        "the server moved the pane but its resulting ownership could not be read \
                         ({err}); {suffix}"
                    ),
                    "run `phux ls` to verify where the pane landed",
                ),
                1,
            );
        }
    };
    if !snapshot_confirms_destination(&post_move_snapshot, plan) {
        let suffix = rollback_suffix(conn, plan).await;
        return json_err::emit(
            json,
            &CliError::new(
                codes::DESTINATION_CHANGED,
                format!(
                    "the destination pane changed windows while the move was in flight; {suffix}"
                ),
                "re-run `phux ls` and retry with current selectors",
            ),
            1,
        );
    }
    let source_session_reaped = !post_move_snapshot
        .sessions
        .iter()
        .any(|session| session.id == plan.source_session);

    // Destination placement. On failure, ownership is restored with the
    // inverse move (best-effort — the spawn-placement rollback shape).
    if let Err(err) = publish_destination_layout(conn, plan).await {
        let suffix = rollback_suffix(conn, plan).await;
        return json_err::emit(
            json,
            &CliError::new(
                codes::DESTINATION_LAYOUT_FAILED,
                format!("destination layout write failed ({err}); {suffix}"),
                "run `phux ls` to verify pane ownership, then retry the move",
            ),
            1,
        );
    }

    // Drop the stale leaf from the source envelope. The final pane cannot be
    // represented as an empty Workspace, and its session was reaped by the
    // ownership move, so delete that dead session's envelope instead.
    if let Err(err) = cleanup_source_layout(conn, plan, source_session_reaped).await {
        return json_err::emit(
            json,
            &CliError::new(
                codes::SOURCE_LAYOUT_FAILED,
                format!(
                    "the pane was moved and placed, but the source layout could not be cleaned up \
                     ({err})"
                ),
                "retry the layout edit before relying on either session's topology",
            ),
            1,
        );
    }

    print_success(json, &plan.output, &plan.human)
}

async fn publish_destination_layout(
    conn: &mut Connection,
    plan: &CrossMovePlan,
) -> Result<(), String> {
    let placement = LayoutMutation::Split {
        target: plan.target.clone(),
        new_pane: plan.source.clone(),
        dir: plan.dir,
        ratio: plan.ratio,
    };
    match LayoutOps::new(conn, plan.dest_session, 202)
        .mutate(placement)
        .await
    {
        Ok(workspace)
            if workspace_has_placement(
                &workspace,
                &plan.target,
                &plan.source,
                plan.dir,
                plan.ratio,
            ) =>
        {
            Ok(())
        }
        Ok(_) => Err(
            "a concurrent layout writer replaced the requested destination placement during \
             confirmation"
                .to_owned(),
        ),
        Err(err) => Err(err.to_string()),
    }
}

async fn cleanup_source_layout(
    conn: &mut Connection,
    plan: &CrossMovePlan,
    source_session_reaped: bool,
) -> Result<(), LayoutOpsError> {
    if source_session_reaped {
        return delete_layout(conn, plan.source_session, 206).await;
    }

    match LayoutOps::new(conn, plan.source_session, 206)
        .mutate(LayoutMutation::Close {
            target: plan.source.clone(),
        })
        .await
    {
        Ok(workspace) if !workspace_contains(&workspace, &plan.source) => Ok(()),
        Ok(_) => Err(LayoutOpsError::Refused(
            "a concurrent layout writer restored the source leaf during confirmation".to_owned(),
        )),
        Err(err) => Err(err),
    }
}

/// The cross-session plan, when `operation` is a move whose two panes
/// resolve to different sessions; `None` keeps the local same-session path.
fn cross_move_plan(
    snapshot: &SessionSnapshot,
    operation: &RequestedOperation,
    terminals: &[TerminalId],
) -> Option<PlanKind> {
    let RequestedOperation::Move {
        direction, ratio, ..
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
        source_window: window_for(snapshot, source)?,
        dest_window: window_for(snapshot, target)?,
        source_session,
        dest_session,
        dir: direction.wire(),
        ratio,
        rollback_owner: sibling_in_window(snapshot, source),
        output: serde_json::json!({
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
        human: format!(
            "moved @{} beside @{} across sessions ({}, ratio {ratio})",
            local_id(source),
            local_id(target),
            direction.as_str(),
        ),
    }))
}

/// Fold a `TERMINAL_MOVED` result into the verb's error vocabulary.
/// `MoveResult` is `#[non_exhaustive]`; a future variant from a newer server
/// reads as a refusal rather than a silent success.
fn move_refusal(moved: MoveResult) -> Result<(), CliError> {
    match moved {
        MoveResult::Ok(_) => Ok(()),
        MoveResult::Err(MoveError::UnsupportedSatelliteRoute) => Err(CliError::new(
            codes::SATELLITE_TARGET,
            "cross-session moves are local-only; satellite panes are not supported",
            "pick a hub-local pane for layout edits",
        )),
        MoveResult::Err(err) => Err(CliError::new(
            codes::MOVE_REFUSED,
            err_text(&err),
            "run `phux ls` to re-check both panes, then retry",
        )),
        other => Err(CliError::new(
            codes::MOVE_REFUSED,
            format!("unrecognized move result: {other:?}"),
            "run `phux ls` to re-check both panes, then retry",
        )),
    }
}

/// Best-effort inverse `MOVE_TERMINAL` after a failed destination layout
/// write; `true` when ownership was restored. `rollback_owner = None` means
/// the emptied source window was reaped — there is no ownership address to
/// move back to.
async fn rollback_move(conn: &mut Connection, plan: &CrossMovePlan) -> bool {
    let Some(owner) = &plan.rollback_owner else {
        return false;
    };
    let owner_is_still_in_source = read_snapshot(conn, 209)
        .await
        .is_ok_and(|snapshot| window_for(&snapshot, owner) == Some(plan.source_window));
    if !owner_is_still_in_source {
        return false;
    }
    let inverse = FrameKind::MoveTerminal {
        request_id: 205,
        terminal: plan.source.clone(),
        owner_terminal: owner.clone(),
    };
    let moved = conn
        .request_move(&inverse)
        .await
        .is_ok_and(|reply| matches!(reply.into_parts().0, Ok(MoveResult::Ok(_))));
    if !moved {
        return false;
    }
    read_snapshot(conn, 210)
        .await
        .is_ok_and(|snapshot| window_for(&snapshot, &plan.source) == Some(plan.source_window))
}

async fn rollback_suffix(conn: &mut Connection, plan: &CrossMovePlan) -> &'static str {
    if rollback_move(conn, plan).await {
        "the pane was moved back to its original window"
    } else {
        "the pane's current ownership could not be restored; inspect it with `phux ls` and \
         place it with `phux insert-pane`"
    }
}

fn workspace_contains(workspace: &Workspace, terminal: &TerminalId) -> bool {
    workspace
        .windows
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .any(|tree| leaves(tree).contains(terminal))
}

fn workspace_has_placement(
    workspace: &Workspace,
    target: &TerminalId,
    moved: &TerminalId,
    dir: SplitDir,
    ratio: f32,
) -> bool {
    workspace
        .windows
        .iter()
        .filter_map(|window| window.state.tree.as_ref())
        .any(|tree| tree_has_placement(tree, target, moved, dir, ratio))
}

fn snapshot_confirms_destination(snapshot: &SessionSnapshot, plan: &CrossMovePlan) -> bool {
    window_for(snapshot, &plan.source) == Some(plan.dest_window)
        && window_for(snapshot, &plan.target) == Some(plan.dest_window)
        && session_for(snapshot, &plan.source) == Some(plan.dest_session)
        && session_for(snapshot, &plan.target) == Some(plan.dest_session)
}

fn tree_has_placement(
    node: &LayoutNode,
    target: &TerminalId,
    moved: &TerminalId,
    expected_dir: SplitDir,
    expected_ratio: f32,
) -> bool {
    match node {
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => {
            (*dir == expected_dir
                && ratio.to_bits() == expected_ratio.to_bits()
                && matches!(left.as_ref(), LayoutNode::Leaf(id) if id == target)
                && matches!(right.as_ref(), LayoutNode::Leaf(id) if id == moved))
                || tree_has_placement(left, target, moved, expected_dir, expected_ratio)
                || tree_has_placement(right, target, moved, expected_dir, expected_ratio)
        }
        _ => false,
    }
}

async fn delete_layout(
    conn: &mut Connection,
    session: SessionId,
    request_id: u32,
) -> Result<(), LayoutOpsError> {
    conn.send(&FrameKind::DeleteMetadata {
        request_id,
        scope: Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
        key: layout_key(session),
    })
    .await?;

    match LayoutOps::new(conn, session, request_id.wrapping_add(1))
        .read()
        .await
    {
        Err(LayoutOpsError::MissingLayout) => Ok(()),
        Ok(_) => Err(LayoutOpsError::Refused(
            "source layout still exists after deletion".to_owned(),
        )),
        Err(err) => Err(err),
    }
}

fn err_text(err: &MoveError) -> String {
    match err {
        MoveError::MoveFailed(msg) => msg.clone(),
        MoveError::UnsupportedSatelliteRoute => "satellite panes are not supported".to_owned(),
        _ => format!("{err:?}"),
    }
}

/// Whether the server advertises the `MOVE_TERMINAL` feature bit.
///
/// The CLI's UDS connection is tolerated HELLO-less, so no capabilities were
/// exchanged yet: send the HELLO now and read the `HELLO_OK` it must answer
/// with. An old server that lacks the bit would otherwise silently drop the
/// unknown `MOVE_TERMINAL` discriminant and hang the caller forever.
async fn server_supports_move(conn: &mut Connection) -> Result<bool, AttachError> {
    conn.send(&FrameKind::Hello {
        client_name: format!("phux-cli/{}", env!("CARGO_PKG_VERSION")),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::new().with_layers(LayerSet::with(&[Layer::L3])),
    })
    .await?;
    // Nothing is attached or subscribed on this connection, so HELLO_OK is
    // the next frame; the bound is a guard against a misbehaving peer.
    for _ in 0..32 {
        if let FrameKind::HelloOk { server_caps, .. } = conn.recv().await? {
            return Ok(server_caps.features.contains(ServerFeature::MoveTerminal));
        }
    }
    Err(AttachError::Protocol(
        "server did not answer HELLO with HELLO_OK".to_owned(),
    ))
}

/// Another pane sharing `terminal`'s window, if any — the inverse move's
/// ownership address.
fn sibling_in_window(snapshot: &SessionSnapshot, terminal: &TerminalId) -> Option<TerminalId> {
    let window = snapshot
        .panes
        .iter()
        .find(|pane| &pane.id == terminal)?
        .window_id;
    snapshot
        .panes
        .iter()
        .find(|pane| pane.window_id == window && &pane.id != terminal)
        .map(|pane| pane.id.clone())
}

impl RequestedOperation {
    const fn ratio(&self) -> Option<f32> {
        match self {
            Self::Insert { ratio, .. } | Self::Move { ratio, .. } => Some(*ratio),
            Self::Swap { .. } => None,
        }
    }

    fn parse_selectors(&self) -> Result<Vec<selector::Selector>, CliError> {
        self.raw_selectors()
            .into_iter()
            .map(|(role, raw)| {
                selector::parse(raw).map_err(|err| {
                    CliError::new(
                        codes::INVALID_SELECTOR,
                        format!("invalid {role} selector {raw:?}: {err}"),
                        "selector grammar: session, session:window, session:window.pane, @id, `.`",
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
            Self::Swap { first, second } => vec![("first", first), ("second", second)],
        }
    }
}

async fn read_snapshot(
    conn: &mut Connection,
    request_id: u32,
) -> Result<SessionSnapshot, AttachError> {
    match command_on(
        conn,
        request_id,
        WireCommand::GetState {
            scope: StateScope::Server,
        },
    )
    .await?
    {
        CommandResult::OkWith(CommandValue::State(snapshot)) => Ok(snapshot),
        other => Err(AttachError::Protocol(
            phux_client::explain::explain_unexpected("GET_STATE", &other),
        )),
    }
}

async fn build_plan(
    socket_path: &Path,
    snapshot: &SessionSnapshot,
    operation: RequestedOperation,
    selectors: Vec<selector::Selector>,
) -> Result<PlanKind, CliError> {
    let roles = operation.raw_selectors();
    let mut terminals = Vec::with_capacity(selectors.len());
    for ((role, _), selector) in roles.iter().zip(&selectors) {
        let candidates = resolve_targets(socket_path, selector, snapshot).await;
        terminals.push(exactly_one_local(role, &candidates)?);
    }
    if terminals.len() == 2 && terminals[0] == terminals[1] {
        return Err(same_pane_error());
    }

    // Cross-session move (ADR-0056): the one spatial operation that may span
    // sessions. Ownership moves on L1 via MOVE_TERMINAL; the two layout
    // writes stay client-side. Every other operation keeps the same-session
    // requirement below.
    if let Some(plan) = cross_move_plan(snapshot, &operation, &terminals) {
        return Ok(plan);
    }

    let session = same_session(snapshot, &terminals)?;

    match (operation, terminals.as_slice()) {
        (
            RequestedOperation::Insert {
                direction, ratio, ..
            },
            [target, new_pane],
        ) => Ok(PlanKind::Local(Plan {
            session,
            mutation: LayoutMutation::Split {
                target: target.clone(),
                new_pane: new_pane.clone(),
                dir: direction.wire(),
                ratio,
            },
            output: serde_json::json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "insert-pane",
                "session_id": session.get(),
                "target_terminal_id": local_id(target),
                "new_terminal_id": local_id(new_pane),
                "direction": direction.as_str(),
                "ratio": ratio,
            }),
            human: format!(
                "inserted @{} beside @{} ({}, ratio {ratio})",
                local_id(new_pane),
                local_id(target),
                direction.as_str(),
            ),
        })),
        (
            RequestedOperation::Move {
                direction, ratio, ..
            },
            [source, target],
        ) => Ok(PlanKind::Local(Plan {
            session,
            mutation: LayoutMutation::Move {
                source: source.clone(),
                target: target.clone(),
                dir: direction.wire(),
                ratio,
            },
            output: serde_json::json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "move-pane",
                "session_id": session.get(),
                "source_terminal_id": local_id(source),
                "target_terminal_id": local_id(target),
                "direction": direction.as_str(),
                "ratio": ratio,
            }),
            human: format!(
                "moved @{} beside @{} ({}, ratio {ratio})",
                local_id(source),
                local_id(target),
                direction.as_str(),
            ),
        })),
        (RequestedOperation::Swap { .. }, [first, second]) => Ok(PlanKind::Local(Plan {
            session,
            mutation: LayoutMutation::Swap {
                first: first.clone(),
                second: second.clone(),
            },
            output: serde_json::json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "swap-pane",
                "session_id": session.get(),
                "first_terminal_id": local_id(first),
                "second_terminal_id": local_id(second),
            }),
            human: format!("swapped @{} and @{}", local_id(first), local_id(second)),
        })),
        _ => Err(CliError::new(
            codes::INTERNAL_ERROR,
            "spatial operation argument mismatch",
            "this is a phux bug; run `phux doctor` and report it",
        )),
    }
}

/// The shared "two selectors, one pane" refusal (raised both client-side and
/// by the server's layout engine).
fn same_pane_error() -> CliError {
    CliError::new(
        codes::SAME_PANE,
        "the two pane selectors must resolve differently",
        "pass two selectors that name different panes (`phux ls` lists them)",
    )
}

fn validate_ratio(ratio: f32) -> Result<(), CliError> {
    if ratio.is_finite() && ratio > 0.0 && ratio < 1.0 {
        Ok(())
    } else {
        Err(CliError::new(
            codes::INVALID_RATIO,
            format!("ratio must be finite and strictly between 0 and 1; got {ratio}"),
            "pass e.g. --ratio 0.5",
        ))
    }
}

fn exactly_one_local(role: &str, candidates: &[TerminalId]) -> Result<TerminalId, CliError> {
    let [terminal] = candidates else {
        let err = if candidates.is_empty() {
            CliError::new(
                codes::SELECTOR_MISS,
                format!("{role} selector matched no panes"),
                "run `phux ls` to see live sessions and panes",
            )
        } else {
            CliError::new(
                codes::SELECTOR_NOT_SINGLE,
                format!(
                    "{role} selector matched {} panes; use an exact pane selector",
                    candidates.len()
                ),
                "address exactly one pane, e.g. @N or session:window.pane",
            )
        };
        return Err(err);
    };
    match terminal {
        TerminalId::Local { .. } => Ok(terminal.clone()),
        TerminalId::Satellite { .. } => Err(CliError::new(
            codes::SATELLITE_TARGET,
            format!("{role} must resolve to a local pane; satellite panes are not supported"),
            "pick a hub-local pane for layout edits",
        )),
    }
}

fn same_session(
    snapshot: &SessionSnapshot,
    terminals: &[TerminalId],
) -> Result<SessionId, CliError> {
    let unknown_session = |terminal: &TerminalId| {
        CliError::new(
            codes::UNKNOWN_TERMINAL_SESSION,
            format!(
                "cannot determine the session containing {}",
                crate::selector::format_terminal_id(terminal)
            ),
            "run `phux ls` to see live sessions and panes",
        )
    };
    let Some(first) = terminals.first() else {
        return Err(CliError::new(
            codes::INTERNAL_ERROR,
            "no pane selectors",
            "this is a phux bug; run `phux doctor` and report it",
        ));
    };
    let session = session_for(snapshot, first).ok_or_else(|| unknown_session(first))?;
    for terminal in &terminals[1..] {
        let other = session_for(snapshot, terminal).ok_or_else(|| unknown_session(terminal))?;
        if other != session {
            return Err(CliError::new(
                codes::CROSS_SESSION,
                "all panes in a spatial operation must belong to the same session",
                "pick panes from one session (`phux ls` shows the grouping)",
            ));
        }
    }
    Ok(session)
}

fn session_for(snapshot: &SessionSnapshot, terminal: &TerminalId) -> Option<SessionId> {
    let window = window_for(snapshot, terminal)?;
    snapshot
        .windows
        .iter()
        .find(|candidate| candidate.id == window)
        .map(|candidate| candidate.session_id)
}

fn window_for(snapshot: &SessionSnapshot, terminal: &TerminalId) -> Option<WindowId> {
    snapshot
        .panes
        .iter()
        .find(|pane| &pane.id == terminal)
        .map(|pane| pane.window_id)
}

fn local_id(terminal: &TerminalId) -> u32 {
    terminal.local_id().unwrap_or(0)
}

fn print_success(json: bool, output: &serde_json::Value, human: &str) -> ExitCode {
    if json {
        match serde_json::to_string_pretty(output) {
            Ok(rendered) => outln!("{rendered}"),
            Err(err) => {
                return json_err::emit(
                    true,
                    &CliError::new(
                        codes::JSON_SERIALIZE,
                        err.to_string(),
                        "this is a phux bug; run `phux doctor` and report it",
                    ),
                    1,
                );
            }
        }
    } else {
        outln!("{human}");
    }
    ExitCode::SUCCESS
}

fn print_layout_error(json: bool, err: &LayoutOpsError, socket_path: &Path) -> ExitCode {
    match err {
        LayoutOpsError::Transport(transport) => {
            json_err::report_no_server(json, transport, socket_path, "layout")
        }
        LayoutOpsError::MissingLayout => json_err::emit(
            json,
            &CliError::new(
                codes::LAYOUT_MISSING,
                "session has no persisted layout; attach a TUI before editing topology",
                "attach once with `phux attach SESSION` to seed the layout, then retry",
            ),
            2,
        ),
        LayoutOpsError::ForeignTarget(_) => json_err::emit(
            json,
            &CliError::new(
                codes::PANE_NOT_IN_LAYOUT,
                "a selected pane is not present in this session's persisted layout",
                "insert it first with `phux insert-pane`",
            ),
            2,
        ),
        LayoutOpsError::DuplicatePane(_) => json_err::emit(
            json,
            &CliError::new(
                codes::PANE_ALREADY_IN_LAYOUT,
                "the pane being inserted is already present in the persisted layout",
                "use `phux move-pane` to relocate a pane the layout already holds",
            ),
            2,
        ),
        LayoutOpsError::SamePane => json_err::emit(json, &same_pane_error(), 2),
        other => json_err::emit(
            json,
            &CliError::new(
                codes::LAYOUT_REJECTED,
                other.to_string(),
                "run `phux doctor` for a health check",
            ),
            2,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::ids::{SatelliteHost, WindowId};
    use phux_protocol::wire::info::{SessionInfo, TerminalInfo, WindowInfo};

    fn snapshot() -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(10), TerminalId::local(1))
            .with_sessions(vec![
                SessionInfo::new(SessionId::new(1), "one"),
                SessionInfo::new(SessionId::new(2), "two"),
            ])
            .with_windows(vec![
                WindowInfo::new(WindowId::new(10), SessionId::new(1), "a"),
                WindowInfo::new(WindowId::new(20), SessionId::new(2), "b"),
            ])
            .with_panes(vec![
                TerminalInfo::new(TerminalId::local(1), WindowId::new(10), 80, 24),
                TerminalInfo::new(TerminalId::local(2), WindowId::new(10), 80, 24),
                TerminalInfo::new(TerminalId::local(3), WindowId::new(20), 80, 24),
            ])
    }

    #[tokio::test]
    async fn cross_session_move_takes_the_l1_path_and_records_rollback_owner() {
        let snapshot = snapshot();
        let path = Path::new("/unused-for-local-selectors");

        // @1 (session 1) -> beside @3 (session 2): the plan switches to the
        // MOVE_TERMINAL path, and @2 (the surviving sibling in @1's window)
        // is the inverse move's ownership address.
        let op = RequestedOperation::Move {
            source: "@1".to_owned(),
            target: "@3".to_owned(),
            direction: Direction::Horizontal,
            ratio: 0.5,
        };
        let selectors = op.parse_selectors().unwrap();
        match build_plan(path, &snapshot, op, selectors).await.unwrap() {
            PlanKind::CrossMove(plan) => {
                assert_eq!(plan.source, TerminalId::local(1));
                assert_eq!(plan.target, TerminalId::local(3));
                assert_eq!(plan.source_window, WindowId::new(10));
                assert_eq!(plan.dest_window, WindowId::new(20));
                assert_eq!(plan.source_session, SessionId::new(1));
                assert_eq!(plan.dest_session, SessionId::new(2));
                assert_eq!(plan.rollback_owner, Some(TerminalId::local(2)));
                assert_eq!(plan.output["cross_session"], true);
            }
            PlanKind::Local(other) => panic!("expected a cross-session plan, got {other:?}"),
        }

        // A solo source pane has no rollback owner: @3 is alone in window 20.
        let op = RequestedOperation::Move {
            source: "@3".to_owned(),
            target: "@1".to_owned(),
            direction: Direction::Horizontal,
            ratio: 0.5,
        };
        let selectors = op.parse_selectors().unwrap();
        match build_plan(path, &snapshot, op, selectors).await.unwrap() {
            PlanKind::CrossMove(plan) => assert_eq!(plan.rollback_owner, None),
            PlanKind::Local(other) => panic!("expected a cross-session plan, got {other:?}"),
        }

        // Insert and swap keep the same-session requirement.
        let op = RequestedOperation::Insert {
            target: "@1".to_owned(),
            new_pane: "@3".to_owned(),
            direction: Direction::Horizontal,
            ratio: 0.5,
        };
        let selectors = op.parse_selectors().unwrap();
        assert_eq!(
            build_plan(path, &snapshot, op, selectors)
                .await
                .unwrap_err()
                .code,
            "cross_session"
        );
    }

    #[test]
    fn ratio_must_be_finite_and_strictly_inside_unit_interval() {
        assert!(validate_ratio(0.3).is_ok());
        for ratio in [0.0, 1.0, -0.1, 1.1, f32::NAN, f32::INFINITY] {
            assert_eq!(validate_ratio(ratio).unwrap_err().code, "invalid_ratio");
        }
    }

    #[test]
    fn destination_confirmation_requires_the_requested_split() {
        let target = TerminalId::local(1);
        let moved = TerminalId::local(2);
        let expected = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.4,
            left: Box::new(LayoutNode::Leaf(target.clone())),
            right: Box::new(LayoutNode::Leaf(moved.clone())),
        };
        let workspace = Workspace {
            windows: vec![phux_client::layout::WindowState {
                name: "1".to_owned(),
                state: phux_client::layout::LayoutState {
                    tree: Some(expected),
                    focus: Some(moved.clone()),
                },
            }],
            active: 0,
        };

        assert!(workspace_has_placement(
            &workspace,
            &target,
            &moved,
            SplitDir::Horizontal,
            0.4,
        ));
        assert!(!workspace_has_placement(
            &workspace,
            &target,
            &moved,
            SplitDir::Vertical,
            0.4,
        ));
        assert!(!workspace_has_placement(
            &workspace,
            &target,
            &moved,
            SplitDir::Horizontal,
            0.6,
        ));
    }

    #[test]
    fn selectors_must_resolve_to_exactly_one_local_terminal() {
        assert_eq!(
            exactly_one_local("target", &[]).unwrap_err().code,
            "selector_miss"
        );
        assert_eq!(
            exactly_one_local("target", &[TerminalId::local(1), TerminalId::local(2)])
                .unwrap_err()
                .code,
            "selector_not_single"
        );
        let satellite = TerminalId::satellite(SatelliteHost::new("edge"), 7);
        assert_eq!(
            exactly_one_local("target", &[satellite]).unwrap_err().code,
            "satellite_target"
        );
        assert_eq!(
            exactly_one_local("target", &[TerminalId::local(7)]).unwrap(),
            TerminalId::local(7)
        );
    }

    #[test]
    fn panes_must_belong_to_one_session() {
        let snapshot = snapshot();
        assert_eq!(
            same_session(&snapshot, &[TerminalId::local(1), TerminalId::local(2)]).unwrap(),
            SessionId::new(1)
        );
        assert_eq!(
            same_session(&snapshot, &[TerminalId::local(1), TerminalId::local(3)])
                .unwrap_err()
                .code,
            "cross_session"
        );
    }

    fn local(plan: PlanKind) -> Plan {
        match plan {
            PlanKind::Local(plan) => plan,
            PlanKind::CrossMove(other) => panic!("expected a local plan, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn plans_map_cli_arguments_to_all_layout_mutations() {
        let snapshot = snapshot();
        let path = Path::new("/unused-for-local-selectors");

        let insert = RequestedOperation::Insert {
            target: "@1".to_owned(),
            new_pane: "@2".to_owned(),
            direction: Direction::Vertical,
            ratio: 0.3,
        };
        let selectors = insert.parse_selectors().unwrap();
        let plan = local(
            build_plan(path, &snapshot, insert, selectors)
                .await
                .unwrap(),
        );
        assert!(matches!(
            plan.mutation,
            LayoutMutation::Split {
                target,
                new_pane,
                dir: SplitDir::Horizontal,
                ratio,
            } if target == TerminalId::local(1)
                && new_pane == TerminalId::local(2)
                && (ratio - 0.3).abs() < f32::EPSILON
        ));
        assert_eq!(plan.output["schema_version"], 1);
        assert_eq!(plan.output["operation"], "insert-pane");
        assert_eq!(
            plan.output["direction"], "vertical",
            "JSON retains the user-facing divider label"
        );

        let move_pane = RequestedOperation::Move {
            source: "@1".to_owned(),
            target: "@2".to_owned(),
            direction: Direction::Horizontal,
            ratio: 0.5,
        };
        let selectors = move_pane.parse_selectors().unwrap();
        let plan = local(
            build_plan(path, &snapshot, move_pane, selectors)
                .await
                .unwrap(),
        );
        assert!(matches!(
            plan.mutation,
            LayoutMutation::Move {
                dir: SplitDir::Vertical,
                ..
            }
        ));
        assert_eq!(
            plan.output["direction"], "horizontal",
            "JSON retains the user-facing divider label"
        );

        let swap = RequestedOperation::Swap {
            first: "@1".to_owned(),
            second: "@2".to_owned(),
        };
        let selectors = swap.parse_selectors().unwrap();
        let plan = local(build_plan(path, &snapshot, swap, selectors).await.unwrap());
        assert!(matches!(plan.mutation, LayoutMutation::Swap { .. }));

        let same = RequestedOperation::Swap {
            first: "@1".to_owned(),
            second: "@1".to_owned(),
        };
        let selectors = same.parse_selectors().unwrap();
        assert_eq!(
            build_plan(path, &snapshot, same, selectors)
                .await
                .unwrap_err()
                .code,
            "same_pane"
        );
    }

    /// Spatial errors ride the shared emitter (phux-i0e8.8.2): same
    /// versioned shape as before, now with `remedy` and `exit_code` added.
    #[test]
    fn json_error_documents_are_versioned() {
        let error = json_err::error_document(&same_pane_error(), 2);
        assert_eq!(error["schema_version"], 1);
        assert_eq!(error["error"]["code"], "same_pane");
        assert!(
            error["remedy"].as_str().is_some_and(|r| !r.is_empty()),
            "spatial errors must carry a remedy: {error}"
        );
        assert_eq!(error["exit_code"], 2);
    }
}
