use std::path::Path;

use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, StateScope};
use phux_protocol::wire::info::SessionSnapshot;
use serde_json::json;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout_ops::{LayoutMutation, validate_projection_key};
use crate::selector::{self, Selector, format_terminal_id};
use crate::state::Degradation;

use super::refusal::internal_error;
use super::{
    CrossMovePlan, JSON_SCHEMA_VERSION, Plan, PlanKind, SpatialOp, SpatialOutcome, SpatialRefusal,
    codes,
};

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
    pub(super) const fn ratio(&self) -> Option<f32> {
        match self {
            Self::Insert { ratio, .. } | Self::Move { ratio, .. } => Some(*ratio),
            Self::Swap { .. } => None,
        }
    }

    pub(super) fn parse_selectors(&self) -> Result<Vec<Selector>, SpatialRefusal> {
        self.raw_selectors()
            .into_iter()
            .map(|(role, raw)| {
                selector::parse(raw).map_err(|err| {
                    SpatialRefusal::new(
                        codes::INVALID_SELECTOR,
                        format!("invalid {role} selector {raw:?}: {err}"),
                        "selector grammar: session, session:group, session:group.pane, @id, `.`",
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

/// `GET_STATE` on `conn`; a refusal is reported through `explain_unexpected`.
pub(super) async fn read_snapshot(
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

/// Each role's selector, resolved to exactly one local Terminal.
async fn resolve_roles(
    socket: &Path,
    snapshot: &SessionSnapshot,
    operation: &SpatialOp,
    selectors: &[Selector],
) -> Result<Vec<ResourceId>, SpatialRefusal> {
    let mut terminals = Vec::with_capacity(selectors.len());
    for ((role, _), selector) in operation.raw_selectors().iter().zip(selectors) {
        let candidates = crate::state::resolve_targets(socket, selector, snapshot)
            .await
            .map_err(|err| agent_refusal(role, &err))?;
        terminals.push(exactly_one_local(role, &candidates)?);
    }
    Ok(terminals)
}

pub(super) async fn build_plan(
    socket: &Path,
    snapshot: &SessionSnapshot,
    operation: SpatialOp,
    selectors: Vec<Selector>,
) -> Result<PlanKind, SpatialRefusal> {
    let terminals = resolve_roles(socket, snapshot, &operation, &selectors).await?;
    if terminals.len() == 2 && terminals[0] == terminals[1] {
        return Err(same_pane_error());
    }

    if let Some(plan) = cross_move_plan(snapshot, &operation, &terminals) {
        return Ok(plan);
    }

    let session = same_session(snapshot, &terminals)?;
    let [first, second] = terminals.as_slice() else {
        return Err(internal_error("spatial operation argument mismatch"));
    };
    let (projection, mutation, document, summary) = match operation {
        SpatialOp::Insert {
            direction,
            ratio,
            projection,
            ..
        } => (
            projection,
            LayoutMutation::Split {
                target: first.clone(),
                new_pane: second.clone(),
                dir: direction.wire(),
                ratio,
            },
            json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "insert-pane",
                "session_id": session.get(),
                "target_terminal_id": local_id(first),
                "new_terminal_id": local_id(second),
                "direction": direction.as_str(),
                "ratio": ratio,
            }),
            format!(
                "inserted @{} beside @{} ({}, ratio {ratio})",
                local_id(second),
                local_id(first),
                direction.as_str(),
            ),
        ),
        SpatialOp::Move {
            direction,
            ratio,
            projection,
            ..
        } => (
            projection,
            LayoutMutation::Move {
                source: first.clone(),
                target: second.clone(),
                dir: direction.wire(),
                ratio,
            },
            json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "move-pane",
                "session_id": session.get(),
                "source_terminal_id": local_id(first),
                "target_terminal_id": local_id(second),
                "direction": direction.as_str(),
                "ratio": ratio,
            }),
            format!(
                "moved @{} beside @{} ({}, ratio {ratio})",
                local_id(first),
                local_id(second),
                direction.as_str(),
            ),
        ),
        SpatialOp::Swap { projection, .. } => (
            projection,
            LayoutMutation::Swap {
                first: first.clone(),
                second: second.clone(),
            },
            json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "swap-pane",
                "session_id": session.get(),
                "first_terminal_id": local_id(first),
                "second_terminal_id": local_id(second),
            }),
            format!("swapped @{} and @{}", local_id(first), local_id(second)),
        ),
    };
    Ok(PlanKind::Local(Plan {
        session,
        mutation,
        projection_key: resolve_local_projection(&projection, session)?,
        outcome: SpatialOutcome { document, summary },
    }))
}

/// `projection` for an edit of one session layout: `[]` or one key valid
/// for `session` (ADR-0129).
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

/// The "two selectors, one pane" refusal.
pub(super) fn same_pane_error() -> SpatialRefusal {
    SpatialRefusal::new(
        codes::SAME_PANE,
        "the two pane selectors must resolve differently",
        "pass two selectors that name different panes (`phux ls` lists them)",
        2,
    )
}

pub(super) fn validate_ratio(ratio: f32) -> Result<(), SpatialRefusal> {
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

pub(super) fn exactly_one_local(
    role: &str,
    candidates: &[ResourceId],
) -> Result<ResourceId, SpatialRefusal> {
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

/// A `%name` that did not resolve to one agent, on its ADR-0075 code and
/// exit status.
fn agent_refusal(role: &str, err: &selector::AgentResolveError) -> SpatialRefusal {
    SpatialRefusal::new(
        err.code(),
        format!("{role} selector: {err}"),
        err.remedy(),
        err.exit_code(),
    )
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
        "address exactly one pane, e.g. @N or session:group.pane",
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
    crate::resource::session_of(snapshot, terminal)
}

fn local_id(terminal: &ResourceId) -> u32 {
    terminal.local_id().unwrap_or(0)
}
