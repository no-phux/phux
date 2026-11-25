use crate::runtime::connections::commands::{Undispatched, validate_dispatch_frame};
use crate::transport::FrameOrigin;
use phux_protocol::ids::ResourceId as WireResourceId;

use super::*;

fn connection(server_features: ServerFeatureSet) -> NegotiatedConnection {
    NegotiatedConnection {
        client_caps: ClientCapabilities::default(),
        profile: Some(BootstrapProfile::SynthesizedVtRaw),
        limits: BootstrapLimits::default(),
        server_features,
        compression: Compression::None,
    }
}

#[test]
fn larger_history_pages_are_local_and_leave_hub_limits_unchanged() {
    let state = SharedState::new();
    let larger = BootstrapLimits::clamped(
        phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES,
        phux_protocol::MAX_HISTORY_PAGE_BYTES,
    );
    let requested = ClientCapabilities::default()
        .with_bootstrap(phux_protocol::caps::BootstrapCapabilities::new().with_limits(larger));
    let limits = |caps: &ClientCapabilities, transport| {
        select_hello_profile(&state, caps, ClientId(1), transport)
            .map_err(|error| error.message)
            .unwrap()
            .1
    };
    assert_eq!(limits(&requested, TransportType::UnixSocket), larger);
    assert_eq!(
        limits(&requested, TransportType::Quic),
        BootstrapLimits::default()
    );
    assert_eq!(
        limits(&ClientCapabilities::default(), TransportType::UnixSocket),
        BootstrapLimits::default()
    );
    state.with_mut(|s| {
        s.federation_mut()
            .set_table(crate::hub::HubTable::default());
    });
    assert_eq!(
        limits(&requested, TransportType::UnixSocket),
        BootstrapLimits::default()
    );
}

#[test]
fn negotiated_resize_uses_the_terminal_stream() {
    let client_id = ClientId(1);
    let mut bytes = BytesMut::new();
    FrameKind::ResizeTerminal {
        terminal_id: WireResourceId::local(1),
        cols: 80,
        rows: 24,
        cell_px: None,
    }
    .encode(&mut bytes);
    let multistream = connection(ServerFeatureSet::with(&[ServerFeature::QuicStreams]));
    let legacy = connection(ServerFeatureSet::new());
    let validate = |selection, origin| {
        validate_dispatch_frame(&bytes, Some(selection), origin, client_id).is_ok()
    };
    assert!(!validate(&multistream, FrameOrigin::Control));
    assert!(validate(&multistream, FrameOrigin::Terminal));
    assert!(validate(&legacy, FrameOrigin::Control));
}

/// Before HELLO a frame is refused by its type byte, before its body is
/// decoded: a peer that has not said HELLO never reaches the other body
/// decoders (here a server-to-client snapshot declaring a huge list).
#[test]
fn pre_hello_frames_are_refused_before_body_decode() {
    let client_id = ClientId(1);
    let mut attached = BytesMut::new();
    attached.extend_from_slice(&6_u32.to_be_bytes());
    attached.extend_from_slice(&[phux_protocol::wire::frame::TYPE_ATTACHED, 1, 4, 4]);
    attached.extend_from_slice(&[0xFF, 0xFF]);
    let code = |bytes: &BytesMut| match validate_dispatch_frame(
        bytes,
        None,
        FrameOrigin::Control,
        client_id,
    ) {
        Ok(_) => None,
        Err(Undispatched::Close(close)) => Some(close.code),
        Err(Undispatched::Refuse(refusal)) => panic!("refused, not closed: {refusal:?}"),
    };
    assert_eq!(code(&attached), Some(ErrorCode::MalformedMessage));

    let mut ping = BytesMut::new();
    FrameKind::Ping { nonce: 1 }.encode(&mut ping);
    assert_eq!(code(&ping), None, "PING stays legal before HELLO");
}

/// ADR-0115: `QUIC_STREAMS` is advertised only on QUIC, and only to a
/// client that opted in.
#[tokio::test]
async fn quic_streams_bit_advertised_on_quic_only() {
    async fn negotiate(transport: TransportType, quic_streams: bool) -> bool {
        let state = SharedState::new();
        // Every transport here, not only the owner's socket, must reach
        // negotiation: an embedder engine admits them all (ADR-0072).
        state.with_mut(|s| {
            s.config_mut().policy_engine = std::sync::Arc::new(crate::policy::OwnerForAll);
        });
        let client_id = state.with_mut(|s| s.clients_mut().new_client_id());
        state.with_mut(|s| {
            s.clients_mut().set_connection_identity(
                client_id,
                crate::auth::ConnectionIdentity {
                    peer: phux_protocol::policy::PeerIdentity {
                        uid: 0,
                        pid: None,
                        exe_path: None,
                        mcp_host_key: None,
                        transport,
                        source_addr: None,
                    },
                    credential: None,
                    ssh_origin: None,
                    bearer: None,
                },
            );
        });
        let (out_tx, _rx) = tokio::sync::mpsc::channel(8);
        let mut negotiated = None;
        negotiate_hello(
            &state,
            client_id,
            &out_tx,
            HelloRequest {
                client_name: "test".to_owned(),
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                client_caps: ClientCapabilities::default().with_quic_streams(quic_streams),
            },
            &mut negotiated,
            transport,
            matches!(transport, TransportType::Quic),
        )
        .await
        .map_err(|close| close.message)
        .expect("compatible HELLO negotiates");
        negotiated
            .expect("negotiation caches the selection")
            .server_features
            .contains(ServerFeature::QuicStreams)
    }

    assert!(negotiate(TransportType::Quic, true).await);
    assert!(!negotiate(TransportType::Quic, false).await);
    for transport in [
        TransportType::UnixSocket,
        TransportType::SshTunnel,
        TransportType::WebSocket,
        TransportType::WebTransport,
    ] {
        assert!(!negotiate(transport, true).await, "{transport:?}");
    }
}
