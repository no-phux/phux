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
            selected_profile: Some(phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw),
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

/// A native-profile session (the listing bit marks it negotiated).
fn native_session() -> RelaySession {
    RelaySession::new_negotiated(
        host(),
        BootstrapLimits::default(),
        native_profile(),
        ServerFeatureSet::with(&[ServerFeature::ListDirectory]),
    )
}

pub(super) fn native_stream_profile() -> BootstrapStreamProfile {
    BootstrapStreamProfile::NativeState {
        codec: phux_protocol::caps::EngineCodec::LibghosttyCheckpointV2,
    }
}

#[test]
fn relay_rejects_bootstrap_profile_mismatch_before_fanout() {
    let mut session = native_session();
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

/// One native bootstrap generation of Terminal 7, as satellite frames.
#[derive(Clone, Copy)]
struct Flow {
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
}

impl Flow {
    fn new(stream: u64, bootstrap: u64) -> Self {
        Self {
            stream_id: StreamId::new(stream).expect("stream"),
            bootstrap_id: BootstrapId::new(bootstrap).expect("bootstrap"),
        }
    }

    fn begin(self, base_seq: u64) -> FrameKind {
        FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(7),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            profile: native_stream_profile(),
            cols: 80,
            rows: 24,
            base_seq,
        }
    }

    fn chunk(self, chunk_seq: u32) -> FrameKind {
        FrameKind::BootstrapChunk {
            terminal_id: ResourceId::local(7),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            chunk_seq,
            payload: bytes::Bytes::from_static(b"opaque"),
        }
    }

    fn ready(self) -> FrameKind {
        FrameKind::BootstrapReady {
            terminal_id: ResourceId::local(7),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            history_cursor: None,
        }
    }

    fn output(self, seq: u64, bytes: &'static [u8]) -> FrameKind {
        FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(7),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            seq,
            bytes: bytes::Bytes::from_static(bytes),
        }
    }
}

#[test]
fn relay_rejects_gapped_chunks_and_live_delta_before_ready() {
    let flow = Flow::new(2, 3);
    for (label, frame, needle) in [
        ("gapped chunk", flow.chunk(1), "expected 0"),
        (
            "live output before READY",
            flow.output(11, b"x"),
            "before BOOTSTRAP_READY",
        ),
    ] {
        let mut session = native_session();
        session.feed(&flow.begin(10));
        let error = session.handle_inbound(&encode(&frame)).expect_err(label);
        assert!(error.contains(needle), "{label}: {error}");
    }
}

#[test]
fn relay_fans_out_complete_native_prefix_before_live_delta() {
    let mut session = native_session();
    let (out_tx, mut out_rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), out_tx);
    let flow = Flow::new(4, 5);
    for frame in [
        flow.begin(20),
        flow.chunk(0),
        flow.ready(),
        flow.output(21, b"live"),
    ] {
        session.feed(&frame);
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
    let active = session.bootstrap_flows.get(&7).expect("active generation");
    assert!(active.ready);
    assert_eq!(active.stream_id, flow.stream_id);
    assert_eq!(active.bootstrap_id, flow.bootstrap_id);
    assert_eq!(active.profile, native_stream_profile());
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
            awaits_snapshot: true,
            bootstrap_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: Some(limits),
            ..proxy(7, ClientId(1), out_tx)
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
            awaits_snapshot: true,
            bootstrap_profile: Some(native_profile()),
            bootstrap_limits: Some(smaller),
            ..proxy(8, ClientId(2), out_tx)
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
    let mut session = native_session();
    let (out_tx, mut out_rx) = mpsc::channel(8);
    attach(&mut session, 7, ClientId(1), out_tx);
    let flow = Flow::new(4, 5);
    session.feed(&flow.begin(20));
    session.feed(&flow.ready());
    let _ = out_rx.try_recv().expect("BEGIN");
    let _ = out_rx.try_recv().expect("READY");
    let before = session.bootstrap_flows[&7];
    let other_generation = Flow {
        bootstrap_id: BootstrapId::new(6).expect("different bootstrap"),
        ..flow
    };
    let error = session
        .handle_inbound(&encode(&other_generation.output(21, b"invalid")))
        .expect_err("identity change must terminate the link");
    assert!(error.contains("non-active bootstrap identity"));
    assert!(out_rx.try_recv().is_err(), "invalid frame reached consumer");
    assert_eq!(session.bootstrap_flows[&7], before);
}

#[test]
fn relay_passes_a_document_stream_the_satellite_advertises() {
    let begin = || {
        encode(&FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(7),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
            profile: BootstrapStreamProfile::AccessibilityTreeJsonV1,
            cols: 0,
            rows: 0,
            base_seq: 1,
        })
    };
    let session_with = |ext: &[ServerFeatureExt]| {
        let mut session = native_session();
        session.set_satellite_features_ext(ServerFeatureExtSet::with(ext));
        session
    };

    let mut trees = session_with(&[ServerFeatureExt::AccessibilityTree]);
    let (out_tx, mut out_rx) = mpsc::channel(8);
    attach(&mut trees, 7, ClientId(1), out_tx);
    trees
        .handle_inbound(&begin())
        .expect("an advertised document stream relays under any terminal profile");
    assert!(
        out_rx.try_recv().is_ok(),
        "the tree's BEGIN reached the viewer"
    );

    let mut silent = session_with(&[]);
    let (out_tx, _out_rx) = mpsc::channel(8);
    attach(&mut silent, 7, ClientId(1), out_tx);
    assert!(
        silent.handle_inbound(&begin()).is_err(),
        "a kind the satellite never advertised is still refused"
    );
}
