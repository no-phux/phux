//! `SPAWN_RESOURCE` routing: the host reads the requested kind once and hands
//! the whole request to that kind's app, which validates and answers it.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ResourceKindSet,
};
use phux_protocol::ids::{GroupId, ResourceId as WireResourceId, SatelliteHost};
use phux_protocol::wire::frame::{ErrorCode, FrameKind, SpawnError, SpawnResource};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::resource::ResourceHandle;
use crate::runtime::spawn_reply::refuse_spawn;
use crate::state::{ClientId, Outbound, SharedState};

/// The decoded `SPAWN_RESOURCE` payload, minus `request_id`.
#[derive(Debug)]
pub(crate) struct SpawnRequest {
    /// Group under which to spawn (v0.1 servers expose `GroupId(1)`).
    pub(crate) group: GroupId,
    /// Command + argv, or `None` for the server's default shell.
    pub(crate) command: Option<Vec<String>>,
    /// Working directory, or `None` for the server's default policy.
    pub(crate) cwd: Option<String>,
    /// Environment pairs, `None` = inherit the server's environment.
    pub(crate) env: Option<Vec<(String, String)>>,
    /// First-class `TERM` override.
    pub(crate) term: Option<String>,
    /// Satellite host to route the spawn to, `None` = local.
    pub(crate) satellite: Option<SatelliteHost>,
    /// Existing Terminal on the spawn's host whose exact window owns the new pane.
    pub(crate) owner_terminal: Option<WireResourceId>,
    /// Opaque native agent-session provenance to install before publication.
    pub(crate) agent_session: Option<Vec<u8>>,
    /// `(cols, rows)` for the pane's grid and PTY, `None` for the default.
    pub(crate) initial_size: Option<(u16, u16)>,
    /// Kind, parent, and provenance fields; `None` is a plain Terminal.
    pub(crate) resource: Option<Box<SpawnResource>>,
    /// The kinds the spawner's `HELLO` says it decodes; not part of the
    /// request's identity.
    pub(crate) decodes: ResourceKindSet,
}

/// The process-launch fields of a `SPAWN_RESOURCE`. Only a kind that starts a
/// process reads them; the others ignore them, as the wire always has.
#[derive(Debug)]
pub(crate) struct SpawnLaunch {
    /// See [`SpawnRequest::group`].
    pub(crate) group: GroupId,
    /// See [`SpawnRequest::command`].
    pub(crate) command: Option<Vec<String>>,
    /// See [`SpawnRequest::cwd`].
    pub(crate) cwd: Option<String>,
    /// See [`SpawnRequest::env`].
    pub(crate) env: Option<Vec<(String, String)>>,
    /// See [`SpawnRequest::term`].
    pub(crate) term: Option<String>,
    /// See [`SpawnRequest::owner_terminal`].
    pub(crate) owner_terminal: Option<WireResourceId>,
    /// See [`SpawnRequest::agent_session`].
    pub(crate) agent_session: Option<Vec<u8>>,
    /// See [`SpawnRequest::initial_size`].
    pub(crate) initial_size: Option<(u16, u16)>,
}

/// One `SPAWN_RESOURCE` addressed to an app's kind.
#[derive(Debug)]
pub(crate) struct ResourceSpawn<'a> {
    /// Server state.
    pub(crate) state: &'a SharedState,
    /// The spawning connection.
    pub(crate) client_id: ClientId,
    /// The request to answer.
    pub(crate) request_id: u32,
    /// Kind, parent, and provenance.
    pub(crate) resource: &'a SpawnResource,
    /// Satellite to relay to, `None` for local.
    pub(crate) satellite: Option<&'a SatelliteHost>,
    /// The process-launch fields.
    pub(crate) launch: SpawnLaunch,
    /// Where the reply and the stream's frames go.
    pub(crate) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// The server's cancellation.
    pub(crate) root_token: &'a CancellationToken,
    /// The Terminal stream profile the connection negotiated, if any; the
    /// host refuses a kind whose stream needs one before this is `None`.
    pub(crate) profile: Option<BootstrapStreamProfile>,
    /// The negotiated bootstrap limits.
    pub(crate) limits: BootstrapLimits,
    /// The connection's cancellation.
    pub(crate) connection_token: &'a CancellationToken,
    /// The connection's output pumps, for a stream the spawn starts.
    pub(crate) output_pumps: &'a mut JoinSet<()>,
    /// Whether content waits for `STREAM_BIND` (QUIC multi-stream, L1 §4.9).
    pub(crate) defer_subscription: bool,
    /// The kinds the spawner decodes. It stays the producer of what it
    /// spawns either way; it is streamed the resource only if listed.
    pub(crate) decodes: ResourceKindSet,
}

/// What one connection lends a spawn it sent.
pub(crate) struct SpawnConnection<'a> {
    /// Where the reply and the stream's frames go.
    pub(crate) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// The negotiated bootstrap profile, if any.
    pub(crate) profile: Option<BootstrapProfile>,
    /// The negotiated bootstrap limits.
    pub(crate) limits: BootstrapLimits,
    /// The server's cancellation.
    pub(crate) root_token: &'a CancellationToken,
    /// The connection's cancellation.
    pub(crate) connection_token: &'a CancellationToken,
    /// The connection's output pumps.
    pub(crate) output_pumps: &'a mut JoinSet<()>,
    /// Whether content waits for `STREAM_BIND`.
    pub(crate) defer_subscription: bool,
}

/// Serve `SPAWN_RESOURCE` (SPEC §10.1) through the app of the requested
/// kind. An unrecognised kind is refused, never spawned as a Terminal. The
/// spawner is subscribed to what it spawns, so a connection with no
/// Terminal profile is refused a kind whose catalog stream contract needs
/// one; any other kind is spawned and streamed without it.
pub(crate) async fn dispatch_spawn(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    request: SpawnRequest,
    connection: SpawnConnection<'_>,
) {
    let SpawnConnection {
        out_tx,
        profile,
        limits,
        root_token,
        connection_token,
        output_pumps,
        defer_subscription,
    } = connection;
    log_spawn_request(client_id, request_id, &request);
    let SpawnRequest {
        group,
        command,
        cwd,
        env,
        term,
        satellite,
        owner_terminal,
        agent_session,
        initial_size,
        resource,
        decodes,
    } = request;
    // An absent record is a plain Terminal: the default record's kind.
    let resource = resource.unwrap_or_default();
    let Some(kind) = crate::id_bridge::core_kind(resource.kind) else {
        warn!(
            ?client_id,
            request_id,
            kind = ?resource.kind,
            "SPAWN_RESOURCE: unknown kind"
        );
        refuse_spawn(out_tx, request_id, SpawnError::UnsupportedKind).await;
        return;
    };
    let stream_profile = profile.map(BootstrapProfile::terminal_stream);
    if stream_profile.is_none() && phux_protocol::kinds::needs_terminal_profile(resource.kind) {
        refuse_without_terminal_profile(out_tx, request_id).await;
        return;
    }
    super::app_for(kind)
        .spawn(ResourceSpawn {
            state,
            client_id,
            request_id,
            resource: &resource,
            satellite: satellite.as_ref(),
            launch: SpawnLaunch {
                group,
                command,
                cwd,
                env,
                term,
                owner_terminal,
                agent_session,
                initial_size,
            },
            out_tx,
            root_token,
            profile: stream_profile,
            limits,
            connection_token,
            output_pumps,
            defer_subscription,
            decodes,
        })
        .await;
}

/// `CODEC_UNAVAILABLE`: the connection selected no Terminal profile at
/// `HELLO`, and the spawn's stream needs one.
pub(crate) async fn refuse_without_terminal_profile(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    request_id: u32,
) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::CodecUnavailable,
            message: super::NO_TERMINAL_PROFILE.to_owned(),
        }))
        .await;
}

fn log_spawn_request(client_id: ClientId, request_id: u32, request: &SpawnRequest) {
    debug!(
        ?client_id,
        request_id,
        group = ?request.group,
        command = ?request.command,
        cwd = ?request.cwd,
        env_count = request.env.as_ref().map_or(0, Vec::len),
        satellite = ?request.satellite,
        owner_terminal = ?request.owner_terminal,
        initial_size = ?request.initial_size,
        "SPAWN_RESOURCE",
    );
}

/// Whether `handle`'s resource can parent a producer-fed projection: it
/// supervises the producer and carries the activation the producer is
/// checked against, which its handle says by carrying an
/// [`AgentParent`](phux_agent_session::parent::AgentParent) companion
/// (docs/resource-kinds.md, Producers). No kind is named here.
pub(crate) fn supervises_producers(handle: &ResourceHandle) -> bool {
    handle
        .companion::<phux_agent_session::parent::AgentParent>()
        .is_some()
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use phux_protocol::ids::ResourceKind;
    use phux_protocol::wire::frame::SpawnResult;

    use super::*;

    fn request(kind: ResourceKind) -> SpawnRequest {
        SpawnRequest {
            group: crate::state::DEFAULT_GROUP_ID,
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: Some(Box::new(SpawnResource {
                kind,
                ..SpawnResource::default()
            })),
            decodes: ResourceKindSet::LEGACY,
        }
    }

    async fn reply(kind: ResourceKind, profile: Option<BootstrapProfile>) -> FrameKind {
        reply_to(request(kind), profile).await
    }

    /// The first reply `dispatch_spawn` sends for `request` on a connection
    /// that negotiated `profile`.
    async fn reply_to(request: SpawnRequest, profile: Option<BootstrapProfile>) -> FrameKind {
        reply_in(&SharedState::new(), request, profile).await
    }

    /// [`reply_to`] against `state`.
    async fn reply_in(
        state: &SharedState,
        request: SpawnRequest,
        profile: Option<BootstrapProfile>,
    ) -> FrameKind {
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(8);
        let token = CancellationToken::new();
        let mut pumps = JoinSet::new();
        dispatch_spawn(
            state,
            ClientId(1),
            7,
            request,
            SpawnConnection {
                out_tx: &out_tx,
                profile,
                limits: BootstrapLimits::default(),
                root_token: &token,
                connection_token: &token,
                output_pumps: &mut pumps,
                defer_subscription: false,
            },
        )
        .await;
        let Some(Outbound::Frame(frame)) = out_rx.recv().await else {
            panic!("dispatch_spawn always answers");
        };
        frame
    }

    fn refusal(frame: FrameKind) -> SpawnError {
        let FrameKind::ResourceSpawned {
            request_id: 7,
            result: SpawnResult::Err(error),
        } = frame
        else {
            panic!("a typed refusal: {frame:?}");
        };
        error
    }

    /// With no Terminal profile only a kind whose stream needs one is
    /// refused `CODEC_UNAVAILABLE`; every other kind reaches its app, and an
    /// unknown kind is still refused as unknown.
    #[tokio::test]
    async fn no_profile_refuses_only_a_kind_that_needs_one() {
        let FrameKind::Error {
            request_id: Some(7),
            code: ErrorCode::CodecUnavailable,
            message,
        } = reply(ResourceKind::Terminal, None).await
        else {
            panic!("CODEC_UNAVAILABLE for a Terminal");
        };
        assert_eq!(message, super::super::NO_TERMINAL_PROFILE);
        let SpawnError::SpawnFailed(agent) = refusal(reply(ResourceKind::AgentSession, None).await)
        else {
            panic!("the agent session app refuses");
        };
        assert!(agent.contains("agent session"), "{agent}");
        for kind in [
            ResourceKind::AccessibilityTree,
            ResourceKind::PixelSurface,
            ResourceKind::Document,
        ] {
            let SpawnError::SpawnFailed(document) = refusal(reply(kind, None).await) else {
                panic!("the document app refuses {kind:?}");
            };
            assert!(document.contains("producer observes"), "{document}");
        }
        assert_eq!(
            refusal(reply(ResourceKind::Unknown { tag: 99 }, None).await),
            SpawnError::UnsupportedKind
        );
    }

    /// A document's spawner becomes its producer, so only a connection that
    /// may publish (the Unix socket or localhost) opens one: an unstamped
    /// or remote one is refused before the parent is looked up.
    #[tokio::test]
    async fn only_a_connection_that_may_publish_opens_a_document() {
        use phux_protocol::policy::TransportType;
        let profile = Some(BootstrapProfile::SynthesizedVtRaw);
        for transport in [
            None,
            Some(TransportType::Quic),
            Some(TransportType::WebSocket),
        ] {
            let state = SharedState::new();
            if let Some(transport) = transport {
                let peer = phux_protocol::policy::PeerIdentity {
                    transport,
                    ..crate::runtime::test_support::owner_peer()
                };
                state.with_mut(|s| s.set_peer_identity(ClientId(1), peer));
            }
            let SpawnError::SpawnFailed(message) =
                refusal(reply_in(&state, document_request(), profile).await)
            else {
                panic!("{transport:?} opened a document");
            };
            assert!(message.contains("local owner connection"), "{message}");
        }
        let state = SharedState::new();
        state.with_mut(|s| {
            s.set_peer_identity(ClientId(1), crate::runtime::test_support::owner_peer());
        });
        assert_eq!(
            refusal(reply_in(&state, document_request(), profile).await),
            SpawnError::ParentNotFound,
            "the owner's socket reaches the parent lookup"
        );
    }

    fn document_request() -> SpawnRequest {
        let mut request = request(ResourceKind::Document);
        request.resource = Some(Box::new(SpawnResource::document(
            WireResourceId::local(3),
            "text/plain",
            None,
        )));
        request
    }

    #[tokio::test]
    async fn an_unknown_kind_is_refused_not_spawned_as_a_terminal() {
        let frame = reply(
            ResourceKind::Unknown { tag: 99 },
            Some(BootstrapProfile::SynthesizedVtRaw),
        )
        .await;
        assert_eq!(refusal(frame), SpawnError::UnsupportedKind);
    }

    /// Each kind's own app answers: an agent session and a document refuse
    /// a missing producer name, a Terminal refuses a parent.
    #[tokio::test]
    async fn each_kind_reaches_its_own_app() {
        let profile = Some(BootstrapProfile::SynthesizedVtRaw);
        let SpawnError::SpawnFailed(agent) =
            refusal(reply(ResourceKind::AgentSession, profile).await)
        else {
            panic!("the agent session app refuses");
        };
        assert!(agent.contains("agent session"), "{agent}");
        for kind in [
            ResourceKind::AccessibilityTree,
            ResourceKind::PixelSurface,
            ResourceKind::Document,
        ] {
            let SpawnError::SpawnFailed(document) = refusal(reply(kind, profile).await) else {
                panic!("the document app refuses {kind:?}");
            };
            assert!(document.contains("producer observes"), "{document}");
        }

        let mut terminal = request(ResourceKind::Terminal);
        if let Some(resource) = terminal.resource.as_mut() {
            resource.parent = Some(WireResourceId::local(3));
        }
        let frame = reply_to(terminal, profile).await;
        assert_eq!(refusal(frame), SpawnError::ParentKindMismatch);
    }
}
