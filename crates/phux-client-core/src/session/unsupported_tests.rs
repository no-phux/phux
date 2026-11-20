//! A stream in a codec this build does not know refuses that one resource:
//! its frames are ignored, and the other resources and the attach carry on.

#![allow(clippy::unwrap_used, reason = "tests")]

use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{BootstrapProfile, BootstrapStreamProfile};

use super::kernel_rig::RigAdapter;
use super::test_rig::{Gen, Rig, any_effect, attach_ready, attach_started, closed, gen_of};
use super::{KernelEffect, KernelError, KernelStatus, SessionKernel};

const fn unknown(codec_tag: u8) -> BootstrapStreamProfile {
    BootstrapStreamProfile::Unknown { codec_tag }
}

fn rig() -> Rig<RigAdapter> {
    Rig::of(SessionKernel::new(
        RigAdapter::default(),
        BootstrapProfile::SynthesizedVtRaw,
    ))
}

/// Every resource here streams generation 1 of stream 1.
fn res(id: u32) -> Gen {
    gen_of(id, 1, 1)
}

#[test]
fn an_unknown_codec_refuses_one_resource_and_the_rest_carry_on() {
    let mut rig = rig();
    let (terminal, alien, document) = (res(1), res(2), res(3));

    // The alien is of no kind the kernel knows, and outside the barrier.
    rig.ok(attach_started(7, std::slice::from_ref(&terminal.id)));
    rig.ok(alien.begin_gridless(unknown(0x77), 0));
    assert_eq!(
        rig.fx.as_slice(),
        [KernelEffect::Status(KernelStatus::Unsupported {
            terminal_id: alien.id.clone(),
            codec_tag: 0x77,
        })]
    );
    assert!(rig.k.is_unsupported(&alien.id));
    for input in [
        alien.chunk(0, b"opaque"),
        alien.ready(None),
        alien.output(1, b"live"),
        alien.tombstone(TombstoneReason::OutboundGap, 0),
    ] {
        rig.ok(input);
        assert!(rig.fx.as_slice().is_empty());
    }

    rig.ok(terminal.begin(0));
    rig.ok(terminal.chunk(0, b"hello"));
    rig.ok(terminal.ready(None));
    rig.ok(attach_ready(7));
    rig.ok(terminal.output(1, b" world"));

    rig.ok(document.begin_gridless(BootstrapStreamProfile::DocumentTypedV1, 0));
    rig.ok(document.chunk(0, b"\x0atext/plainhi"));
    rig.ok(document.ready(None));
    assert!(any_effect!(rig.fx, KernelEffect::Document(_)));

    // A close forgets the refusal; a reused id streams normally.
    rig.ok(closed(&alien.id));
    assert!(!rig.k.is_unsupported(&alien.id));
}

#[test]
fn an_unknown_codec_on_a_document_kind_is_refused_too() {
    let mut rig = rig();
    let g = res(5);
    rig.ok(g.begin_gridless(BootstrapStreamProfile::DocumentTypedV1, 0));
    rig.ok(g.chunk(0, b"\x0atext/plainhalf"));
    rig.ok(g.begin_gridless(unknown(9), 0));
    assert!(rig.k.is_unsupported(&g.id));
    rig.ok(g.ready(None));
    assert!(rig.fx.as_slice().is_empty(), "the half cut was dropped");
}

#[test]
fn a_refused_documents_retired_generations_stay_fenced_after_a_known_recut() {
    let mut rig = rig();
    let cut = |rig: &mut Rig<RigAdapter>, g: &Gen| {
        rig.ok(g.begin_gridless(BootstrapStreamProfile::DocumentTypedV1, 0));
        rig.ok(g.chunk(0, b"\x0atext/plainhi"));
        rig.ok(g.ready(None));
    };
    let generations = [1, 2, 3].map(|bootstrap| gen_of(9, 1, bootstrap));

    // Generation 1 publishes; generation 2 names an unknown codec.
    cut(&mut rig, &generations[0]);
    rig.ok(generations[1].begin_gridless(unknown(9), 0));
    assert!(rig.k.is_unsupported(&generations[1].id));

    // A known recut lifts the refusal and publishes generation 3.
    cut(&mut rig, &generations[2]);
    assert!(!rig.k.is_unsupported(&generations[2].id));

    // Late frames of the published and the refused generations are fenced.
    for retired in &generations[..2] {
        assert!(matches!(
            rig.up(retired.output(5, b"late")),
            Err(KernelError::RetiredGeneration { .. })
        ));
    }
    rig.ok(generations[2].output(5, b"late"));
    assert!(matches!(
        rig.fx.as_slice(),
        [KernelEffect::Document(document)] if document.bytes == b"late"
    ));
}

#[test]
fn an_unknown_codec_on_a_known_kind_is_refused_as_that_kind() {
    // A Terminal in the attach inventory: the codec is a profile mismatch.
    let mut rig = rig();
    let terminal = res(6);
    rig.ok(attach_started(1, std::slice::from_ref(&terminal.id)));
    assert!(matches!(
        rig.up(terminal.begin_as(unknown(0x77), 0)),
        Err(KernelError::ProfileMismatch {
            incoming: BootstrapStreamProfile::Unknown { codec_tag: 0x77 },
            ..
        })
    ));
    assert!(!rig.k.is_unsupported(&terminal.id));

    // An AgentSession: a kind mismatch.
    let agent = res(7);
    rig.ok(agent.begin_gridless(BootstrapStreamProfile::AgentEventsJsonlV1, 0));
    assert!(matches!(
        rig.up(agent.begin_gridless(unknown(0x77), 0)),
        Err(KernelError::KindMismatch {
            declared: phux_protocol::ResourceKind::AgentSession,
            ..
        })
    ));
    assert!(!rig.k.is_unsupported(&agent.id));

    // A closed resource: refused as closed.
    let gone = res(8);
    rig.ok(closed(&gone.id));
    assert!(matches!(
        rig.up(gone.begin_gridless(unknown(1), 0)),
        Err(KernelError::ClosedTerminal(_))
    ));
}
