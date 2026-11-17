//! Slot tab edits (`docs/organization.md#slot-tabs`): the same-session plans
//! behind `phux slot add|out|next|prev`.

use phux_protocol::ids::{ResourceId, SessionId};
use serde_json::json;

use super::plan::local_id;
use super::refusal::internal_error;
use super::{JSON_SCHEMA_VERSION, Plan, SpatialOp, SpatialOutcome, SpatialRefusal, codes};
use crate::layout_ops::LayoutMutation;

impl SpatialOp {
    /// Whether this edits slot tabs, which only a server advertising
    /// `SLOT_TABS` keeps.
    pub(super) const fn is_slot(&self) -> bool {
        matches!(
            self,
            Self::SlotAdd { .. } | Self::SlotOut { .. } | Self::SlotStep { .. }
        )
    }
}

/// The refusal from a server that would drop tabs written to it.
pub(super) fn unsupported() -> SpatialRefusal {
    SpatialRefusal::new(
        codes::SLOT_TABS_UNSUPPORTED,
        "this phux server does not keep slot tabs",
        "update the server (`phux update`, then `phux upgrade`)",
        2,
    )
}

/// The plan for a slot edit of `session`, whose panes `terminals` are, in
/// the operation's selector order.
pub(super) fn plan(
    operation: SpatialOp,
    session: SessionId,
    terminals: &[ResourceId],
) -> Result<Plan, SpatialRefusal> {
    let (mutation, document, summary) = match (operation, terminals) {
        (SpatialOp::SlotAdd { .. }, [pane, to]) => (
            LayoutMutation::SlotAdd {
                pane: pane.clone(),
                to: to.clone(),
            },
            json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "slot-add",
                "session_id": session.get(),
                "terminal_id": local_id(pane),
                "slot_terminal_id": local_id(to),
            }),
            format!("@{} is a tab of @{}'s slot", local_id(pane), local_id(to)),
        ),
        (
            SpatialOp::SlotOut {
                direction, ratio, ..
            },
            [pane],
        ) => (
            LayoutMutation::SlotOut {
                pane: pane.clone(),
                dir: direction.wire(),
                ratio,
            },
            json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": "slot-out",
                "session_id": session.get(),
                "terminal_id": local_id(pane),
                "direction": direction.as_str(),
                "ratio": ratio,
            }),
            format!("@{} has a slot of its own", local_id(pane)),
        ),
        (SpatialOp::SlotStep { delta, .. }, [pane]) => (
            LayoutMutation::SlotStep {
                pane: pane.clone(),
                delta,
            },
            json!({
                "schema_version": JSON_SCHEMA_VERSION,
                "operation": if delta < 0 { "slot-prev" } else { "slot-next" },
                "session_id": session.get(),
                "terminal_id": local_id(pane),
            }),
            format!(
                "moved the default tab of @{}'s slot {}",
                local_id(pane),
                if delta < 0 { "back" } else { "on" }
            ),
        ),
        _ => return Err(internal_error("slot operation argument mismatch")),
    };
    Ok(Plan {
        session,
        mutation,
        projection_key: None,
        outcome: SpatialOutcome { document, summary },
    })
}
