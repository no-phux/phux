//! Shared `GET_STATE`, L3 tag, and agent-index lookups for the CLI and MCP.

use std::path::Path;

use phux_protocol::caps::{ClientCapabilities, ServerFeature, ServerFeatureSet};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, RESOURCE_TAGS_KEY, Scope, StateScope,
};
use phux_protocol::wire::info::SessionSnapshot;

use crate::agent_meta::{RESOURCE_AGENT_KEY, parse_agent_record};
use crate::attach::AttachError;
use crate::attach::connection::{Answer, Connection};
use crate::selector::{self, AgentIndex, AgentResolveError, AgentTarget, Selector, TagIndex};

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
}

impl StateView {
    /// Pair a snapshot with the degradation observed alongside it.
    pub const fn new(snapshot: SessionSnapshot, degradation: Degradation) -> Self {
        Self {
            snapshot,
            degradation,
            server_features: None,
        }
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

/// `GET_PERF`: the server's performance telemetry; `reset` zeroes it after
/// the snapshot. A server that predates it refuses.
pub async fn get_perf_on(
    conn: &mut Connection,
    reset: bool,
) -> Result<phux_perf::PerfReport, AttachError> {
    const REQUEST_ID: u32 = 0;
    let (result, _interleaved) = conn
        .request(REQUEST_ID, Command::GetPerf { reset })
        .await?
        .into_parts();
    match result {
        CommandResult::OkWith(CommandValue::Json(json)) => phux_perf::PerfReport::from_json(&json)
            .map_err(|err| {
                AttachError::Protocol(format!("GET_PERF returned unparseable JSON: {err}"))
            }),
        CommandResult::Error { message, .. } => Err(AttachError::Refused(message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "GET_PERF", &other,
        ))),
    }
}

/// The protocol version triple the server selected in `HELLO_OK`. A
/// negotiated connection answers without a second `HELLO`; only a raw
/// transport performs the exchange here.
///
/// # Errors
///
/// Transport failure, a refusal (version incompatibility), or an unexpected
/// reply.
pub async fn probe_hello(conn: &mut Connection) -> Result<(u16, u16, u16), AttachError> {
    if conn.negotiated_bootstrap().is_some() {
        return Ok((
            phux_protocol::PROTOCOL_VERSION.major,
            phux_protocol::PROTOCOL_VERSION.minor,
            phux_protocol::PROTOCOL_VERSION.patch,
        ));
    }
    match raw_hello(conn).await? {
        FrameKind::HelloOk {
            protocol_major,
            protocol_minor,
            protocol_patch,
            ..
        } => Ok((protocol_major, protocol_minor, protocol_patch)),
        _ => unreachable!("raw_hello returns only HELLO_OK"),
    }
}

/// The `ServerFeatureSet` the server advertised, so a caller can observe a
/// feature bit before sending. A negotiated connection answers from its
/// kept `HELLO_OK`.
///
/// # Errors
///
/// As [`probe_hello`].
pub async fn probe_hello_features(
    conn: &mut Connection,
) -> Result<Option<phux_protocol::caps::ServerFeatureSet>, AttachError> {
    if let Some(negotiated) = conn.negotiated_bootstrap() {
        return Ok(Some(negotiated.server_features));
    }
    match raw_hello(conn).await? {
        FrameKind::HelloOk { server_caps, .. } => Ok(Some(server_caps.features)),
        _ => unreachable!("raw_hello returns only HELLO_OK"),
    }
}

/// Send `HELLO` on a raw connection; `Ok` only for a `HELLO_OK`.
async fn raw_hello(conn: &mut Connection) -> Result<FrameKind, AttachError> {
    conn.send(&FrameKind::Hello {
        client_name: format!("phux-cli/{}", env!("CARGO_PKG_VERSION")),
        protocol_major: phux_protocol::PROTOCOL_VERSION.major,
        protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
        protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::default(),
    })
    .await?;
    match conn.recv().await? {
        ok @ FrameKind::HelloOk { .. } => Ok(ok),
        FrameKind::Error { message, .. } => Err(AttachError::Refused(message)),
        // Version skew, not a frame worth dumping.
        _ => Err(AttachError::Protocol(crate::explain::unexpected_reply(
            "HELLO",
        ))),
    }
}

/// The uncorrelated `ERROR` messages (a hub's per-satellite degradation
/// notices, `proto.md` §9) among frames interleaved ahead of a reply.
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
    let mut records = std::collections::HashMap::new();
    for (offset, pane) in crate::resource::terminals(snapshot).enumerate() {
        let request_id = u32::try_from(offset).unwrap_or(u32::MAX).saturating_add(1);
        let Ok(reply) = conn
            .request_metadata(
                request_id,
                Scope::Resource(pane.id.clone()),
                RESOURCE_AGENT_KEY.to_owned(),
            )
            .await
        else {
            return AgentIndex::partial(records);
        };
        let (answer, interleaved) = reply.into_parts();
        report_degradation(&interleaved);
        match answer {
            Answer::Ok(Some(bytes)) => {
                if let Some(record) = parse_agent_record(&bytes) {
                    records.insert(pane.id.clone(), record);
                }
            }
            Answer::Ok(None) => {}
            // A refusal is not "no record": the pane was not looked at.
            Answer::Err(_) => return AgentIndex::partial(records),
        }
    }
    AgentIndex::complete(records)
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
/// only when the form needs metadata.
///
/// `#tag` fetches L3 tags (a failed lookup is a miss). `%name` is singular
/// (ADR-0075 point 3): exactly the one named Terminal, or a typed refusal —
/// never the empty miss a set-valued verb would misreport as "no such
/// target". Every other form is pure snapshot resolution. Input verbs that
/// need the point 5 write guard resolve `%name` through
/// [`resolve_agent_target`] with `for_input` instead.
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
        Selector::Tag(_) => {
            let tags = match Connection::connect(socket).await {
                Ok(mut conn) => fetch_tag_index(&mut conn, snapshot).await,
                Err(_) => TagIndex::new(),
            };
            Ok(selector::resolve_with_tags(selector, snapshot, &tags))
        }
        _ => Ok(selector::resolve(selector, snapshot)),
    }
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
        Selector::Tag(_) => {
            let tags = fetch_tag_index(conn, snapshot).await;
            Ok(selector::resolve_with_tags(selector, snapshot, &tags))
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
mod tests {
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    use phux_protocol::wire::frame::{Command, ErrorCode, FrameKind};
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

    use super::{Connection, get_state};
    use crate::testkit::{ScriptSpec, serve_one};

    #[tokio::test]
    async fn get_state_skips_unrelated_command_results() {
        let dir = tempfile::tempdir().unwrap();
        let expected =
            SessionSnapshot::new(SessionId::new(7), WindowId::new(8), ResourceId::local(9));
        // A foreign COMMAND_RESULT ahead of this ack must be skipped.
        let spec = ScriptSpec::new().foreign_ack(99).state(expected.clone());
        let (socket, server) = serve_one(dir.path(), spec);

        let view = get_state(&socket).await.unwrap();
        assert!(
            view.is_complete(),
            "a server that pushed no uncorrelated ERROR is not degraded"
        );
        assert_eq!(*view.snapshot(), expected);
        let seen = server.await.unwrap();
        let Some(FrameKind::Hello {
            client_name,
            client_caps,
            ..
        }) = seen.first()
        else {
            panic!("HELLO must be first, got {seen:?}");
        };
        assert!(client_name.starts_with("phux-client/"));
        assert_eq!(
            client_caps.layers,
            phux_protocol::caps::LayerSet::with(&[phux_protocol::caps::Layer::L3]),
            "control commands advertise the metadata tier used by the shared connection API"
        );
        assert!(
            matches!(
                seen.get(1),
                Some(FrameKind::Command {
                    request_id: 0,
                    command: Command::GetState { .. }
                })
            ),
            "GET_STATE must follow negotiation on request id 0; got {seen:?}",
        );
        assert_eq!(seen.len(), 2, "exactly one HELLO and one GET_STATE");
    }

    #[tokio::test]
    async fn get_state_hands_back_the_degradation_with_the_snapshot() {
        // A hub's degradation notice ahead of the ack rides with the view.
        let dir = tempfile::tempdir().unwrap();
        let expected =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(2), ResourceId::local(3));
        let spec = ScriptSpec::new()
            .degradation_notice("satellite build-box is unreachable: link is down")
            .state(expected.clone());
        let (socket, server) = serve_one(dir.path(), spec);

        let view = get_state(&socket).await.unwrap();
        assert!(!view.is_complete(), "one dead satellite is a partial view");
        assert_eq!(*view.snapshot(), expected, "degradation is not failure");
        let (snapshot, degradation) = view.into_parts();
        assert_eq!(snapshot, expected);
        assert_eq!(
            degradation.notices(),
            ["satellite build-box is unreachable: link is down".to_owned()]
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn probe_hello_returns_the_negotiated_version_triple() {
        let dir = tempfile::tempdir().unwrap();
        let (socket, server) = serve_one(dir.path(), ScriptSpec::new());

        let mut conn = Connection::connect(&socket).await.unwrap();
        let triple = super::probe_hello(&mut conn).await.unwrap();
        assert_eq!(
            triple,
            (
                phux_protocol::PROTOCOL_VERSION.major,
                phux_protocol::PROTOCOL_VERSION.minor,
                phux_protocol::PROTOCOL_VERSION.patch,
            )
        );
        drop(conn);
        let seen = server.await.unwrap();
        assert!(
            matches!(seen.first(), Some(FrameKind::Hello { .. })),
            "the probe's first frame must be HELLO; got {:?}",
            seen.first()
        );
        assert_eq!(
            seen.iter()
                .filter(|frame| matches!(frame, FrameKind::Hello { .. }))
                .count(),
            1,
            "probing an already-negotiated connection must not emit a second HELLO",
        );
    }

    #[tokio::test]
    async fn peer_pid_names_the_process_behind_the_socket() {
        // The listener is this process, so the peer pid is ours.
        let dir = tempfile::tempdir().unwrap();
        let (socket, server) = serve_one(dir.path(), ScriptSpec::new());

        let conn = Connection::connect(&socket).await.unwrap();
        assert_eq!(
            conn.peer_pid(),
            Some(i32::try_from(std::process::id()).unwrap()),
            "a UDS connection's peer pid must be the listening process"
        );
        drop(conn);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn the_agent_index_is_complete_only_when_every_pane_answered() {
        // Two panes, one record: a definite absence keeps the index complete.
        let dir = tempfile::tempdir().unwrap();
        let spec = ScriptSpec::new().metadata(|scope, key| {
            (key == crate::agent_meta::RESOURCE_AGENT_KEY
                && matches!(
                    scope,
                    phux_protocol::wire::frame::Scope::Resource(id) if *id == ResourceId::local(1)
                ))
            .then(|| br#"{"name":"reviewer","kind":"claude","state":"working"}"#.to_vec())
        });
        let (socket, server) = serve_one(dir.path(), spec);
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
                .with_resources(vec![
                    ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24),
                    ResourceInfo::new(ResourceId::local(2), WindowId::new(1), 80, 24),
                ]);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let index = super::fetch_agent_index(&mut conn, &snapshot).await;
        assert!(index.is_complete());
        assert!(index.get(&ResourceId::local(2)).is_none());
        assert_eq!(index.get(&ResourceId::local(1)).unwrap().name, "reviewer");
        drop(conn);
        server.await.unwrap();

        // A refused read is not an absence: the index is partial.
        let dir = tempfile::tempdir().unwrap();
        let spec = ScriptSpec::new().refuse_metadata(ErrorCode::PermissionDenied, "policy refused");
        let (socket, server) = serve_one(dir.path(), spec);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let index = super::fetch_agent_index(&mut conn, &snapshot).await;
        assert!(
            !index.is_complete(),
            "a refusal must not read as a complete absence"
        );
        drop(conn);
        server.await.unwrap();
    }

    #[test]
    fn degradation_notices_ignores_correlated_errors() {
        // A correlated ERROR is some command's answer, not a notice.
        let frames = vec![
            FrameKind::Error {
                request_id: Some(4),
                code: phux_protocol::wire::frame::ErrorCode::TerminalNotFound,
                message: "someone else's refusal".to_owned(),
            },
            FrameKind::Error {
                request_id: None,
                code: phux_protocol::wire::frame::ErrorCode::UnsupportedSatelliteRoute,
                message: "satellite unreachable".to_owned(),
            },
        ];
        assert_eq!(
            super::degradation_notices(&frames),
            vec!["satellite unreachable".to_owned()]
        );
    }

    /// Long enough that a loaded machine cannot trip it, short enough that a
    /// genuine wedge fails this test instead of hanging the run.
    const WEDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

    #[tokio::test]
    async fn refused_tag_read_ends_the_index_instead_of_wedging_it() {
        // Regression: a correlated ERROR on one tag read once wedged every
        // `#tag` selector forever. The timeout is the assertion.
        let dir = tempfile::tempdir().unwrap();
        let spec = ScriptSpec::new().refuse_metadata(ErrorCode::PermissionDenied, "policy refused");
        let (socket, server) = serve_one(dir.path(), spec);

        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
                .with_resources(vec![
                    ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24),
                    ResourceInfo::new(ResourceId::local(2), WindowId::new(1), 80, 24),
                ]);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let index =
            tokio::time::timeout(WEDGE_TIMEOUT, super::fetch_tag_index(&mut conn, &snapshot))
                .await
                .expect("a refused read must end the index; a timeout here is the wedge itself");

        assert!(index.is_empty(), "got {index:?}");
        drop(conn);
        server.await.unwrap();
    }
}
