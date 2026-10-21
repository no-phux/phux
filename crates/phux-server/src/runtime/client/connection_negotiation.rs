async fn dispatch_stream_event(
    event: Option<QuicStreamEvent>,
    stream_events: &mut Option<tokio::sync::mpsc::Receiver<QuicStreamEvent>>,
    state: &SharedState,
    client_id: ClientId,
    plumbing: &mut ClientPlumbing,
    negotiated: Option<&NegotiatedConnection>,
    token: &CancellationToken,
) {
    let Some(event) = event else {
        // Disable the select arm permanently. Polling a closed receiver is
        // immediately ready and would starve control EOF.
        *stream_events = None;
        return;
    };
    handle_stream_event(state, client_id, event, plumbing, negotiated, token).await;
}

async fn wait_initial_hello(deadline: std::pin::Pin<&mut tokio::time::Sleep>, waiting: bool) {
    if waiting {
        deadline.await;
    } else {
        core::future::pending::<()>().await;
    }
}

#[derive(Debug, Clone, Copy)]
struct NegotiatedConnection {
    client_caps: ClientCapabilities,
    profile: BootstrapProfile,
    limits: BootstrapLimits,
    server_features: ServerFeatureSet,
    /// The frame compression HELLO selected (`docs/spec/proto.md` §6.4).
    /// [`Compression::None`] for every consumer that offered nothing, which
    /// is every local one.
    compression: Compression,
}

impl NegotiatedConnection {
    /// QUIC multi-stream: Terminal content rides per-Terminal streams, and
    /// subscriptions start at `STREAM_BIND`.
    const fn quic_streams(self) -> bool {
        self.server_features.contains(ServerFeature::QuicStreams)
    }

    const fn accepts_terminal_reply(self) -> bool {
        self.server_features.contains(ServerFeature::TerminalReply)
    }
}
const fn runtime_server_features() -> ServerFeatureSet {
    // QUIC_STREAMS is transport-gated at HELLO (see negotiate_hello); every
    // other known bit is advertised on every connection.
    ServerFeatureSet::all().without(ServerFeature::QuicStreams)
}

#[cfg(test)]
mod negotiated_feature_tests {
    use super::*;

    fn connection(server_features: ServerFeatureSet) -> NegotiatedConnection {
        NegotiatedConnection {
            client_caps: ClientCapabilities::default(),
            profile: BootstrapProfile::SynthesizedVtRaw,
            limits: BootstrapLimits::default(),
            server_features,
            compression: Compression::None,
        }
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
        let code = |bytes: &BytesMut| {
            validate_dispatch_frame(bytes, None, FrameOrigin::Control, client_id)
                .err()
                .map(|close| close.code)
        };
        assert_eq!(code(&attached), Some(ErrorCode::VersionIncompatible));

        let mut ping = BytesMut::new();
        FrameKind::Ping { nonce: 1 }.encode(&mut ping);
        assert_eq!(code(&ping), None, "PING stays legal before HELLO");
    }

    /// Every known bit but `QUIC_STREAMS` is advertised; an old peer without
    /// the `TERMINAL_REPLY` bit gets its replies refused.
    #[test]
    fn runtime_advertises_every_known_bit_except_quic_streams() {
        let advertised = runtime_server_features();
        assert!(!advertised.contains(ServerFeature::QuicStreams));
        assert_eq!(advertised.iter().count(), ServerFeature::ALL.len() - 1);
        assert!(connection(advertised).accepts_terminal_reply());
        assert!(!connection(ServerFeatureSet::new()).accepts_terminal_reply());
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
                s.set_policy_engine(std::sync::Arc::new(crate::policy::OwnerForAll));
            });
            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            state.with_mut(|s| {
                s.set_connection_identity(
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
}
