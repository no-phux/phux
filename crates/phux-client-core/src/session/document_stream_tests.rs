//! A document stream (`AccessibilityTree`, `PixelSurface`, `Document`) is no replica:
//! the bootstrap cut arrives in chunks and lands whole at READY, and every
//! live frame is a newer whole document. Each reaches the consumer as one
//! [`KernelEffect::Document`].

#![allow(clippy::unwrap_used, reason = "tests")]

use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{
    BootstrapId, BootstrapProfile, BootstrapStreamProfile, ResourceId, ResourceKind, StreamId,
};

use super::kernel_rig::RigAdapter;
use super::{DocumentPublication, EffectBuffer, KernelEffect, KernelInput, SessionKernel};
use crate::engine::CanonicalGeometry;

mod rails;

fn surface() -> ResourceId {
    ResourceId::local(9)
}

fn stream() -> StreamId {
    StreamId::new(3).unwrap()
}

fn generation(raw: u64) -> BootstrapId {
    BootstrapId::new(raw).unwrap()
}

fn documents(effects: &EffectBuffer) -> Vec<&DocumentPublication> {
    effects
        .as_slice()
        .iter()
        .filter_map(|effect| match effect {
            KernelEffect::Document(document) => Some(document),
            _ => None,
        })
        .collect()
}

#[test]
fn a_document_cut_lands_whole_at_ready_and_each_live_frame_is_a_document() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    let id = surface();
    kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id: &id,
                stream_id: stream(),
                bootstrap_id: generation(1),
                profile: BootstrapStreamProfile::PixelSurfacePngV1,
                geometry: CanonicalGeometry { cols: 0, rows: 0 },
                base_seq: 2,
            },
            &mut effects,
        )
        .unwrap();
    assert!(documents(&effects).is_empty());
    for (chunk_seq, payload) in [b"\x89PN" as &[u8], b"G-chunk"].into_iter().enumerate() {
        kernel
            .update(
                KernelInput::BootstrapChunk {
                    terminal_id: &id,
                    stream_id: stream(),
                    bootstrap_id: generation(1),
                    chunk_seq: u32::try_from(chunk_seq).unwrap(),
                    payload,
                },
                &mut effects,
            )
            .unwrap();
        assert!(documents(&effects).is_empty(), "a chunk is not a document");
    }
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &id,
                stream_id: stream(),
                bootstrap_id: generation(1),
                history_cursor: None,
            },
            &mut effects,
        )
        .unwrap();
    let cut = documents(&effects);
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0].kind, ResourceKind::PixelSurface);
    assert_eq!(cut[0].seq, 2);
    assert_eq!(cut[0].bytes, b"\x89PNG-chunk");

    kernel
        .update(
            KernelInput::ResourceOutput {
                terminal_id: &id,
                stream_id: stream(),
                bootstrap_id: generation(1),
                seq: 3,
                payload: b"newer",
            },
            &mut effects,
        )
        .unwrap();
    let live = documents(&effects);
    assert_eq!(
        (live[0].seq, live[0].bytes.as_slice()),
        (3, b"newer" as &[u8])
    );

    // A lapped viewer's fence changes nothing by itself; the recut lands.
    kernel
        .update(
            KernelInput::Tombstone {
                terminal_id: &id,
                stream_id: stream(),
                bootstrap_id: generation(1),
                reason: TombstoneReason::OutboundGap,
                last_valid_seq: 3,
            },
            &mut effects,
        )
        .unwrap();
    assert!(documents(&effects).is_empty());
}

#[test]
fn a_tree_profile_on_a_surface_is_a_kind_mismatch() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    let id = surface();
    let begin = |profile, bootstrap_id| KernelInput::BootstrapBegin {
        terminal_id: &id,
        stream_id: stream(),
        bootstrap_id,
        profile,
        geometry: CanonicalGeometry { cols: 0, rows: 0 },
        base_seq: 0,
    };
    kernel
        .update(
            begin(BootstrapStreamProfile::PixelSurfacePngV1, generation(1)),
            &mut effects,
        )
        .unwrap();
    assert!(
        kernel
            .update(
                begin(
                    BootstrapStreamProfile::AccessibilityTreeJsonV1,
                    generation(2)
                ),
                &mut effects,
            )
            .is_err()
    );
}

#[test]
fn a_typed_document_names_its_content_type_at_ready_and_on_every_live_frame() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    let id = surface();
    let payload =
        phux_protocol::wire::frame::typed_document::encode_bootstrap("text/markdown", b"# hi");
    let inputs = [
        KernelInput::BootstrapBegin {
            terminal_id: &id,
            stream_id: stream(),
            bootstrap_id: generation(1),
            profile: BootstrapStreamProfile::DocumentTypedV1,
            geometry: CanonicalGeometry { cols: 0, rows: 0 },
            base_seq: 4,
        },
        KernelInput::BootstrapChunk {
            terminal_id: &id,
            stream_id: stream(),
            bootstrap_id: generation(1),
            chunk_seq: 0,
            payload: &payload,
        },
        KernelInput::BootstrapReady {
            terminal_id: &id,
            stream_id: stream(),
            bootstrap_id: generation(1),
            history_cursor: None,
        },
    ];
    for input in inputs {
        kernel.update(input, &mut effects).unwrap();
    }
    let cut = documents(&effects);
    assert_eq!(cut[0].kind, ResourceKind::Document);
    assert_eq!(
        (
            cut[0].seq,
            cut[0].bytes.as_slice(),
            cut[0].content_type.as_deref()
        ),
        (4, b"# hi" as &[u8], Some("text/markdown"))
    );
    kernel
        .update(
            KernelInput::ResourceOutput {
                terminal_id: &id,
                stream_id: stream(),
                bootstrap_id: generation(1),
                seq: 5,
                payload: b"# two",
            },
            &mut effects,
        )
        .unwrap();
    let live = documents(&effects);
    assert_eq!(
        (live[0].bytes.as_slice(), live[0].content_type.as_deref()),
        (b"# two" as &[u8], Some("text/markdown"))
    );
}
