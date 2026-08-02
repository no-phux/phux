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
use phux_protocol::ids::{SessionId, TerminalId};
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult, CommandValue, StateScope};
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, command_on, report_no_server, resolve_targets};
use crate::selector;

const JSON_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Horizontal,
    Vertical,
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
struct SpatialError {
    code: &'static str,
    message: String,
}

impl SpatialError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
struct Plan {
    session: SessionId,
    mutation: LayoutMutation,
    output: serde_json::Value,
    human: String,
}

/// Insert an already-created pane beside `target`.
pub(crate) fn run_insert_pane(
    target: &str,
    new_pane: &str,
    vertical: bool,
    ratio: f32,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run(
        RequestedOperation::Insert {
            target: target.to_owned(),
            new_pane: new_pane.to_owned(),
            direction: if vertical {
                Direction::Vertical
            } else {
                Direction::Horizontal
            },
            ratio,
        },
        json,
        socket,
    )
}

/// Relocate an existing pane beside another pane in the same session.
pub(crate) fn run_move_pane(
    source: &str,
    target: &str,
    vertical: bool,
    ratio: f32,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    run(
        RequestedOperation::Move {
            source: source.to_owned(),
            target: target.to_owned(),
            direction: if vertical {
                Direction::Vertical
            } else {
                Direction::Horizontal
            },
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
        return print_error(json, &err, ExitCode::from(2));
    }
    let parsed = match operation.parse_selectors() {
        Ok(parsed) => parsed,
        Err(err) => return print_error(json, &err, ExitCode::from(2)),
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => return print_transport_error(json, &err, &socket_path),
        };
        let snapshot = match read_snapshot(&mut conn).await {
            Ok(snapshot) => snapshot,
            Err(err) => return print_transport_or_protocol_error(json, &err, &socket_path),
        };
        let plan = match build_plan(&socket_path, &snapshot, operation, parsed).await {
            Ok(plan) => plan,
            Err(err) => return print_error(json, &err, ExitCode::from(2)),
        };
        let mut layout = LayoutOps::new(&mut conn, plan.session, 100);
        match layout.mutate(plan.mutation.clone()).await {
            Ok(_) => print_success(json, &plan),
            Err(err) => print_layout_error(json, &err, &socket_path),
        }
    })
}

impl RequestedOperation {
    const fn ratio(&self) -> Option<f32> {
        match self {
            Self::Insert { ratio, .. } | Self::Move { ratio, .. } => Some(*ratio),
            Self::Swap { .. } => None,
        }
    }

    fn parse_selectors(&self) -> Result<Vec<selector::Selector>, SpatialError> {
        self.raw_selectors()
            .into_iter()
            .map(|(role, raw)| {
                selector::parse(raw).map_err(|err| {
                    SpatialError::new(
                        "invalid_selector",
                        format!("invalid {role} selector {raw:?}: {err}"),
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

async fn read_snapshot(conn: &mut Connection) -> Result<SessionSnapshot, AttachError> {
    match command_on(
        conn,
        0,
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
) -> Result<Plan, SpatialError> {
    let roles = operation.raw_selectors();
    let mut terminals = Vec::with_capacity(selectors.len());
    for ((role, _), selector) in roles.iter().zip(&selectors) {
        let candidates = resolve_targets(socket_path, selector, snapshot).await;
        terminals.push(exactly_one_local(role, &candidates)?);
    }
    let session = same_session(snapshot, &terminals)?;
    if terminals.len() == 2 && terminals[0] == terminals[1] {
        return Err(SpatialError::new(
            "same_pane",
            "the two pane selectors must resolve differently",
        ));
    }

    match (operation, terminals.as_slice()) {
        (
            RequestedOperation::Insert {
                direction, ratio, ..
            },
            [target, new_pane],
        ) => Ok(Plan {
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
        }),
        (
            RequestedOperation::Move {
                direction, ratio, ..
            },
            [source, target],
        ) => Ok(Plan {
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
        }),
        (RequestedOperation::Swap { .. }, [first, second]) => Ok(Plan {
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
        }),
        _ => Err(SpatialError::new(
            "internal_error",
            "spatial operation argument mismatch",
        )),
    }
}

fn validate_ratio(ratio: f32) -> Result<(), SpatialError> {
    if ratio.is_finite() && ratio > 0.0 && ratio < 1.0 {
        Ok(())
    } else {
        Err(SpatialError::new(
            "invalid_ratio",
            format!("ratio must be finite and strictly between 0 and 1; got {ratio}"),
        ))
    }
}

fn exactly_one_local(role: &str, candidates: &[TerminalId]) -> Result<TerminalId, SpatialError> {
    let [terminal] = candidates else {
        let (code, message) = if candidates.is_empty() {
            ("selector_miss", format!("{role} selector matched no panes"))
        } else {
            (
                "selector_not_single",
                format!(
                    "{role} selector matched {} panes; use an exact pane selector",
                    candidates.len()
                ),
            )
        };
        return Err(SpatialError::new(code, message));
    };
    match terminal {
        TerminalId::Local { .. } => Ok(terminal.clone()),
        TerminalId::Satellite { .. } => Err(SpatialError::new(
            "satellite_target",
            format!("{role} must resolve to a local pane; satellite panes are not supported"),
        )),
    }
}

fn same_session(
    snapshot: &SessionSnapshot,
    terminals: &[TerminalId],
) -> Result<SessionId, SpatialError> {
    let Some(first) = terminals.first() else {
        return Err(SpatialError::new("internal_error", "no pane selectors"));
    };
    let session = session_for(snapshot, first).ok_or_else(|| {
        SpatialError::new(
            "unknown_terminal_session",
            format!("cannot determine the session containing {first:?}"),
        )
    })?;
    for terminal in &terminals[1..] {
        let other = session_for(snapshot, terminal).ok_or_else(|| {
            SpatialError::new(
                "unknown_terminal_session",
                format!("cannot determine the session containing {terminal:?}"),
            )
        })?;
        if other != session {
            return Err(SpatialError::new(
                "cross_session",
                "all panes in a spatial operation must belong to the same session",
            ));
        }
    }
    Ok(session)
}

fn session_for(snapshot: &SessionSnapshot, terminal: &TerminalId) -> Option<SessionId> {
    let window = snapshot
        .panes
        .iter()
        .find(|pane| &pane.id == terminal)?
        .window_id;
    snapshot
        .windows
        .iter()
        .find(|candidate| candidate.id == window)
        .map(|candidate| candidate.session_id)
}

fn local_id(terminal: &TerminalId) -> u32 {
    terminal.local_id().unwrap_or(0)
}

fn print_success(json: bool, plan: &Plan) -> ExitCode {
    if json {
        match serde_json::to_string_pretty(&plan.output) {
            Ok(rendered) => outln!("{rendered}"),
            Err(err) => {
                return print_error(
                    true,
                    &SpatialError::new("json_serialize", err.to_string()),
                    ExitCode::FAILURE,
                );
            }
        }
    } else {
        outln!("{}", plan.human);
    }
    ExitCode::SUCCESS
}

fn print_error(json: bool, err: &SpatialError, exit: ExitCode) -> ExitCode {
    if json {
        match serde_json::to_string(&error_document(err)) {
            Ok(rendered) => eprintln!("{rendered}"),
            Err(_) => eprintln!("phux: {}", err.message),
        }
    } else {
        eprintln!("phux: {}", err.message);
    }
    exit
}

fn error_document(err: &SpatialError) -> serde_json::Value {
    serde_json::json!({
        "schema_version": JSON_SCHEMA_VERSION,
        "error": { "code": err.code, "message": err.message },
    })
}

fn print_transport_error(json: bool, err: &AttachError, socket_path: &Path) -> ExitCode {
    if json {
        print_error(
            true,
            &SpatialError::new("transport", err.to_string()),
            ExitCode::FAILURE,
        )
    } else {
        report_no_server(err, socket_path, "layout")
    }
}

fn print_transport_or_protocol_error(
    json: bool,
    err: &AttachError,
    socket_path: &Path,
) -> ExitCode {
    print_transport_error(json, err, socket_path)
}

fn print_layout_error(json: bool, err: &LayoutOpsError, socket_path: &Path) -> ExitCode {
    match err {
        LayoutOpsError::Transport(transport) => print_transport_error(json, transport, socket_path),
        LayoutOpsError::MissingLayout => print_error(
            json,
            &SpatialError::new(
                "layout_missing",
                "session has no persisted layout; attach a TUI before editing topology",
            ),
            ExitCode::from(2),
        ),
        LayoutOpsError::ForeignTarget(_) => print_error(
            json,
            &SpatialError::new(
                "pane_not_in_layout",
                "a selected pane is not present in this session's persisted layout",
            ),
            ExitCode::from(2),
        ),
        LayoutOpsError::DuplicatePane(_) => print_error(
            json,
            &SpatialError::new(
                "pane_already_in_layout",
                "the pane being inserted is already present in the persisted layout",
            ),
            ExitCode::from(2),
        ),
        LayoutOpsError::SamePane => print_error(
            json,
            &SpatialError::new(
                "same_pane",
                "the two pane selectors must resolve differently",
            ),
            ExitCode::from(2),
        ),
        other => print_error(
            json,
            &SpatialError::new("layout_rejected", other.to_string()),
            ExitCode::from(2),
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
        let plan = build_plan(path, &snapshot, insert, selectors)
            .await
            .unwrap();
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
        let plan = build_plan(path, &snapshot, move_pane, selectors)
            .await
            .unwrap();
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
        let plan = build_plan(path, &snapshot, swap, selectors).await.unwrap();
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

    #[test]
    fn json_error_documents_are_versioned() {
        let error = error_document(&SpatialError::new("cross_session", "no"));
        assert_eq!(error["schema_version"], 1);
        assert_eq!(error["error"]["code"], "cross_session");
    }
}
