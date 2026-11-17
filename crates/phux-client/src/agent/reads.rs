//! `agent list` and `agent show|explain` over a socket: the reports, or why
//! there are none. Printing, warnings, and exit statuses are each surface's.

use std::path::Path;

use phux_protocol::wire::info::SessionSnapshot;

use super::detect::{AgentStateReport, classify_snapshot, configured_agents};
use crate::attach::AttachError;
use crate::selector::{AgentResolveError, Selector, format_terminal_id, pick_target_pane};
use crate::state::Degradation;

/// The projected panes, and what the fleet view could not see.
#[derive(Debug)]
pub struct Projection {
    /// One report per pane, sorted by terminal address.
    pub reports: Vec<AgentStateReport>,
    /// Satellites the snapshot could not reach.
    pub degradation: Degradation,
}

/// Why a read has no reports.
#[derive(Debug)]
pub enum ReadError {
    /// The `GET_STATE` exchange failed.
    Transport(AttachError),
    /// A `%name` selector was refused (ADR-0075 point 3).
    Resolve(AgentResolveError),
    /// The selector matched no Terminal, against this view of the fleet.
    Miss(Degradation),
}

/// `agent list`'s filters: any listed state passes, and every query word
/// must appear in some inventory field, never in screen contents.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListFilter<'a> {
    /// Keep only these state words; empty keeps every state.
    pub states: &'a [String],
    /// Words every kept report must contain somewhere in its inventory.
    pub query: Option<&'a str>,
}

impl ListFilter<'_> {
    /// Whether `report` passes both filters.
    #[must_use]
    pub fn matches(&self, report: &AgentStateReport) -> bool {
        let state_matches = self.states.is_empty()
            || self
                .states
                .iter()
                .any(|state| state == report.state.as_str());
        state_matches && report.matches_query(self.query.unwrap_or_default())
    }
}

/// Every pane at `socket` that passes `filter`.
///
/// Addressability is computed against the whole inventory before
/// filtering: hiding a duplicate name must never turn it into a usable
/// `%name`. A partial record read appends to `notices`.
///
/// # Errors
///
/// [`ReadError::Transport`] when the snapshot read fails.
pub async fn list(
    socket: &Path,
    filter: &ListFilter<'_>,
    notices: &mut Vec<String>,
) -> Result<Projection, ReadError> {
    let (snapshot, degradation) = snapshot(socket).await?;
    let plugins = configured_agents();
    let mut reports = classify_snapshot(socket, &snapshot, &plugins, notices).await;
    reports.retain(|report| filter.matches(report));
    Ok(Projection {
        reports,
        degradation,
    })
}

/// The one pane `selector` names (the focused pane when it spans several).
///
/// `%name` is singular (ADR-0075 point 3): the agent's pane, or a refusal
/// that names why. A partial record read appends to `notices`.
///
/// # Errors
///
/// [`ReadError::Transport`], [`ReadError::Resolve`], or [`ReadError::Miss`].
pub async fn show(
    socket: &Path,
    selector: &Selector,
    notices: &mut Vec<String>,
) -> Result<Projection, ReadError> {
    let (snapshot, degradation) = snapshot(socket).await?;
    let candidates = crate::state::resolve_targets(socket, selector, &snapshot)
        .await
        .map_err(ReadError::Resolve)?;
    let Some(target) = pick_target_pane(&candidates, &snapshot.focused_resource) else {
        return Err(ReadError::Miss(degradation));
    };
    let terminal = format_terminal_id(&target);
    let plugins = configured_agents();
    let reports = classify_snapshot(socket, &snapshot, &plugins, notices).await;
    let Some(report) = reports
        .into_iter()
        .find(|report| report.terminal == terminal)
    else {
        return Err(ReadError::Miss(degradation));
    };
    Ok(Projection {
        reports: vec![report],
        degradation,
    })
}

/// One `GET_STATE`, plus what it could not see.
async fn snapshot(socket: &Path) -> Result<(SessionSnapshot, Degradation), ReadError> {
    crate::state::get_state(socket)
        .await
        .map(crate::state::StateView::into_parts)
        .map_err(|err| {
            tracing::debug!(error = %err, "agent state read failed");
            ReadError::Transport(err)
        })
}
