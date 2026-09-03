//! Shared server-state and L3 tag lookup helpers.
//!
//! CLI and MCP consumers use these free functions instead of maintaining
//! separate `GET_STATE` and `GET_METADATA` receive loops. Selector resolution
//! remains client-side (ADR-0017), and candidates retain snapshot order.

use std::path::Path;

use phux_protocol::caps::ClientCapabilities;
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, Scope, StateScope, TERMINAL_TAGS_KEY,
};
use phux_protocol::wire::info::SessionSnapshot;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::selector::{self, Selector, TagIndex};

/// How much of the fleet a `GET_STATE` answer could not see.
///
/// Empty is the normal case and means "complete": a non-hub server, or a hub
/// that reached every satellite in its registry. A non-empty one carries the
/// hub's own per-satellite diagnostics, in the order it emitted them.
///
/// The messages are prose written by `crates/phux-server/src/hub/relay.rs`
/// (`satellite <host> is unreachable: <why>`, `... link is saturated; retry`,
/// `... did not answer within <n>s`). They name the satellite, but their exact
/// text is a diagnostic, not a contract — branch on
/// [`Degradation::is_complete`], not on substrings.
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

    /// Whether the answer this accompanies covers the whole fleet.
    ///
    /// The one predicate a caller should branch on. Spelled affirmatively
    /// because every interesting call site asks "may I trust an *absence* in
    /// this snapshot?", and `if view.is_complete()` reads as that question.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.notices.is_empty()
    }

    /// The per-satellite diagnostics, in the order the hub emitted them.
    ///
    /// For rendering — a stderr warning, a `--json` document's `unreachable`
    /// list. Do not parse them; see the type docs.
    #[must_use]
    pub fn notices(&self) -> &[String] {
        &self.notices
    }
}

/// A `GET_STATE` answer together with the part of the fleet it could not see.
///
/// # Why the snapshot is not returned bare
///
/// `handle_get_state_federated` answers a hub-wide `GET_STATE` with a *merged*
/// snapshot and pushes one uncorrelated `ERROR` per unreachable satellite
/// ahead of the ack, deliberately — "observable degradation, not silence". The
/// merged snapshot is perfectly usable; what it is *not* is complete. It
/// simply omits that satellite's panes, with nothing in the value itself to
/// say so.
///
/// That omission is invisible at exactly the moment it matters. Every
/// selector the CLI resolves is a *search* over `snapshot.panes`, and a search
/// that finds nothing is reported as "no such target". Against a degraded
/// snapshot that sentence is a guess: the pane may be sitting on the
/// satellite the hub could not reach. Returning the snapshot alone lets a
/// caller state that guess as a fact, which is the defect this type exists to
/// make unwritable.
///
/// So the pairing is structural, the same principle as
/// [`Reply`](crate::attach::connection::Reply): the fields are private, the
/// snapshot is reachable only through a named accessor, and the only way to
/// drop the degradation is
/// [`Self::into_snapshot_ignoring_degradation`] — whose name is the audit
/// trail. `grep` for it and you have the complete list of places that claim a
/// partial fleet view cannot change their answer; each one owes a comment
/// saying why.
#[derive(Debug, Clone)]
#[must_use = "the view says whether the snapshot is complete; dropping it \
              turns a partial answer into a confident one"]
pub struct StateView {
    /// The merged snapshot the `GET_STATE` ack carried.
    snapshot: SessionSnapshot,
    /// What that snapshot could not see.
    degradation: Degradation,
}

impl StateView {
    /// Pair a snapshot with the degradation observed alongside it.
    pub const fn new(snapshot: SessionSnapshot, degradation: Degradation) -> Self {
        Self {
            snapshot,
            degradation,
        }
    }

    /// The snapshot and its degradation, both bound.
    ///
    /// The default way to consume a view: binding both halves is what makes
    /// forgetting one a visible act rather than an omission.
    #[must_use]
    pub fn into_parts(self) -> (SessionSnapshot, Degradation) {
        (self.snapshot, self.degradation)
    }

    /// Borrow the merged snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
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

    /// Take the snapshot and drop the degradation on the floor.
    ///
    /// **Only correct where a partial fleet view cannot change the answer.**
    /// The two cases that hold today:
    ///
    /// - the caller only reads `sessions` / `windows`. Those never aggregate
    ///   — `handle_get_state_federated` discards a satellite's session and
    ///   window lists because their `u32` ids would collide with the hub's —
    ///   so an unreachable satellite cannot add or hide one.
    /// - the caller has no channel to report on (a library helper whose
    ///   caller already reported, a best-effort background read).
    ///
    /// Anything that resolves a Terminal selector is in neither case: panes
    /// *do* aggregate. Cite the reason in a comment at every call site; this
    /// method name is how the next audit finds you.
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

/// Fetch the server-wide session snapshot over an existing connection.
///
/// Unrelated interleaved frames are skipped until the matching command result
/// arrives, as required by SPEC §5; the uncorrelated `ERROR`s among them
/// become the returned view's [`Degradation`].
///
/// # Errors
///
/// Returns a transport error when the connection closes during the request, a
/// refusal when the server rejects `GET_STATE`, or a protocol error when the
/// matching response has an unexpected value.
pub async fn get_state_on(conn: &mut Connection) -> Result<StateView, AttachError> {
    const REQUEST_ID: u32 = 0;
    let (result, interleaved) = conn
        .request(
            REQUEST_ID,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await?
        .into_parts();
    // A hub answers GET_STATE with a *merged* snapshot and reports each
    // unreachable satellite as an uncorrelated ERROR pushed ahead of the ack
    // (`handle_get_state_federated`: "observable degradation, not silence").
    // The snapshot is still usable — it just does not list that satellite's
    // panes — so degradation rides *with* the value instead of failing it.
    let degradation = Degradation::from_interleaved(&interleaved);
    match result {
        CommandResult::OkWith(CommandValue::State(snapshot)) => {
            Ok(StateView::new(snapshot, degradation))
        }
        CommandResult::Error { message, .. } => Err(AttachError::Refused(message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "GET_STATE",
            &other,
        ))),
    }
}

/// `GET_PERF` on an already-negotiated connection.
///
/// Returns the server's in-process performance telemetry as a
/// [`phux_perf::PerfReport`]. `reset` zeroes the server's metrics after the
/// snapshot so the next call reads as an interval. A server that predates
/// the command answers with an error, surfaced as [`AttachError::Refused`].
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

/// Send `HELLO` on `conn` and return the protocol version triple the server
/// selected in `HELLO_OK`.
///
/// Production [`Connection`] constructors already complete that exchange.
/// This helper returns the version they validated without emitting a second
/// `HELLO`; crate-internal raw connections still perform the exchange here.
///
/// # Errors
///
/// Returns a transport error when an unnegotiated raw connection fails
/// mid-exchange, a refusal carrying the server's message when it answers
/// `ERROR` (version incompatibility), or a protocol error on any other reply.
///
/// Callers may use this on either a production-negotiated connection or a
/// crate-internal raw connection.
pub async fn probe_hello(conn: &mut Connection) -> Result<(u16, u16, u16), AttachError> {
    if conn.negotiated_bootstrap().is_some() {
        return Ok((
            phux_protocol::PROTOCOL_VERSION.major,
            phux_protocol::PROTOCOL_VERSION.minor,
            phux_protocol::PROTOCOL_VERSION.patch,
        ));
    }
    conn.send(&FrameKind::Hello {
        client_name: format!("phux-cli/{}", env!("CARGO_PKG_VERSION")),
        protocol_major: phux_protocol::PROTOCOL_VERSION.major,
        protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
        protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::default(),
    })
    .await?;
    match conn.recv().await? {
        FrameKind::HelloOk {
            protocol_major,
            protocol_minor,
            protocol_patch,
            ..
        } => Ok((protocol_major, protocol_minor, protocol_patch)),
        FrameKind::Error { message, .. } => Err(AttachError::Refused(message)),
        // Neither HELLO_OK nor ERROR: the two binaries disagree about the
        // handshake itself, which is version skew, not a frame worth dumping.
        _ => Err(AttachError::Protocol(crate::explain::unexpected_reply(
            "HELLO",
        ))),
    }
}

/// The uncorrelated `ERROR` messages among frames the server interleaved
/// ahead of a `COMMAND_RESULT`.
///
/// An `ERROR` with no `request_id` is not any command's answer (`proto.md`
/// §9) — on this wire it is the federation degradation notice a hub emits
/// per unreachable satellite. Extracted rather than logged here so a caller
/// that owns a user-visible channel (the CLI owns stderr) can surface it
/// instead of burying it in a `tracing` subscriber nobody has installed.
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

/// Log every degradation notice in `interleaved` at `warn`.
///
/// The library-side default for callers with nowhere better to put it. A CLI
/// should prefer [`degradation_notices`] and print.
pub fn report_degradation(interleaved: &[FrameKind]) {
    for message in degradation_notices(interleaved) {
        tracing::warn!(
            %message,
            "server reported partial state: a federated satellite contributed nothing",
        );
    }
}

/// Fetch the L3 tag index for every pane in `snapshot` over `conn`.
///
/// Missing, empty, or malformed `phux.tags/v1` values are omitted, as is a
/// pane whose read the server *refuses* — a refusal is not a tag, and this
/// index has no channel to report one.
///
/// # Why one round trip per pane instead of a pipelined batch
///
/// This used to fire every `GET_METADATA` up front and then collect
/// `METADATA_VALUE` frames by request id, which is exactly the hand-rolled
/// correlation the workspace has one engine for now — and it wedged forever
/// on a peer that answered any one of those reads with a correlated `ERROR`,
/// because the `remaining` counter only ever decremented on a
/// `METADATA_VALUE`. Sequential [`Connection::request_metadata`] calls cost
/// one local round trip per pane (tens of microseconds on the UDS these
/// callers use), against a CLI verb that has already paid milliseconds for
/// process start. Buying back a millisecond by keeping a second correlation
/// loop in the tree is the wrong side of that trade.
///
/// `GET_STATE` uses request id 0 on a shared connection, so these start at 1. A
/// snapshot cannot practically hold `u32::MAX` panes; the saturating
/// conversion keeps a malformed fixture panic-free regardless.
///
/// Best-effort: a transport failure returns the entries collected before it.
pub async fn fetch_tag_index(conn: &mut Connection, snapshot: &SessionSnapshot) -> TagIndex {
    let mut index = TagIndex::new();
    for (offset, pane) in snapshot.panes.iter().enumerate() {
        let request_id = u32::try_from(offset).unwrap_or(u32::MAX).saturating_add(1);
        let Ok(reply) = conn
            .request_metadata(
                request_id,
                Scope::Terminal(pane.id.clone()),
                TERMINAL_TAGS_KEY.to_owned(),
            )
            .await
        else {
            return index;
        };
        let (answer, interleaved) = reply.into_parts();
        // A hub pushes its per-satellite degradation notices onto whatever
        // connection is next to hear from it, so they can surface here rather
        // than on the GET_STATE that preceded this.
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

/// Resolve a selector against a snapshot, fetching L3 tags only for `#tag`.
///
/// Non-tag selectors are resolved synchronously without an extra connection.
/// A tag lookup failure degrades to an empty index, preserving the established
/// CLI behavior that reports the result as a selector miss.
pub async fn resolve_targets(
    socket: &Path,
    selector: &Selector,
    snapshot: &SessionSnapshot,
) -> Vec<TerminalId> {
    if !matches!(selector, Selector::Tag(_)) {
        return selector::resolve(selector, snapshot);
    }

    let tags = match Connection::connect(socket).await {
        Ok(mut conn) => fetch_tag_index(&mut conn, snapshot).await,
        Err(_) => TagIndex::new(),
    };
    selector::resolve_with_tags(selector, snapshot, &tags)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests"
)]
mod tests {
    use phux_protocol::ids::{SessionId, TerminalId, WindowId};
    use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
    use phux_protocol::wire::info::{SessionSnapshot, TerminalInfo};
    use tokio::net::UnixListener;

    use super::{Connection, get_state};
    use crate::testkit::{ScriptSpec, ScriptedServer};

    #[tokio::test]
    async fn get_state_skips_unrelated_command_results() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("state.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let expected =
            SessionSnapshot::new(SessionId::new(7), WindowId::new(8), TerminalId::local(9));
        // A COMMAND_RESULT for request 99 belongs to some other pipelined
        // request; the shared harness always emits it AHEAD of this one's
        // ack, because that is the only ordering in which it is a hazard.
        let spec = ScriptSpec::new().foreign_ack(99).state(expected.clone());
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

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
    async fn hub_satellite_degradation_is_not_lost_before_the_get_state_ack() {
        // `handle_get_state_federated` pushes one uncorrelated ERROR per
        // unreachable satellite AHEAD of the merged snapshot's ack, on
        // purpose: "observable degradation, not silence". Every hand-rolled
        // wait loop in the workspace dropped it, so `phux ls`/`kill`/`spatial`
        // against a hub with a dead satellite reported a silently partial
        // fleet as though it were the whole truth. The snapshot must still
        // come back (degradation is not failure) and the notice must be
        // extractable rather than consumed.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("degraded.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let expected =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(2), TerminalId::local(3));
        let spec = ScriptSpec::new()
            .degradation_notice("no satellite route to build-box")
            .state(expected.clone());
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let mut conn = crate::attach::connection::Connection::connect(&socket)
            .await
            .unwrap();
        let (result, interleaved) = conn
            .request(
                0,
                Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            )
            .await
            .unwrap()
            .into_parts();
        assert!(
            matches!(result, CommandResult::OkWith(CommandValue::State(snap)) if snap == expected),
            "a degraded hub still answers with the merged snapshot"
        );
        assert_eq!(
            super::degradation_notices(&interleaved),
            vec!["no satellite route to build-box".to_owned()],
            "the satellite's failure must reach the caller, not the floor"
        );
        // The harness serves until the client hangs up, so the connection has
        // to go before its task can be joined.
        drop(conn);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn get_state_hands_back_the_degradation_with_the_snapshot() {
        // The seam this bead closes: `get_state` used to return a bare
        // snapshot and log the notices at `warn`, which for a CLI verb (no
        // subscriber installed by default) is indistinguishable from silence.
        // The view carries both, so a caller cannot report a half-seen fleet
        // as the whole one without naming the method that drops the evidence.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("view.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let expected =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(2), TerminalId::local(3));
        let spec = ScriptSpec::new()
            .degradation_notice("satellite build-box is unreachable: link is down")
            .state(expected.clone());
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

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
        // The scripted server answers HELLO with HELLO_OK echoing this
        // build's PROTOCOL_VERSION — the reference server's shape. The probe
        // must hand that triple back, and HELLO must be the first (and here
        // only) frame it sends.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("hello.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server =
            tokio::spawn(async move { ScriptedServer::accept(&listener, ScriptSpec::new()).await });

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
        // The listener lives in this test process, so the peer credentials
        // of a connection to it name this very pid — on both Linux
        // (SO_PEERCRED) and macOS (LOCAL_PEEREPID), the two platforms phux
        // ships on. The scripted peer only completes the production
        // constructor's mandatory HELLO negotiation.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("pid.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server =
            tokio::spawn(async move { ScriptedServer::accept(&listener, ScriptSpec::new()).await });

        let conn = Connection::connect(&socket).await.unwrap();
        assert_eq!(
            conn.peer_pid(),
            Some(i32::try_from(std::process::id()).unwrap()),
            "a UDS connection's peer pid must be the listening process"
        );
        drop(conn);
        server.await.unwrap();
    }

    #[test]
    fn degradation_notices_ignores_correlated_errors() {
        // A correlated ERROR is some command's answer (proto.md §9), not a
        // degradation notice — `Connection::request` already resolves the
        // caller's own; anything else belongs to another pipelined request.
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
        // phux-h5hj.12. The pipelined version fired one GET_METADATA per pane
        // up front and decremented a `remaining` counter only on
        // METADATA_VALUE, so a server answering any one of them with a
        // correlated ERROR (`proto.md` 9) left the counter above zero and the
        // caller blocked on `recv` forever. Every `#tag` selector in the CLI
        // routes through here, so the whole verb hung.
        //
        // The timeout is the assertion: on the pre-fix code this test does not
        // fail, it hangs.
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("refusing.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let spec = ScriptSpec::new().refuse_metadata(ErrorCode::PermissionDenied, "policy refused");
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });

        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), TerminalId::local(1))
                .with_panes(vec![
                    TerminalInfo::new(TerminalId::local(1), WindowId::new(1), 80, 24),
                    TerminalInfo::new(TerminalId::local(2), WindowId::new(1), 80, 24),
                ]);
        let mut conn = Connection::connect(&socket).await.unwrap();
        let index =
            tokio::time::timeout(WEDGE_TIMEOUT, super::fetch_tag_index(&mut conn, &snapshot))
                .await
                .expect("a refused read must end the index; a timeout here is the wedge itself");

        // A refusal is not a tag. The pane is simply absent, and the selector
        // it would have matched reports a miss rather than never returning.
        assert!(index.is_empty(), "got {index:?}");
        drop(conn);
        server.await.unwrap();
    }
}
