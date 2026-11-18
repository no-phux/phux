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
use super::test_rig::{Gen, Rig};
use super::{DocumentPublication, EffectBuffer, KernelEffect, SessionKernel};

mod deadlines;
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

fn rig() -> Rig<RigAdapter> {
    Rig::of(SessionKernel::new(
        RigAdapter::default(),
        BootstrapProfile::SynthesizedVtRaw,
    ))
}

/// Generation `raw` of the surface on stream 3.
fn surface_gen(raw: u64) -> Gen {
    Gen {
        id: surface(),
        s: stream(),
        b: generation(raw),
    }
}

#[test]
fn a_document_cut_lands_whole_at_ready_and_each_live_frame_is_a_document() {
    let mut rig = rig();
    let g = surface_gen(1);
    rig.ok(g.begin_gridless(BootstrapStreamProfile::PixelSurfacePngV1, 2));
    assert!(documents(&rig.fx).is_empty());
    for (chunk_seq, payload) in (0..).zip([b"\x89PN" as &[u8], b"G-chunk"]) {
        rig.ok(g.chunk(chunk_seq, payload));
        assert!(documents(&rig.fx).is_empty(), "a chunk is not a document");
    }
    rig.ok(g.ready(None));
    let cut = documents(&rig.fx);
    assert_eq!(cut.len(), 1);
    assert_eq!(cut[0].kind, ResourceKind::PixelSurface);
    assert_eq!(cut[0].seq, 2);
    assert_eq!(cut[0].bytes, b"\x89PNG-chunk");

    rig.ok(g.output(3, b"newer"));
    let live = documents(&rig.fx);
    assert_eq!(
        (live[0].seq, live[0].bytes.as_slice()),
        (3, b"newer" as &[u8])
    );

    // A lapped viewer's fence changes nothing by itself; the recut lands.
    rig.ok(g.tombstone(TombstoneReason::OutboundGap, 3));
    assert!(documents(&rig.fx).is_empty());
}

#[test]
fn a_typed_document_names_its_content_type_at_ready_and_on_every_live_frame() {
    let mut rig = rig();
    let g = surface_gen(1);
    let payload =
        phux_protocol::wire::frame::typed_document::encode_bootstrap("text/markdown", b"# hi");
    rig.ok(g.begin_gridless(BootstrapStreamProfile::DocumentTypedV1, 4));
    rig.ok(g.chunk(0, &payload));
    rig.ok(g.ready(None));
    let cut = documents(&rig.fx);
    assert_eq!(cut[0].kind, ResourceKind::Document);
    assert_eq!(
        (
            cut[0].seq,
            cut[0].bytes.as_slice(),
            cut[0].content_type.as_deref()
        ),
        (4, b"# hi" as &[u8], Some("text/markdown"))
    );
    rig.ok(g.output(5, b"# two"));
    let live = documents(&rig.fx);
    assert_eq!(
        (live[0].bytes.as_slice(), live[0].content_type.as_deref()),
        (b"# two" as &[u8], Some("text/markdown"))
    );
}
