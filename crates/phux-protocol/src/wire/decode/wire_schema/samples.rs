//! Encoded samples of every frame variant: shared builders, the server's
//! connection frames, and the sample collector.

use super::{
    ActorRef, BootstrapId, BootstrapLimits, BootstrapProfile, Bytes, BytesMut, ClientId,
    Compression, DetachReason, ErrorCode, FRAME_COMPRESSED, FrameKind, IdempotencyKey, ResourceId,
    ServerCapabilities, SessionId, SessionSnapshot, StreamId, WindowId, decodes, frame_entry,
    host_samples, lifecycle_samples, metadata_samples, reply_samples, spawn_samples,
    stream_samples,
};

pub(super) fn terminal() -> ResourceId {
    ResourceId::local(7)
}

pub(super) fn stream() -> StreamId {
    StreamId::new(3).unwrap()
}

pub(super) fn generation() -> BootstrapId {
    BootstrapId::new(5).unwrap()
}

pub(super) fn key() -> IdempotencyKey {
    IdempotencyKey::new([0x5A; 16]).unwrap()
}

pub(super) fn actor() -> ActorRef {
    ActorRef::new(ClientId::new(7))
        .with_credential_id(Some("cred".to_owned()))
        .with_client_name(Some("cli".to_owned()))
}

/// Every [`FrameKind`] variant at least once; every optional field both
/// present and absent where the encoder can omit it. Client frames come
/// from the classifier's per-variant samples, less those the decoder
/// refuses (a spawn whose kind contradicts its binding), which show nothing
/// about presence.
pub(super) fn frame_samples() -> Vec<FrameKind> {
    let mut samples: Vec<FrameKind> = crate::kinds::samples::frame_samples()
        .into_iter()
        .map(|(frame, _)| frame)
        .filter(|frame| decodes(&encoded(frame)))
        .collect();
    samples.extend(connection_samples());
    samples.extend(stream_samples());
    samples.extend(metadata_samples());
    samples.extend(host_samples());
    samples.extend(spawn_samples());
    samples.extend(lifecycle_samples());
    samples.extend(reply_samples());
    samples
}

fn hello_ok(server_caps: ServerCapabilities) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 9,
        protocol_patch: 0,
        server_caps,
        server_id: b"server".to_vec(),
        selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
        bootstrap_limits: BootstrapLimits::default(),
    }
}

/// The server's handshake, attach, and error frames.
fn connection_samples() -> Vec<FrameKind> {
    vec![
        hello_ok(ServerCapabilities::new()),
        hello_ok(ServerCapabilities::new().with_compression(Compression::Deflate)),
        hello_ok(
            ServerCapabilities::new().with_features_ext2(crate::caps::ServerFeatureExt2Set::all()),
        ),
        FrameKind::Pong { nonce: 1 },
        FrameKind::Attached {
            attach_id: 1,
            snapshot: SessionSnapshot::new(SessionId::new(1), WindowId::new(1), terminal()),
            initial_client_id: ClientId::new(7),
        },
        FrameKind::AttachReady { attach_id: 1 },
        FrameKind::Detached {
            reason: None,
            message: String::new(),
        },
        FrameKind::Detached {
            reason: Some(DetachReason::ServerShutdown),
            message: "stopping".to_owned(),
        },
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::InternalError,
            message: String::new(),
        },
        FrameKind::Error {
            request_id: Some(9),
            code: ErrorCode::InvalidCommand,
            message: "bad".to_owned(),
        },
    ]
}

/// One sampled frame on the wire, with the spec name and field module its
/// type byte belongs to.
pub(super) struct Sample {
    pub(super) name: &'static str,
    pub(super) module: &'static str,
    pub(super) bytes: Vec<u8>,
}

pub(super) fn encoded(frame: &FrameKind) -> Vec<u8> {
    let mut out = BytesMut::new();
    frame.encode(&mut out);
    out.to_vec()
}

/// Every sample on the wire, `FRAME_COMPRESSED` included.
pub(super) fn samples() -> Vec<Sample> {
    let mut out: Vec<Sample> = frame_samples()
        .iter()
        .map(|frame| {
            let (_, name, module) = frame_entry(frame);
            Sample {
                name,
                module,
                bytes: encoded(frame),
            }
        })
        .collect();
    // Repetitive enough that deflate shrinks it, so the envelope is emitted.
    let inner = FrameKind::ResourceOutput {
        terminal_id: terminal(),
        stream_id: stream(),
        bootstrap_id: generation(),
        seq: 1,
        bytes: Bytes::from(vec![b'x'; 4096]),
    };
    let (mut scratch, mut bytes) = (BytesMut::new(), BytesMut::new());
    inner.encode_compressed(Compression::Deflate, &mut scratch, &mut bytes);
    out.push(Sample {
        name: FRAME_COMPRESSED.0,
        module: FRAME_COMPRESSED.1,
        bytes: bytes.to_vec(),
    });
    // Presence is inferred from what decodes, so a sample the decoder
    // rejects would be evidence of nothing.
    for sample in &out {
        assert!(
            decodes(&sample.bytes),
            "{} sample does not decode",
            sample.name
        );
    }
    out
}
