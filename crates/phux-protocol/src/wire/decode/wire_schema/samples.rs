//! Encoded samples of every frame variant: shared builders, the connection and input frames, and the sample collector.

use super::{
    ActorRef, AttachTarget, BootstrapId, BootstrapLimits, BootstrapProfile, Bytes, BytesMut,
    ClientCapabilities, ClientId, Compression, CompressionSet, DetachReason, ErrorCode,
    FRAME_COMPRESSED, FocusEvent, FrameKind, IdempotencyKey, KeyAction, KeyEvent, ModSet,
    MouseAction, MouseButton, MouseEvent, PasteEvent, PasteTrust, PhysicalKey, ResourceId,
    RolePolicy, ServerCapabilities, SessionId, SessionSnapshot, SshOrigin, StreamId, ViewportInfo,
    WindowId, decodes, frame_entry, host_samples, lifecycle_samples, metadata_samples,
    spawn_samples, stream_samples,
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
/// present and absent where the encoder can omit it.
pub(super) fn frame_samples() -> Vec<FrameKind> {
    let mut samples = connection_samples();
    samples.extend(input_samples());
    samples.extend(stream_samples());
    samples.extend(metadata_samples());
    samples.extend(host_samples());
    samples.extend(spawn_samples());
    samples.extend(lifecycle_samples());
    samples
}

pub(super) fn hello(client_caps: ClientCapabilities) -> FrameKind {
    FrameKind::Hello {
        client_name: "schema".to_owned(),
        protocol_major: 0,
        protocol_minor: 9,
        protocol_patch: 0,
        client_caps,
    }
}

pub(super) fn hello_ok(server_caps: ServerCapabilities) -> FrameKind {
    FrameKind::HelloOk {
        protocol_major: 0,
        protocol_minor: 9,
        protocol_patch: 0,
        server_caps,
        server_id: b"server".to_vec(),
        selected_profile: BootstrapProfile::SynthesizedVtRaw,
        bootstrap_limits: BootstrapLimits::default(),
    }
}

pub(super) fn attach(role_policy: Option<RolePolicy>) -> FrameKind {
    FrameKind::Attach {
        attach_id: 1,
        target: AttachTarget::ByName("work".to_owned()),
        viewport: ViewportInfo::new(80, 24),
        request_scrollback: true,
        scrollback_limit_lines: 100,
        role_policy,
    }
}

pub(super) fn connection_samples() -> Vec<FrameKind> {
    let origin = SshOrigin {
        client: "192.0.2.1:50000".parse().unwrap(),
        server: Some("192.0.2.2:22".parse().unwrap()),
    };
    vec![
        hello(ClientCapabilities::new()),
        hello(
            ClientCapabilities::new()
                .with_compression(CompressionSet::from_bits(CompressionSet::DEFLATE))
                .with_ssh_origin(origin)
                .with_quic_streams(true),
        ),
        hello_ok(ServerCapabilities::new()),
        hello_ok(ServerCapabilities::new().with_compression(Compression::Deflate)),
        FrameKind::Ping { nonce: 1 },
        FrameKind::Pong { nonce: 1 },
        attach(None),
        attach(Some(RolePolicy::PRIMARY)),
        FrameKind::Detach,
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

pub(super) fn input_samples() -> Vec<FrameKind> {
    vec![
        FrameKind::InputKey {
            terminal_id: terminal(),
            event: KeyEvent {
                action: KeyAction::Press,
                key: PhysicalKey::A,
                mods: ModSet::empty(),
                consumed_mods: ModSet::empty(),
                composing: false,
                text: Some("a".to_owned()),
                unshifted_codepoint: Some(u32::from('a')),
            },
        },
        FrameKind::InputMouse {
            terminal_id: terminal(),
            event: MouseEvent {
                action: MouseAction::Press,
                button: MouseButton::Left,
                mods: ModSet::empty(),
                x: 1.0,
                y: 2.0,
            },
        },
        FrameKind::InputFocus {
            terminal_id: terminal(),
            event: FocusEvent::Gained,
        },
        FrameKind::InputPaste {
            terminal_id: terminal(),
            event: PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"paste".to_vec(),
            },
        },
        FrameKind::InputTerminalReply {
            terminal_id: terminal(),
            bytes: Bytes::from_static(b"\x1b[0n"),
        },
        FrameKind::ViewportResize {
            viewport: ViewportInfo::new(80, 24),
        },
        FrameKind::ResizeTerminal {
            terminal_id: terminal(),
            cols: 80,
            rows: 24,
            cell_px: None,
        },
        FrameKind::ResizeTerminal {
            terminal_id: terminal(),
            cols: 80,
            rows: 24,
            cell_px: Some((8, 16)),
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
