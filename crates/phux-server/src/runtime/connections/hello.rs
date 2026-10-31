use super::commands::Undispatched;
use super::lifecycle::{incompatible_protocol_message, protocol_is_compatible};
use super::plumbing::{ConnectionClose, framing_violation};
use super::unsupported::undecodable;
use crate::auth::Standing;
use crate::state::{ClientId, Outbound, SharedState};
use bytes::BytesMut;
use phux_protocol::PROTOCOL_VERSION;
#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::ServerFeature;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, Compression, LayerSet, QUIC_STREAMS,
    ServerCapabilities, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::policy::TransportType;
use phux_protocol::wire::frame::{DetachReason, ErrorCode, FrameKind};
use std::io;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

/// SPEC §5: a framing violation is answered with `FRAME_TOO_LARGE` before
/// closing. `None` for every other read error: the transport died.
pub(super) fn framing_violation_close(
    err: &io::Error,
    client_id: ClientId,
) -> Option<ConnectionClose> {
    let framing = framing_violation(err)?;
    warn!(?client_id, error = %framing, "client framing violation; closing");
    Some(ConnectionClose {
        attached_reason: Some("framing violation"),
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::FrameTooLarge,
        message: framing.wire_message(),
    })
}

/// Decode one framed message. Once HELLO has been negotiated the connection's
/// selected limits apply, so an oversized borrowed payload is rejected before
/// the decoder copies it into owned storage.
pub(super) fn decode_client_frame(
    framed: &BytesMut,
    negotiated: Option<&NegotiatedConnection>,
) -> Result<FrameKind, Undispatched> {
    // FRAME_COMPRESSED is server-to-client only (docs/protocol.md#framing); refuse the
    // envelope before decode. The type byte follows the u32 length header.
    if framed.get(4) == Some(&phux_protocol::wire::frame::TYPE_FRAME_COMPRESSED) {
        warn!("client sent FRAME_COMPRESSED; closing");
        return Err(Undispatched::Close(ConnectionClose {
            attached_reason: Some("client-sent FRAME_COMPRESSED"),
            detach_reason: DetachReason::ProtocolError,
            code: ErrorCode::MalformedMessage,
            message: "FRAME_COMPRESSED is server-to-client only".to_owned(),
        }));
    }
    let decoded = negotiated.map_or_else(
        || FrameKind::decode(framed),
        |selection| FrameKind::decode_with_limits(framed, selection.limits),
    );
    decoded
        .map(|(frame, _rest)| frame)
        .map_err(|err| undecodable(&err, framed, negotiated.is_some()))
}

/// Nothing stateful may precede HELLO; PING is a stateless liveness probe.
pub(super) fn reject_frame_before_hello(
    frame: &FrameKind,
    negotiated: bool,
    client_id: ClientId,
) -> Option<ConnectionClose> {
    if negotiated || matches!(frame, FrameKind::Hello { .. } | FrameKind::Ping { .. }) {
        return None;
    }
    Some(before_hello_close(client_id))
}

/// Whether a framed message's type byte is one `PRE_HELLO` admits.
pub(super) fn is_pre_hello_type(framed: &[u8]) -> bool {
    use phux_protocol::wire::frame::{TYPE_HELLO, TYPE_PING};
    matches!(
        framed.get(phux_protocol::wire::LENGTH_PREFIX_LEN),
        Some(&(TYPE_HELLO | TYPE_PING))
    )
}

/// A protocol violation, not a version verdict: clients treat
/// `VERSION_INCOMPATIBLE` as a permanent refusal and stop redialing.
pub(super) fn before_hello_close(client_id: ClientId) -> ConnectionClose {
    warn!(?client_id, "stateful frame before HELLO; closing");
    ConnectionClose {
        attached_reason: None,
        detach_reason: DetachReason::ProtocolError,
        code: ErrorCode::MalformedMessage,
        message: "HELLO required before any stateful frame".to_owned(),
    }
}

/// The HELLO frame's payload.
pub(super) struct HelloRequest {
    pub(super) client_name: String,
    pub(super) protocol_major: u16,
    pub(super) protocol_minor: u16,
    pub(super) protocol_patch: u16,
    pub(in crate::runtime) client_caps: ClientCapabilities,
}

/// Negotiate the connection: version compatibility, the policy engine's
/// verdict on the authenticated peer, and the bootstrap profile both sides can
/// speak. All of it is cached in `negotiated` exactly once — before any
/// stateful frame can be processed — and acknowledged with `HELLO_OK`.
pub(super) async fn negotiate_hello(
    state: &SharedState,
    client_id: ClientId,
    out_tx: &mpsc::Sender<Outbound>,
    hello: HelloRequest,
    negotiated: &mut Option<NegotiatedConnection>,
    transport: TransportType,
    route_supports_quic_streams: bool,
) -> Result<(), ConnectionClose> {
    let HelloRequest {
        client_name,
        protocol_major,
        protocol_minor,
        protocol_patch,
        client_caps,
    } = hello;
    if negotiated.is_some() {
        warn!(?client_id, "duplicate HELLO; closing");
        return Err(ConnectionClose {
            attached_reason: Some("duplicate HELLO"),
            detach_reason: DetachReason::ProtocolError,
            code: ErrorCode::InvalidCommand,
            message: "HELLO already completed on this connection".to_owned(),
        });
    }
    debug!(
        ?client_id,
        %client_name,
        protocol_major,
        protocol_minor,
        protocol_patch,
        color_support = ?client_caps.color_support,
        "HELLO",
    );
    if !protocol_is_compatible(protocol_major) {
        let message = incompatible_protocol_message(protocol_major, protocol_minor, protocol_patch);
        warn!(?client_id, %message, "HELLO protocol mismatch");
        return Err(ConnectionClose {
            attached_reason: None,
            detach_reason: DetachReason::ProtocolError,
            code: ErrorCode::VersionIncompatible,
            message,
        });
    }
    authorize_hello(state, client_id).await?;
    // After authorization, so the announcement cannot influence it.
    state.with_mut(|s| {
        crate::runtime::whoami::admit_ssh_origin(s, client_id, client_caps.ssh_origin);
    });
    let (selected_profile, bootstrap_limits) = select_hello_profile(&client_caps, client_id)?;

    let mut effective_client_caps = client_caps;
    effective_client_caps.output_mode =
        if matches!(selected_profile, BootstrapProfile::SynthesizedVtStateSync) {
            phux_protocol::caps::OutputMode::StateSync
        } else {
            phux_protocol::caps::OutputMode::Raw
        };
    let mut server_features = runtime_server_features();
    // ADR-0115: advertised only where streams exist to back it.
    if matches!(transport, TransportType::Quic)
        && route_supports_quic_streams
        && client_caps.quic_streams
    {
        server_features = ServerFeatureSet::from_wire(server_features.as_wire() | QUIC_STREAMS);
    }
    // Never compress toward a consumer that did not offer to inflate.
    let compression = client_caps.compression.select();

    // Cached once, before any stateful frame; decoding uses these limits.
    *negotiated = Some(NegotiatedConnection {
        client_caps: effective_client_caps,
        profile: selected_profile,
        limits: bootstrap_limits,
        server_features,
        compression,
    });
    state.with_mut(|s| {
        // SPEC §6.2: the L3 arms gate on the negotiated layer set.
        s.set_client_layers(client_id, client_caps.layers);
        // ADR-0123: an actor label, not an authenticated fact.
        s.set_client_name(client_id, client_name);
    });
    let hello_ok = FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps: ServerCapabilities::new()
            .with_layers(LayerSet::all())
            .with_features(server_features)
            .with_features_ext(runtime_server_features_ext())
            .with_compression(compression),
        server_id: state.with(|server| server.server_incarnation().as_bytes().to_vec()),
        selected_profile,
        bootstrap_limits,
    };
    if out_tx.send(Outbound::Frame(hello_ok)).await.is_err() {
        trace!(?client_id, "HELLO_OK send dropped: writer gone");
    }
    Ok(())
}

/// Explicitly implemented extension support, separate from HELLO negotiation.
const fn runtime_server_features_ext() -> ServerFeatureExtSet {
    ServerFeatureExtSet::with(&[
        ServerFeatureExt::PathQuery,
        ServerFeatureExt::ResizeCellPx,
        ServerFeatureExt::CommittedTeardown,
        ServerFeatureExt::AgentStreamResume,
        // Every organization command commits and is enforced.
        ServerFeatureExt::Organization,
        ServerFeatureExt::CreatePlacement,
        ServerFeatureExt::ApiDescription,
        ServerFeatureExt::SessionKill,
        ServerFeatureExt::NativeAgent,
        ServerFeatureExt::NativeAgentLaunch,
        ServerFeatureExt::PerfControl,
        ServerFeatureExt::SessionProject,
        ServerFeatureExt::FileDownload,
        ServerFeatureExt::Checkout,
        ServerFeatureExt::HostServiceHttp,
    ])
}

/// Policy check: authorize HELLO only against the identity authenticated by
/// the accepting transport. A missing registry entry is never equivalent to a
/// local root peer. Every refusal here is HELLO's authentication outcome, so
/// the peer gets `ERROR { PERMISSION_DENIED }` then `DETACHED {
/// AUTHENTICATION_FAILED }` (workload-auth §7).
async fn authorize_hello(state: &SharedState, client_id: ClientId) -> Result<(), ConnectionClose> {
    let identity = state.with(|s| {
        let peer = s.peer_identity(client_id).cloned()?;
        Some((peer, s.authenticated_credential(client_id).cloned()))
    });
    let Some((peer, credential)) = identity else {
        warn!(
            ?client_id,
            "HELLO denied: authenticated peer identity missing"
        );
        return Err(ConnectionClose {
            attached_reason: None,
            detach_reason: DetachReason::AuthenticationFailed,
            code: ErrorCode::PermissionDenied,
            message: "authenticated peer identity missing".to_owned(),
        });
    };
    // A bearer revoked or expired since its upgrade mints nothing
    // (workload-auth §7).
    let bearer = state.with(|s| s.bearer_admission(client_id).cloned());
    if bearer.is_some_and(|bearer| !matches!(bearer.standing(), Some(Standing::Active { .. }))) {
        warn!(
            ?client_id,
            "HELLO denied: the admitting bearer no longer stands"
        );
        return Err(ConnectionClose {
            attached_reason: None,
            detach_reason: DetachReason::AuthenticationFailed,
            code: ErrorCode::PermissionDenied,
            message: "policy denied: unauthorized: not authorized".to_owned(),
        });
    }
    // ADR-0163: a connection that arrived on a delegated credential's own
    // socket acts as that credential, never as the owner the socket's uid
    // would otherwise make it.
    if let Some(id) = crate::runtime::delegation::delegated_credential(&peer, credential.as_ref()) {
        let grant = state.with(|s| s.delegated_grant(id, chrono::Utc::now()));
        let Some(grant) = grant else {
            warn!(
                ?client_id,
                "HELLO denied: the delegated credential is withdrawn or expired"
            );
            return Err(ConnectionClose {
                attached_reason: None,
                detach_reason: DetachReason::AuthenticationFailed,
                code: ErrorCode::PermissionDenied,
                message: "policy denied: unauthorized: not authorized".to_owned(),
            });
        };
        state.with_mut(|s| s.set_connection_grant(client_id, grant));
        return Ok(());
    }
    let engine = state.with(|s| s.policy_engine().clone());
    // workload-auth §5: the grant comes from the verified identity alone.
    match engine.authorize_hello(&peer, credential.as_ref()).await {
        Ok(grant) => {
            state.with_mut(|s| s.set_connection_grant(client_id, grant));
            Ok(())
        }
        Err(err) => {
            warn!(?client_id, error = %err, "HELLO denied by policy");
            Err(ConnectionClose {
                attached_reason: None,
                detach_reason: DetachReason::AuthenticationFailed,
                code: ErrorCode::PermissionDenied,
                message: format!("policy denied: {err}"),
            })
        }
    }
}

/// Select the protocol-0.7 bootstrap profile both peers can speak. `NativeState`
/// requires an exact common codec and every required engine feature, so no
/// common profile is a connection-ending `CodecUnavailable`.
fn select_hello_profile(
    client_caps: &ClientCapabilities,
    client_id: ClientId,
) -> Result<(BootstrapProfile, BootstrapLimits), ConnectionClose> {
    let server_bootstrap = crate::runtime::apps::bootstrap_capabilities();
    let Ok(selection) = select_bootstrap_profile(client_caps, &server_bootstrap) else {
        let message = format!(
            "no common protocol-0.7 bootstrap profile: client profiles=0x{:02x} native_codecs=0x{:016x} native_features=0x{:08x}; server profiles=0x{:02x} native_codecs=0x{:016x} native_features=0x{:08x}. NativeState requires an exact common codec and every required engine feature; advertise SynthesizedVtRaw/SynthesizedVtStateSync or update the incompatible peer",
            client_caps.bootstrap.profiles.as_wire(),
            client_caps.bootstrap.native_codecs.as_wire(),
            client_caps.bootstrap.native_features.as_wire(),
            server_bootstrap.profiles.as_wire(),
            server_bootstrap.native_codecs.as_wire(),
            server_bootstrap.native_features.as_wire(),
        );
        warn!(?client_id, %message, "HELLO codec unavailable");
        return Err(ConnectionClose {
            attached_reason: None,
            detach_reason: DetachReason::ProtocolError,
            code: ErrorCode::CodecUnavailable,
            message,
        });
    };
    Ok(selection)
}

#[derive(Debug, Clone, Copy)]
pub(in crate::runtime) struct NegotiatedConnection {
    pub(in crate::runtime) client_caps: ClientCapabilities,
    pub(in crate::runtime) profile: BootstrapProfile,
    pub(in crate::runtime) limits: BootstrapLimits,
    pub(super) server_features: ServerFeatureSet,
    /// The frame compression HELLO selected (`docs/protocol.md#framing`).
    /// [`Compression::None`] for every consumer that offered nothing, which
    /// is every local one.
    pub(super) compression: Compression,
}

impl NegotiatedConnection {
    /// QUIC multi-stream: Terminal content rides per-Terminal streams, and
    /// subscriptions start at `STREAM_BIND`.
    pub(in crate::runtime) const fn quic_streams(self) -> bool {
        self.server_features.contains(ServerFeature::QuicStreams)
    }

    pub(super) const fn accepts_terminal_reply(self) -> bool {
        self.server_features.contains(ServerFeature::TerminalReply)
    }
}
const fn runtime_server_features() -> ServerFeatureSet {
    // QUIC_STREAMS is transport-gated at HELLO (see negotiate_hello); every
    // other known bit is advertised on every connection.
    ServerFeatureSet::all().without(ServerFeature::QuicStreams)
}

#[cfg(test)]
mod negotiated_feature_tests;
