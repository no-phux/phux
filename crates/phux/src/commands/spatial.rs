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
use phux_client::layout::SplitDir;
use phux_client::layout_ops::{LayoutMutation, LayoutOps, LayoutOpsError};
use phux_protocol::ids::{ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult, CommandValue, StateScope};
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
/// (ADR-0056): ownership moves on L1 via `MOVE_RESOURCE`, then geometry is
/// written client-side — a `Split` into the destination envelope and a
/// `Close` out of the source envelope.
#[derive(Debug)]
struct CrossMovePlan {
    source: ResourceId,
    target: ResourceId,
    dir: SplitDir,
    ratio: f32,
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
/// back with a single inverse `MOVE_RESOURCE` and no layout repair.
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
    match phux_client::pane_move::move_pane(
        conn,
        plan.source.clone(),
        plan.target.clone(),
        plan.dir,
        plan.ratio,
    )
    .await
    {
        Ok(_) => print_success(json, &plan.output, &plan.human),
        Err(phux_client::pane_move::PaneMoveError::Transport(err)) => {
            json_err::report_no_server(json, &err, socket_path, "layout")
        }
        Err(error) => {
            let (code, remedy) = match &error {
                phux_client::pane_move::PaneMoveError::ServerTooOld => (
                    codes::SERVER_TOO_OLD,
                    "upgrade it with `phux upgrade`, then retry",
                ),
                phux_client::pane_move::PaneMoveError::SatellitePane => (
                    codes::SATELLITE_TARGET,
                    "pick a hub-local pane for layout edits",
                ),
                phux_client::pane_move::PaneMoveError::DestinationChanged { .. } => (
                    codes::DESTINATION_CHANGED,
                    "re-run `phux ls` and retry with current selectors",
                ),
                phux_client::pane_move::PaneMoveError::PostMoveState { .. } => (
                    codes::POST_MOVE_STATE_FAILED,
                    "run `phux ls` to verify where the pane landed",
                ),
                phux_client::pane_move::PaneMoveError::DestinationLayout { .. } => (
                    codes::DESTINATION_LAYOUT_FAILED,
                    "run `phux ls` to verify pane ownership, then retry the move",
                ),
                phux_client::pane_move::PaneMoveError::SourceLayout(_) => (
                    codes::SOURCE_LAYOUT_FAILED,
                    "retry the layout edit before relying on either session's topology",
                ),
                phux_client::pane_move::PaneMoveError::SamePane => (
                    codes::SAME_PANE,
                    "pass two selectors that name different panes",
                ),
                phux_client::pane_move::PaneMoveError::UnknownPane { .. } => (
                    codes::SELECTOR_MISS,
                    "run `phux ls` to see live sessions and panes",
                ),
                phux_client::pane_move::PaneMoveError::MoveRefused(_) => (
                    codes::MOVE_REFUSED,
                    "run `phux ls` to re-check both panes, then retry",
                ),
                phux_client::pane_move::PaneMoveError::Layout(_) => (
                    codes::LAYOUT_REJECTED,
                    "run `phux ls` to inspect the winning layout, then retry",
                ),
                phux_client::pane_move::PaneMoveError::Transport(_) => unreachable!(),
            };
            json_err::emit(json, &CliError::new(code, error.to_string(), remedy), 1)
        }
    }
}

/// The cross-session plan, when `operation` is a move whose two panes
/// resolve to different sessions; `None` keeps the local same-session path.
fn cross_move_plan(
    snapshot: &SessionSnapshot,
    operation: &RequestedOperation,
    terminals: &[ResourceId],
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
        dir: direction.wire(),
        ratio,
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
    // sessions. Ownership moves on L1 via MOVE_RESOURCE; the two layout
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

fn exactly_one_local(role: &str, candidates: &[ResourceId]) -> Result<ResourceId, CliError> {
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
        ResourceId::Local { .. } => Ok(terminal.clone()),
        ResourceId::Satellite { .. } => Err(CliError::new(
            codes::SATELLITE_TARGET,
            format!("{role} must resolve to a local pane; satellite panes are not supported"),
            "pick a hub-local pane for layout edits",
        )),
    }
}

fn same_session(
    snapshot: &SessionSnapshot,
    terminals: &[ResourceId],
) -> Result<SessionId, CliError> {
    let unknown_session = |terminal: &ResourceId| {
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

    #[tokio::test]
    async fn cross_session_move_takes_the_shared_l1_path() {
        let snapshot = snapshot();
        let path = Path::new("/unused-for-local-selectors");

        // @1 (session 1) -> beside @3 (session 2): the plan switches to the
        // shared MOVE_RESOURCE path.
        let op = RequestedOperation::Move {
            source: "@1".to_owned(),
            target: "@3".to_owned(),
            direction: Direction::Horizontal,
            ratio: 0.5,
        };
        let selectors = op.parse_selectors().unwrap();
        match build_plan(path, &snapshot, op, selectors).await.unwrap() {
            PlanKind::CrossMove(plan) => {
                assert_eq!(plan.source, ResourceId::local(1));
                assert_eq!(plan.target, ResourceId::local(3));
                assert_eq!(plan.output["cross_session"], true);
            }
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
            } if target == ResourceId::local(1)
                && new_pane == ResourceId::local(2)
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
