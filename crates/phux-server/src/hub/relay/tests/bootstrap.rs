//! Relayed bootstrap flows: profiles, chunk order, identity, and content subscriptions.

use super::*;

#[test]
fn post_negotiation_hello_ok_is_fatal_to_relay_session() {
    let mut session = RelaySession::new(host(), BootstrapLimits::default());
    let error = session
        .handle_inbound(&encode(&FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: phux_protocol::caps::ServerCapabilities::new(),
            server_id: Vec::new(),
            selected_profile: phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        }))
        .expect_err("duplicate HELLO_OK must tear down the relay");
    assert!(error.contains("direction-invalid"));
}
pub(super) fn native_profile() -> BootstrapProfile {
    BootstrapProfile::NativeState {
        codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
        features: phux_protocol::caps::EngineFeatureSet::required_native(),
    }
}

pub(super) fn native_stream_profile() -> BootstrapStreamProfile {
    BootstrapStreamProfile::NativeState {
        codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
    }
}

#[test]
fn relay_rejects_bootstrap_profile_mismatch_before_fanout() {
    let mut session = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        native_profile(),
        ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
    );
    let (out_tx, mut out_rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), out_tx);
    let error = session
        .handle_inbound(&encode(&FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        }))
        .expect_err("profile mismatch must fail the link");
    assert!(error.contains("negotiated"));
    assert!(out_rx.try_recv().is_err(), "mismatched BEGIN leaked");
    assert!(session.bootstrap_flows.is_empty());
}

#[test]
fn relay_rejects_gapped_chunks_and_live_delta_before_ready() {
    let stream_id = StreamId::new(2).expect("stream");
    let bootstrap_id = BootstrapId::new(3).expect("bootstrap");

    let mut gapped = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        native_profile(),
        ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
    );
    gapped
        .handle_inbound(&encode(&FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            profile: native_stream_profile(),
            cols: 80,
            rows: 24,
            base_seq: 10,
        }))
        .expect("valid BEGIN");
    let error = gapped
        .handle_inbound(&encode(&FrameKind::BootstrapChunk {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            chunk_seq: 1,
            payload: bytes::Bytes::from_static(b"opaque"),
        }))
        .expect_err("gapped chunk must fail the link");
    assert!(error.contains("expected 0"));

    let mut early_live = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        native_profile(),
        ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
    );
    early_live
        .handle_inbound(&encode(&FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            profile: native_stream_profile(),
            cols: 80,
            rows: 24,
            base_seq: 10,
        }))
        .expect("valid BEGIN");
    let error = early_live
        .handle_inbound(&encode(&FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            seq: 11,
            bytes: bytes::Bytes::from_static(b"x"),
        }))
        .expect_err("live output before READY must fail the link");
    assert!(error.contains("before BOOTSTRAP_READY"));
}

#[test]
fn relay_fans_out_complete_native_prefix_before_live_delta() {
    let mut session = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        native_profile(),
        ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
    );
    let (out_tx, mut out_rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), out_tx);
    let stream_id = StreamId::new(4).expect("stream");
    let bootstrap_id = BootstrapId::new(5).expect("bootstrap");
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            profile: native_stream_profile(),
            cols: 80,
            rows: 24,
            base_seq: 20,
        },
        FrameKind::BootstrapChunk {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::from_static(b"opaque"),
        },
        FrameKind::BootstrapReady {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            seq: 21,
            bytes: bytes::Bytes::from_static(b"live"),
        },
    ] {
        session
            .handle_inbound(&encode(&frame))
            .expect("ordered native bootstrap frame");
    }
    assert!(matches!(
        out_rx.try_recv().expect("BEGIN"),
        Outbound::Frame(FrameKind::BootstrapBegin { .. })
    ));
    assert!(matches!(
        out_rx.try_recv().expect("CHUNK"),
        Outbound::Frame(FrameKind::BootstrapChunk { .. })
    ));
    assert!(matches!(
        out_rx.try_recv().expect("READY"),
        Outbound::Frame(FrameKind::BootstrapReady { .. })
    ));
    assert!(matches!(
        out_rx.try_recv().expect("live"),
        Outbound::Frame(FrameKind::ResourceOutput { seq: 21, .. })
    ));
    let flow = session.bootstrap_flows.get(&7).expect("active generation");
    assert!(flow.ready);
    assert_eq!(flow.stream_id, stream_id);
    assert_eq!(flow.bootstrap_id, bootstrap_id);
    assert_eq!(flow.profile, native_stream_profile());
}

#[test]
fn content_subscription_requires_exact_downstream_profile_and_bounds() {
    let limits = BootstrapLimits::default();
    let mut session = RelaySession::new_negotiated(
        host(),
        limits,
        native_profile(),
        ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
    );
    let (out_tx, _out_rx) = mpsc::channel(8);
    let (reply, mut reply_rx) = oneshot::channel();
    let wire = session.handle_request_checked(RelayRequest::Command {
        command: Command::attach_terminal(ResourceId::local(7), None),
        reply,
        subscribe: Some(ProxySubscription {
            resource_kind: None,
            terminal: 7,
            client: ClientId(1),
            out_tx,
            consumer_cancel: CancellationToken::new(),
            seq: 1,
            awaits_snapshot: true,
            bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: Some(limits),
        }),
    });
    assert!(
        wire.is_none(),
        "mismatched content request reached the link"
    );
    assert!(session.subscribers.is_empty());
    assert!(session.pending.is_empty());
    let result = reply_rx.try_recv().expect("typed local refusal");
    assert!(matches!(
        result,
        CommandResult::Error {
            code: ErrorCode::CodecUnavailable,
            ref message,
        } if message.contains("transcoding is unavailable")
            && message.contains("NativeState")
            && message.contains("SynthesizedVtRaw")
    ));

    let smaller = BootstrapLimits::new(64 * 1024, 128 * 1024).expect("valid bounds");
    let (out_tx, _out_rx) = mpsc::channel(8);
    let (reply, mut reply_rx) = oneshot::channel();
    let wire = session.handle_request_checked(RelayRequest::Command {
        command: Command::attach_terminal(ResourceId::local(8), None),
        reply,
        subscribe: Some(ProxySubscription {
            resource_kind: None,
            terminal: 8,
            client: ClientId(2),
            out_tx,
            consumer_cancel: CancellationToken::new(),
            seq: 1,
            awaits_snapshot: true,
            bootstrap_profile: Some(native_profile()),
            bootstrap_limits: Some(smaller),
        }),
    });
    assert!(wire.is_none(), "mismatched bounds reached the link");
    assert!(matches!(
        reply_rx.try_recv().expect("typed bounds refusal"),
        CommandResult::Error {
            code: ErrorCode::CodecUnavailable,
            ..
        }
    ));
}

#[test]
fn relay_rejects_identity_change_after_ready_without_fanout_or_mutation() {
    let mut session = RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        native_profile(),
        ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
    );
    let (out_tx, mut out_rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), out_tx);
    let stream_id = StreamId::new(4).expect("stream");
    let bootstrap_id = BootstrapId::new(5).expect("bootstrap");
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            profile: native_stream_profile(),
            cols: 80,
            rows: 24,
            base_seq: 20,
        },
        FrameKind::BootstrapReady {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        session
            .handle_inbound(&encode(&frame))
            .expect("valid bootstrap prefix");
    }
    let _ = out_rx.try_recv().expect("BEGIN");
    let _ = out_rx.try_recv().expect("READY");
    let before = session.bootstrap_flows[&7];
    let error = session
        .handle_inbound(&encode(&FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id,
            bootstrap_id: BootstrapId::new(6).expect("different bootstrap"),
            seq: 21,
            bytes: bytes::Bytes::from_static(b"invalid"),
        }))
        .expect_err("identity change must terminate the link");
    assert!(error.contains("non-active bootstrap identity"));
    assert!(out_rx.try_recv().is_err(), "invalid frame reached consumer");
    assert_eq!(session.bootstrap_flows[&7], before);
}
