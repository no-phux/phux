//! Shared `GET_STATE`, L3 tag, and agent-index lookups for the CLI and MCP.

use std::path::Path;

use phux_protocol::caps::{ServerFeature, ServerFeatureSet};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, RESOURCE_TAGS_KEY, Scope, StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::selector::{
    self, AgentIndex, AgentResolveError, AgentTarget, Arrangements, ResolveContext, Selector,
    TagIndex,
};

mod hello;
mod perf;

pub use hello::{probe_hello, probe_hello_features, probe_hello_features_ext};
pub use perf::{get_perf_on, perf_control_on};

/// How much of the fleet a `GET_STATE` answer could not see: one hub
/// diagnostic per unreachable satellite, empty when complete. Branch on
/// [`Degradation::is_complete`], never on the prose.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Degradation {
    /// One notice per satellite that contributed nothing, in arrival order.
    notices: Vec<String>,
}

impl Degradation {
    /// The degradation implied by the frames a server pushed ahead of an ack.
    #[must_use]
    pub fn from_interleaved(interleaved: &[FrameKind]) -> Self {
        Self {
            notices: degradation_notices(interleaved),
        }
    }

    /// Whether the answer this accompanies covers the whole fleet, so an
    /// absence in it can be trusted.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.notices.is_empty()
    }

    /// The per-satellite diagnostics, for rendering only.
    #[must_use]
    pub fn notices(&self) -> &[String] {
        &self.notices
    }
}

/// The stderr line `phux` and `phux-mcp` print per notice when a verb hits
/// against a partial fleet view.
#[must_use]
pub fn partial_view_warning(verb: &str, notice: &str) -> String {
    format!("phux: warning: {verb} saw only part of the fleet — {notice}")
}

/// A `GET_STATE` answer together with the part of the fleet it could not see.
///
/// A hub's merged snapshot silently omits unreachable satellites' panes, so
/// "no such target" against it is a guess. The pairing is structural: the
/// only way to drop the degradation is the audit-trail-named
/// [`Self::into_snapshot_ignoring_degradation`].
#[derive(Debug, Clone)]
#[must_use = "the view says whether the snapshot is complete; dropping it \
              turns a partial answer into a confident one"]
pub struct StateView {
    /// The merged snapshot the `GET_STATE` ack carried.
    snapshot: SessionSnapshot,
    /// What that snapshot could not see.
    degradation: Degradation,
    /// The features the answering server advertised, when negotiated: an
    /// empty `hosts` list is authoritative only under `HostSessions`.
    server_features: Option<ServerFeatureSet>,
    /// The organization records read alongside, when the server advertises
    /// `ORGANIZATION` and the reader asked for them.
    organization: Option<Box<crate::org::OrgInventory>>,
}

impl StateView {
    /// Pair a snapshot with the degradation observed alongside it.
    pub const fn new(snapshot: SessionSnapshot, degradation: Degradation) -> Self {
        Self {
            snapshot,
            degradation,
            server_features: None,
            organization: None,
        }
    }

    /// Attach the organization records read alongside the snapshot.
    pub fn with_organization(mut self, organization: Option<crate::org::OrgInventory>) -> Self {
        self.organization = organization.map(Box::new);
        self
    }

    /// The organization records read alongside the snapshot, when any.
    #[must_use]
    pub fn organization(&self) -> Option<&crate::org::OrgInventory> {
        self.organization.as_deref()
    }

    /// Record the feature set the answering server negotiated.
    pub const fn with_server_features(mut self, features: ServerFeatureSet) -> Self {
        self.server_features = Some(features);
        self
    }

    /// The feature set the answering server negotiated, when known.
    #[must_use]
    pub const fn server_features(&self) -> Option<ServerFeatureSet> {
        self.server_features
    }

    /// Whether the host-session inventory is authoritative (the server
    /// advertised `ServerFeature::HostSessions`).
    #[must_use]
    pub const fn host_sessions_complete(&self) -> bool {
        match self.server_features {
            Some(features) => features.contains(ServerFeature::HostSessions),
            None => false,
        }
    }

    /// The snapshot and its degradation, both bound.
    #[must_use]
    pub fn into_parts(self) -> (SessionSnapshot, Degradation) {
        (self.snapshot, self.degradation)
    }

    /// Borrow the merged snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    /// Mutably borrow the merged snapshot, for a listing that refines it
    /// from other reads (layout window counts) without dropping the
    /// degradation.
    pub(crate) const fn snapshot_mut(&mut self) -> &mut SessionSnapshot {
        &mut self.snapshot
    }

    /// Borrow what the snapshot could not see.
    #[must_use]
    pub const fn degradation(&self) -> &Degradation {
        &self.degradation
    }

    /// Whether this answer covers the whole fleet — see
    /// [`Degradation::is_complete`].
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.degradation.is_complete()
    }

    /// Take the snapshot and drop the degradation.
    ///
    /// Only correct where a partial view cannot change the answer: reading
    /// only `sessions`/`windows` (which never aggregate), or with no channel
    /// to report on. Never for Terminal selectors; cite the reason at the
    /// call site.
    #[must_use]
    pub fn into_snapshot_ignoring_degradation(self) -> SessionSnapshot {
        if !self.degradation.is_complete() {
            tracing::warn!(
                notices = ?self.degradation.notices(),
                "a partial GET_STATE view was consumed as if it were complete",
            );
        }
        self.snapshot
    }
}

/// Fetch the server-wide session snapshot over a fresh connection.
///
/// # Errors
///
/// Returns a transport error when the connection cannot be opened or closes
/// during the request, a refusal when the server rejects `GET_STATE`, or a
/// protocol error when the matching response has an unexpected value.
pub async fn get_state(socket: &Path) -> Result<StateView, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    get_state_on(&mut conn).await
}

/// Fetch the server-wide session snapshot over an existing connection; the
/// uncorrelated `ERROR`s interleaved ahead become its [`Degradation`].
///
/// # Errors
///
/// Returns a transport error when the connection closes during the request, a
/// refusal when the server rejects `GET_STATE`, or a protocol error when the
/// matching response has an unexpected value.
pub async fn get_state_on(conn: &mut Connection) -> Result<StateView, AttachError> {
    let (view, _interleaved) = get_state_on_with_interleaved(conn).await?;
    Ok(view)
}

/// Fetch server state keeping every interleaved frame, so a subscriber can
/// replay lifecycle events from its subscribe/read window.
pub(crate) async fn get_state_on_with_interleaved(
    conn: &mut Connection,
) -> Result<(StateView, Vec<FrameKind>), AttachError> {
    let (result, interleaved) = get_state_reply(conn).await?;
    let view = state_view(conn, result, &interleaved)?;
    Ok((view, interleaved))
}

/// `GET_STATE`'s raw answer and the frames interleaved ahead of it, which a
/// caller can still read when the answer is a refusal.
pub(crate) async fn get_state_reply(
    conn: &mut Connection,
) -> Result<(CommandResult, Vec<FrameKind>), AttachError> {
    const REQUEST_ID: u32 = 0;
    Ok(conn
        .request(
            REQUEST_ID,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await?
        .into_parts())
}

/// The [`StateView`] a `GET_STATE` answer carries.
pub(crate) fn state_view(
    conn: &Connection,
    result: CommandResult,
    interleaved: &[FrameKind],
) -> Result<StateView, AttachError> {
    let degradation = Degradation::from_interleaved(interleaved);
    match result {
        CommandResult::OkWith(CommandValue::State(snapshot)) => {
            let view = StateView::new(snapshot, degradation);
            Ok(match conn.negotiated_bootstrap() {
                Some(negotiated) => view.with_server_features(negotiated.server_features),
                None => view,
            })
        }
        CommandResult::Error { message, .. } => Err(AttachError::Refused(message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "GET_STATE",
            &other,
        ))),
    }
}

/// The uncorrelated `ERROR` messages (a hub's per-satellite degradation
/// notices, `docs/protocol.md`) among frames interleaved ahead of a reply.
#[must_use]
pub fn degradation_notices(interleaved: &[FrameKind]) -> Vec<String> {
    interleaved
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::Error {
                request_id: None,
                message,
                ..
            } => Some(message.clone()),
            _ => None,
        })
        .collect()
}

/// Log every degradation notice in `interleaved` at `warn`; a CLI should
/// print [`degradation_notices`] instead.
pub fn report_degradation(interleaved: &[FrameKind]) {
    for message in degradation_notices(interleaved) {
        tracing::warn!(
            %message,
            "server reported partial state: a federated satellite contributed nothing",
        );
    }
}

/// Fetch the L3 tag index for every pane in `snapshot` over `conn`, one
/// sequential round trip per pane (request ids from 1; `GET_STATE` uses 0).
///
/// Best-effort: missing, malformed, or refused values are omitted, and a
/// transport failure returns what was collected.
pub async fn fetch_tag_index(conn: &mut Connection, snapshot: &SessionSnapshot) -> TagIndex {
    let mut index = TagIndex::new();
    for (offset, pane) in crate::resource::terminals(snapshot).enumerate() {
        let request_id = u32::try_from(offset).unwrap_or(u32::MAX).saturating_add(1);
        let Ok(reply) = conn
            .request_metadata(
                request_id,
                Scope::Resource(pane.id.clone()),
                RESOURCE_TAGS_KEY.to_owned(),
            )
            .await
        else {
            return index;
        };
        let (answer, interleaved) = reply.into_parts();
        report_degradation(&interleaved);
        if let Ok(Some(bytes)) = answer
            && let Ok(tags) = serde_json::from_slice::<Vec<String>>(&bytes)
            && !tags.is_empty()
        {
            index.insert(pane.id.clone(), tags);
        }
    }
    index
}

/// Fetch the `phux.agent/v1` index for every Terminal in `snapshot`, the
/// `%name` resolver's input (ADR-0075 point 3).
///
/// Unlike [`fetch_tag_index`], a transport failure or refused read returns
/// [`AgentIndex::partial`]; a missing or invalid record is a definite absence.
pub async fn fetch_agent_index(conn: &mut Connection, snapshot: &SessionSnapshot) -> AgentIndex {
    let panes: Vec<&ResourceId> = crate::resource::terminals(snapshot)
        .map(|pane| &pane.id)
        .collect();
    let Ok((answers, degradation)) = crate::agent_record::get_records(conn, &panes, 1).await else {
        return AgentIndex::partial(std::collections::HashMap::new());
    };
    for message in degradation.notices() {
        tracing::warn!(
            %message,
            "server reported partial state: a federated satellite contributed nothing",
        );
    }
    let mut records = std::collections::HashMap::new();
    let mut complete = true;
    for (pane, answer) in panes.into_iter().zip(answers) {
        match answer {
            Ok(Some(record)) => {
                records.insert(pane.clone(), record);
            }
            Ok(None) => {}
            // A refusal is not "no record": the pane was not looked at.
            Err(_) => complete = false,
        }
    }
    if complete {
        AgentIndex::complete(records)
    } else {
        AgentIndex::partial(records)
    }
}

/// Resolve `%name` on an open connection: build the index on `conn`, then
/// apply [`selector::resolve_agent`] (or, when `for_input`, the ADR-0075
/// point 5 write guard [`selector::resolve_agent_for_input`]).
///
/// # Errors
///
/// [`AgentResolveError`] — see its variants.
pub async fn resolve_agent_on(
    conn: &mut Connection,
    name: &str,
    snapshot: &SessionSnapshot,
    for_input: bool,
) -> Result<AgentTarget, AgentResolveError> {
    let index = fetch_agent_index(conn, snapshot).await;
    resolve_agent_in(name, snapshot, &index, for_input)
}

/// Resolve `%name` over a fresh connection, as [`resolve_agent_on`]. A failed
/// connect is a partial index, not a miss.
///
/// # Errors
///
/// [`AgentResolveError`] — see its variants.
pub async fn resolve_agent_target(
    socket: &Path,
    name: &str,
    snapshot: &SessionSnapshot,
    for_input: bool,
) -> Result<AgentTarget, AgentResolveError> {
    match Connection::connect(socket).await {
        Ok(mut conn) => resolve_agent_on(&mut conn, name, snapshot, for_input).await,
        Err(_) => resolve_agent_in(name, snapshot, &AgentIndex::default(), for_input),
    }
}

fn resolve_agent_in(
    name: &str,
    snapshot: &SessionSnapshot,
    index: &AgentIndex,
    for_input: bool,
) -> Result<AgentTarget, AgentResolveError> {
    if for_input {
        selector::resolve_agent_for_input(name, snapshot, index)
    } else {
        selector::resolve_agent(name, snapshot, index)
    }
}

/// Resolve a selector to the Terminals it names, over a fresh connection
/// only when the form needs more than the snapshot.
///
/// `#tag` fetches L3 tags (a failed lookup is a miss). `%name` is singular
/// (ADR-0075 point 3): exactly the one named Terminal, or a typed refusal —
/// never the empty miss a set-valued verb would misreport as "no such
/// target". `name:N` and `name:N.M` read the session's arrangement from a
/// server that advertises `ORGANIZATION` (else the legacy windows), and
/// `^key` reads the organization records. Every other form is pure snapshot
/// resolution. Input verbs that need the point 5 write guard resolve
/// `%name` through [`resolve_agent_target`] with `for_input` instead.
///
/// # Errors
///
/// [`AgentResolveError`] for a `%name` that does not resolve to one agent.
pub async fn resolve_targets(
    socket: &Path,
    selector: &Selector,
    snapshot: &SessionSnapshot,
) -> Result<Vec<ResourceId>, AgentResolveError> {
    match selector {
        Selector::Agent(name) => Ok(vec![
            resolve_agent_target(socket, name, snapshot, false)
                .await?
                .terminal,
        ]),
        Selector::Tag(_) | Selector::Group(..) | Selector::Pane(..) | Selector::Key(_) => {
            match Connection::connect(socket).await {
                Ok(mut conn) => Ok(resolve_with_reads(&mut conn, selector, snapshot).await),
                Err(_) => Ok(selector::resolve(selector, snapshot)),
            }
        }
        _ => Ok(selector::resolve(selector, snapshot)),
    }
}

/// Resolve a form that needs reads beyond the snapshot, doing those reads
/// on `conn`; a failed read resolves as if its input were absent.
async fn resolve_with_reads(
    conn: &mut Connection,
    selector: &Selector,
    snapshot: &SessionSnapshot,
) -> Vec<ResourceId> {
    match selector {
        Selector::Tag(_) => {
            let tags = fetch_tag_index(conn, snapshot).await;
            selector::resolve_with_tags(selector, snapshot, &tags)
        }
        Selector::Group(name, _) | Selector::Pane(name, _, _) => {
            let arrangements = fetch_arrangement(conn, snapshot, name).await;
            selector::resolve_in(
                selector,
                snapshot,
                &ResolveContext {
                    arrangements: Some(&arrangements),
                    ..ResolveContext::default()
                },
            )
        }
        Selector::Key(_) => {
            let organization = if crate::org::supported(conn) {
                crate::org::fetch(conn, phux_protocol::wire::frame::OrgFilter::All, 0x5200)
                    .await
                    .ok()
            } else {
                None
            };
            selector::resolve_in(
                selector,
                snapshot,
                &ResolveContext {
                    organization: organization.as_ref(),
                    ..ResolveContext::default()
                },
            )
        }
        _ => selector::resolve(selector, snapshot),
    }
}

/// The arrangement of the session named `name`, when the server advertises
/// `ORGANIZATION` and the session has one this connection can read.
pub async fn fetch_arrangement(
    conn: &mut Connection,
    snapshot: &SessionSnapshot,
    name: &str,
) -> Arrangements {
    let mut arrangements = Arrangements::new();
    if !crate::org::supported(conn) {
        return arrangements;
    }
    let Some(session) = snapshot.sessions.iter().find(|s| s.name == name) else {
        return arrangements;
    };
    if let Ok(document) = crate::layout_ops::LayoutOps::new(conn, session.id, 0x5100)
        .read()
        .await
    {
        arrangements.insert(session.id, document);
    }
    arrangements
}

/// [`resolve_targets`] on an open connection, for verbs that act on the same
/// connection they resolved on (`kill`, `tag`, `signal`).
///
/// # Errors
///
/// [`AgentResolveError`] for a `%name` that does not resolve to one agent.
pub async fn resolve_targets_on(
    conn: &mut Connection,
    selector: &Selector,
    snapshot: &SessionSnapshot,
) -> Result<Vec<ResourceId>, AgentResolveError> {
    match selector {
        Selector::Agent(name) => Ok(vec![
            resolve_agent_on(conn, name, snapshot, false)
                .await?
                .terminal,
        ]),
        Selector::Tag(_) | Selector::Group(..) | Selector::Pane(..) | Selector::Key(_) => {
            Ok(resolve_with_reads(conn, selector, snapshot).await)
        }
        _ => Ok(selector::resolve(selector, snapshot)),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests;
