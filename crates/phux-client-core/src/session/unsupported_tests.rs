//! A stream in a codec this build does not know refuses that one resource:
//! its frames are ignored, and the other resources and the attach carry on.

#![allow(clippy::unwrap_used, reason = "tests")]

use phux_protocol::wire::frame::{CloseReason, TombstoneReason};
use phux_protocol::{BootstrapId, BootstrapProfile, BootstrapStreamProfile, ResourceId, StreamId};

use super::kernel_rig::RigAdapter;
use super::{EffectBuffer, KernelEffect, KernelError, KernelInput, KernelStatus, SessionKernel};
use crate::engine::CanonicalGeometry;

fn stream() -> StreamId {
    StreamId::new(1).unwrap()
}

fn generation() -> BootstrapId {
    BootstrapId::new(1).unwrap()
}

fn begin(
    terminal_id: &ResourceId,
    profile: BootstrapStreamProfile,
    geometry: CanonicalGeometry,
) -> KernelInput<'_> {
    KernelInput::BootstrapBegin {
        terminal_id,
        stream_id: stream(),
        bootstrap_id: generation(),
        profile,
        geometry,
        base_seq: 0,
    }
}

fn chunk<'a>(terminal_id: &'a ResourceId, payload: &'a [u8]) -> KernelInput<'a> {
    KernelInput::BootstrapChunk {
        terminal_id,
        stream_id: stream(),
        bootstrap_id: generation(),
        chunk_seq: 0,
        payload,
    }
}

fn ready(terminal_id: &ResourceId) -> KernelInput<'_> {
    KernelInput::BootstrapReady {
        terminal_id,
        stream_id: stream(),
        bootstrap_id: generation(),
        history_cursor: None,
    }
}

fn output<'a>(terminal_id: &'a ResourceId, payload: &'a [u8]) -> KernelInput<'a> {
    KernelInput::ResourceOutput {
        terminal_id,
        stream_id: stream(),
        bootstrap_id: generation(),
        seq: 1,
        payload,
    }
}

const GRIDLESS: CanonicalGeometry = CanonicalGeometry { cols: 0, rows: 0 };

#[test]
fn an_unknown_codec_refuses_one_resource_and_the_rest_carry_on() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    let terminal = ResourceId::local(1);
    let alien = ResourceId::local(2);
    let document = ResourceId::local(3);
    let grid = CanonicalGeometry::new(80, 24).unwrap();

    // The alien is of no kind the kernel knows, and outside the barrier.
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 7,
                terminals: std::slice::from_ref(&terminal),
            },
            &mut effects,
        )
        .unwrap();
    kernel
        .update(
            begin(
                &alien,
                BootstrapStreamProfile::Unknown { codec_tag: 0x77 },
                GRIDLESS,
            ),
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        effects.as_slice(),
        [KernelEffect::Status(KernelStatus::Unsupported {
            terminal_id: alien.clone(),
            codec_tag: 0x77,
        })]
    );
    assert!(kernel.is_unsupported(&alien));
    for input in [
        chunk(&alien, b"opaque"),
        ready(&alien),
        output(&alien, b"live"),
        KernelInput::Tombstone {
            terminal_id: &alien,
            stream_id: stream(),
            bootstrap_id: generation(),
            reason: TombstoneReason::OutboundGap,
            last_valid_seq: 0,
        },
    ] {
        kernel.update(input, &mut effects).unwrap();
        assert!(effects.as_slice().is_empty());
    }

    kernel
        .update(
            begin(&terminal, BootstrapStreamProfile::SynthesizedVtRaw, grid),
            &mut effects,
        )
        .unwrap();
    kernel
        .update(chunk(&terminal, b"hello"), &mut effects)
        .unwrap();
    kernel.update(ready(&terminal), &mut effects).unwrap();
    kernel
        .update(KernelInput::AttachReady { attach_id: 7 }, &mut effects)
        .unwrap();
    kernel
        .update(output(&terminal, b" world"), &mut effects)
        .unwrap();

    kernel
        .update(
            begin(&document, BootstrapStreamProfile::DocumentTypedV1, GRIDLESS),
            &mut effects,
        )
        .unwrap();
    kernel
        .update(chunk(&document, b"\x0atext/plainhi"), &mut effects)
        .unwrap();
    kernel.update(ready(&document), &mut effects).unwrap();
    assert!(
        effects
            .as_slice()
            .iter()
            .any(|effect| matches!(effect, KernelEffect::Document(_)))
    );

    // A close forgets the refusal; a reused id streams normally.
    kernel
        .update(
            KernelInput::ResourceClosed {
                terminal_id: &alien,
                exit_status: None,
                signal: None,
                reason: CloseReason::Unknown,
            },
            &mut effects,
        )
        .unwrap();
    assert!(!kernel.is_unsupported(&alien));
}

#[test]
fn a_known_bootstrap_lifts_the_refusal() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    let id = ResourceId::local(4);
    kernel
        .update(
            begin(
                &id,
                BootstrapStreamProfile::Unknown { codec_tag: 9 },
                GRIDLESS,
            ),
            &mut effects,
        )
        .unwrap();
    kernel
        .update(
            begin(&id, BootstrapStreamProfile::PixelSurfacePngV1, GRIDLESS),
            &mut effects,
        )
        .unwrap();
    assert!(!kernel.is_unsupported(&id));
    kernel.update(chunk(&id, b"\x89PNG"), &mut effects).unwrap();
    kernel.update(ready(&id), &mut effects).unwrap();
    assert!(matches!(
        effects.as_slice(),
        [KernelEffect::Document(document)] if document.bytes == b"\x89PNG"
    ));
}

#[test]
fn an_unknown_codec_on_a_document_kind_is_refused_too() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    let id = ResourceId::local(5);
    kernel
        .update(
            begin(&id, BootstrapStreamProfile::DocumentTypedV1, GRIDLESS),
            &mut effects,
        )
        .unwrap();
    kernel
        .update(chunk(&id, b"\x0atext/plainhalf"), &mut effects)
        .unwrap();
    kernel
        .update(
            begin(
                &id,
                BootstrapStreamProfile::Unknown { codec_tag: 9 },
                GRIDLESS,
            ),
            &mut effects,
        )
        .unwrap();
    assert!(kernel.is_unsupported(&id));
    kernel.update(ready(&id), &mut effects).unwrap();
    assert!(effects.as_slice().is_empty(), "the half cut was dropped");
}

#[test]
fn a_refused_documents_retired_generations_stay_fenced_after_a_known_recut() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    let id = ResourceId::local(9);
    let cut = |bootstrap: u64, profile| KernelInput::BootstrapBegin {
        terminal_id: &id,
        stream_id: stream(),
        bootstrap_id: BootstrapId::new(bootstrap).unwrap(),
        profile,
        geometry: GRIDLESS,
        base_seq: 0,
    };
    let chunk_of = |bootstrap: u64| KernelInput::BootstrapChunk {
        terminal_id: &id,
        stream_id: stream(),
        bootstrap_id: BootstrapId::new(bootstrap).unwrap(),
        chunk_seq: 0,
        payload: b"\x0atext/plainhi",
    };
    let ready_of = |bootstrap: u64| KernelInput::BootstrapReady {
        terminal_id: &id,
        stream_id: stream(),
        bootstrap_id: BootstrapId::new(bootstrap).unwrap(),
        history_cursor: None,
    };
    let late = |bootstrap: u64| KernelInput::ResourceOutput {
        terminal_id: &id,
        stream_id: stream(),
        bootstrap_id: BootstrapId::new(bootstrap).unwrap(),
        seq: 5,
        payload: b"late",
    };

    // Generation 1 publishes; generation 2 names an unknown codec.
    kernel
        .update(
            cut(1, BootstrapStreamProfile::DocumentTypedV1),
            &mut effects,
        )
        .unwrap();
    kernel.update(chunk_of(1), &mut effects).unwrap();
    kernel.update(ready_of(1), &mut effects).unwrap();
    kernel
        .update(
            cut(2, BootstrapStreamProfile::Unknown { codec_tag: 9 }),
            &mut effects,
        )
        .unwrap();
    assert!(kernel.is_unsupported(&id));

    // A known recut lifts the refusal and publishes generation 3.
    kernel
        .update(
            cut(3, BootstrapStreamProfile::DocumentTypedV1),
            &mut effects,
        )
        .unwrap();
    kernel.update(chunk_of(3), &mut effects).unwrap();
    kernel.update(ready_of(3), &mut effects).unwrap();
    assert!(!kernel.is_unsupported(&id));

    // Late frames of the published and the refused generations are fenced.
    for bootstrap in [1, 2] {
        assert!(matches!(
            kernel.update(late(bootstrap), &mut effects),
            Err(KernelError::RetiredGeneration { .. })
        ));
    }
    kernel.update(late(3), &mut effects).unwrap();
    assert!(matches!(
        effects.as_slice(),
        [KernelEffect::Document(document)] if document.bytes == b"late"
    ));
}

#[test]
fn an_unknown_codec_on_a_terminal_is_a_profile_mismatch() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    let terminal = ResourceId::local(6);
    kernel
        .update(
            KernelInput::AttachStarted {
                attach_id: 1,
                terminals: std::slice::from_ref(&terminal),
            },
            &mut effects,
        )
        .unwrap();
    let grid = CanonicalGeometry::new(80, 24).unwrap();
    let result = kernel.update(
        begin(
            &terminal,
            BootstrapStreamProfile::Unknown { codec_tag: 0x77 },
            grid,
        ),
        &mut effects,
    );
    assert!(matches!(
        result,
        Err(KernelError::ProfileMismatch {
            incoming: BootstrapStreamProfile::Unknown { codec_tag: 0x77 },
            ..
        })
    ));
    assert!(!kernel.is_unsupported(&terminal));
}

#[test]
fn an_unknown_codec_on_an_agent_session_is_a_kind_mismatch() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    let agent = ResourceId::local(7);
    kernel
        .update(
            begin(&agent, BootstrapStreamProfile::AgentEventsJsonlV1, GRIDLESS),
            &mut effects,
        )
        .unwrap();
    let result = kernel.update(
        begin(
            &agent,
            BootstrapStreamProfile::Unknown { codec_tag: 0x77 },
            GRIDLESS,
        ),
        &mut effects,
    );
    assert!(matches!(
        result,
        Err(KernelError::KindMismatch {
            declared: phux_protocol::ResourceKind::AgentSession,
            ..
        })
    ));
    assert!(!kernel.is_unsupported(&agent));
}

#[test]
fn an_unknown_codec_on_a_closed_resource_is_refused_as_closed() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    let id = ResourceId::local(8);
    kernel
        .update(
            KernelInput::ResourceClosed {
                terminal_id: &id,
                exit_status: None,
                signal: None,
                reason: CloseReason::Unknown,
            },
            &mut effects,
        )
        .unwrap();
    let result = kernel.update(
        begin(
            &id,
            BootstrapStreamProfile::Unknown { codec_tag: 1 },
            GRIDLESS,
        ),
        &mut effects,
    );
    assert!(matches!(result, Err(KernelError::ClosedTerminal(_))));
}
