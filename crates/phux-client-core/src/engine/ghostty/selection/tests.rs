use crate::engine::ghostty::GhosttyAdapter;
use crate::engine::{
    BoundedSelectionText, CanonicalGeometry, DocumentPoint, DocumentSpace, EngineDocumentSelection,
};
use crate::session::{EffectBuffer, KernelInput, SessionKernel};
use libghostty_vt::terminal::PointSpace;
use phux_protocol::{
    BootstrapId, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ResourceId, StreamId,
};

fn kernel() -> SessionKernel<GhosttyAdapter> {
    let mut kernel = SessionKernel::new(
        GhosttyAdapter::new(BootstrapLimits::default()),
        BootstrapProfile::SynthesizedVtRaw,
    );
    let id = ResourceId::local(1);
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    for input in [
        KernelInput::BootstrapBegin {
            terminal_id: &id,
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            geometry: CanonicalGeometry::new(20, 4).unwrap(),
            base_seq: 0,
        },
        KernelInput::BootstrapChunk {
            terminal_id: &id,
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: b"main\r\nsecond",
        },
        KernelInput::BootstrapReady {
            terminal_id: &id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        kernel.update(input, &mut EffectBuffer::new()).unwrap();
    }
    kernel
}

fn output(kernel: &mut SessionKernel<GhosttyAdapter>, seq: u64, payload: &[u8]) {
    kernel
        .update(
            KernelInput::ResourceOutput {
                terminal_id: &ResourceId::local(1),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                seq,
                payload,
            },
            &mut EffectBuffer::new(),
        )
        .unwrap();
}

fn selection(
    kernel: &mut SessionKernel<GhosttyAdapter>,
    end_column: u16,
) -> EngineDocumentSelection {
    let id = ResourceId::local(1);
    let start = kernel
        .track_document_anchor(
            &id,
            DocumentPoint {
                space: DocumentSpace::History,
                x: 0,
                y: 0,
            },
        )
        .unwrap();
    let end = kernel
        .track_document_anchor(
            &id,
            DocumentPoint {
                space: DocumentSpace::History,
                x: end_column,
                y: 0,
            },
        )
        .unwrap();
    EngineDocumentSelection {
        start,
        end,
        rectangle: false,
    }
}

fn copy(
    kernel: &SessionKernel<GhosttyAdapter>,
    selection: EngineDocumentSelection,
    max_bytes: usize,
) -> BoundedSelectionText {
    kernel
        .format_document_selection_bounded(&ResourceId::local(1), selection, max_bytes)
        .unwrap()
}

#[test]
fn bounded_copy_refuses_main_screen_anchors_while_alternate_is_active() {
    let mut kernel = kernel();
    let main = selection(&mut kernel, 3);
    output(&mut kernel, 1, b"\x1b[?1049h\x1b[HALT");
    let replica = kernel.published_engine(&ResourceId::local(1)).unwrap();
    let terminal = replica.terminal().unwrap();
    for id in [main.start, main.end] {
        let anchor = &replica.anchors[&id];
        assert!(
            anchor.point(PointSpace::History).unwrap().is_some(),
            "own-screen coordinates remain valid"
        );
        let snapshot = anchor.snapshot(terminal).unwrap().unwrap();
        assert!(
            terminal
                .point_from_grid_ref(&snapshot, PointSpace::History)
                .unwrap()
                .is_none(),
            "snapshot is not on the active screen"
        );
    }
    // An overflowing allocation budget also proves membership refusal precedes
    // output-buffer allocation. No runtime view reconciliation is involved.
    assert_eq!(
        copy(&kernel, main, usize::MAX),
        BoundedSelectionText::Unavailable
    );
    assert_eq!(copy(&kernel, main, 16), BoundedSelectionText::Unavailable);
    output(&mut kernel, 2, b"\x1b[?1049l");
    assert_eq!(
        copy(&kernel, main, 4),
        BoundedSelectionText::Text(b"main".to_vec())
    );
}

#[test]
fn bounded_copy_refuses_mixed_screen_endpoints_in_both_orders() {
    let mut kernel = kernel();
    let main = selection(&mut kernel, 3);
    output(&mut kernel, 1, b"\x1b[?1049h\x1b[HALT");
    let alt = selection(&mut kernel, 2);
    for (start, end) in [(main.start, alt.end), (alt.start, main.end)] {
        let mixed = EngineDocumentSelection {
            start,
            end,
            rectangle: false,
        };
        assert_eq!(copy(&kernel, mixed, 16), BoundedSelectionText::Unavailable);
    }
    assert_eq!(
        copy(&kernel, alt, 3),
        BoundedSelectionText::Text(b"ALT".to_vec())
    );
}

#[test]
fn bounded_copy_preserves_active_alternate_utf8_and_byte_limits() {
    let mut kernel = kernel();
    output(&mut kernel, 1, "\x1b[?1049h\x1b[Hé界".as_bytes());
    let alt = selection(&mut kernel, 2);
    let expected = "é界".as_bytes().to_vec();
    assert_eq!(
        copy(&kernel, alt, expected.len()),
        BoundedSelectionText::Text(expected.clone())
    );
    assert_eq!(
        copy(&kernel, alt, expected.len() - 1),
        BoundedSelectionText::ByteLimitExceeded
    );
    assert_eq!(
        copy(&kernel, alt, 0),
        BoundedSelectionText::ByteLimitExceeded
    );
}
