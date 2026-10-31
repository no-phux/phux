//! Bootstrap staging budgets and synthesized bootstrap publication.
#![allow(
    clippy::wildcard_imports,
    reason = "the attach test topics share the parent test namespace"
)]

use super::*;

fn tiny_staged_pane(pane: u32) -> Vec<FrameKind> {
    let terminal_id = phux_protocol::ids::ResourceId::local(pane + 1);
    let stream_id = StreamId::new(1).expect("stream id");
    let bootstrap_id = BootstrapId::new(1).expect("bootstrap id");
    vec![
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::from_static(b"pane"),
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ]
}

#[test]
fn aggregate_staging_budget_rejects_many_panes_without_large_allocations() {
    let mut budget = BootstrapStagingBudget::with_limits(8 * 4, 8 * 3);
    let mut staged = Vec::new();

    for pane in 0..16 {
        let mut frames = tiny_staged_pane(pane);
        let result = budget.append_accounted(&mut staged, &mut frames, 4);
        if pane < 8 {
            assert!(result.is_ok(), "pane {pane} fits the aggregate budget");
            assert!(frames.is_empty(), "accepted frames move into staging");
        } else {
            assert!(result.is_err(), "pane {pane} exceeds the aggregate budget");
            assert_eq!(frames.len(), 3, "rejected frames are not appended");
        }
    }

    assert_eq!(staged.len(), 8 * 3);
    assert_eq!(budget.staged_bytes, 8 * 4);
    assert_eq!(budget.staged_frames, 8 * 3);
}

#[test]
fn bootstrap_adaptation_peak_includes_sources_scratch_and_outputs() {
    let mut scrollback = Vec::new();
    scrollback
        .try_reserve_exact(512)
        .expect("scrollback reserve");
    scrollback.resize(512, b's');
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(1_024).expect("snapshot reserve");
    bytes.resize(1_024, b'x');
    let source_capacity = scrollback.capacity() + bytes.capacity();
    let peak_budget = source_capacity.checked_mul(2).expect("peak budget");
    let caps = ClientCapabilities::default()
        .with_color_support(phux_protocol::caps::ColorSupport::Indexed256);
    assert!(!crate::downsample::caps_pass_through(caps));

    let adapted = adapt_bootstrap_snapshot(
        crate::grid::SnapshotBytes {
            cols: 80,
            rows: 24,
            bytes,
            scrollback,
        },
        caps,
        peak_budget,
    )
    .expect("bounded capability adaptation");

    assert_eq!(
        adapted
            .payloads
            .iter()
            .map(bytes::Bytes::len)
            .sum::<usize>(),
        source_capacity,
    );
    assert_eq!(adapted.retained_bytes, source_capacity);
    assert!(adapted.peak_bytes <= peak_budget);
}

#[test]
fn bootstrap_adaptation_restores_images_only_for_kitty_consumers() {
    let mut source = libghostty_vt::Terminal::new(10, 5).unwrap();
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut source).unwrap();
    source.resize(10, 5, 8, 16).unwrap();
    source.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,c=1,r=1,i=9,p=1,C=1,q=2;AAECAw==\x1b\\\x1b_Ga=p,i=9,p=2,U=1,c=2,r=2,q=2;\x1b\\");
    source.vt_write(b"\x1b_Ga=t,f=32,s=1,v=1,i=10,q=2;BAUGBw==\x1b\\");
    for image_protocols in [
        phux_protocol::caps::ImageProtocolSet::new(),
        phux_protocol::caps::ImageProtocolSet::with(&[
            phux_protocol::caps::ImageProtocol::KittyGraphics,
        ]),
    ] {
        let snapshot = crate::grid::synthesize(&source).unwrap();
        let adapted = adapt_bootstrap_snapshot(
            snapshot,
            ClientCapabilities::default().with_image_protocols(image_protocols),
            1024 * 1024,
        )
        .unwrap();
        let mut replica = libghostty_vt::Terminal::new(10, 5).unwrap();
        // Enable storage even on the image-free receiver: this proves that
        // capability adaptation removed the payload, rather than relying on
        // its terminal to ignore it.
        phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut replica).unwrap();
        replica.resize(10, 5, 8, 16).unwrap();
        for payload in adapted.payloads {
            replica.vt_write(&payload);
        }
        let graphics = replica.kitty_graphics().unwrap();
        let expects_images =
            image_protocols.contains(phux_protocol::caps::ImageProtocol::KittyGraphics);
        assert_eq!(graphics.image(9).is_some(), expects_images);
        assert_eq!(graphics.image(10).is_some(), expects_images);
        let mut iter = libghostty_vt::kitty::graphics::PlacementIterator::new().unwrap();
        let mut placements = iter.update(&graphics).unwrap();
        let mut count = 0;
        while placements.next().is_some() {
            count += 1;
        }
        assert_eq!(count, if expects_images { 2 } else { 0 });
    }
}

#[test]
fn native_live_bytes_ignore_restrictive_presentation_caps() {
    let caps = ClientCapabilities::default()
        .with_color_support(phux_protocol::caps::ColorSupport::Mono)
        .with_hyperlinks(false)
        .with_image_protocols(phux_protocol::caps::ImageProtocolSet::new());
    let bytes = bytes::Bytes::from_static(
        b"\x1b[38;2;1;2;3m\x1b]8;;https://example.invalid\x1b\\exact\xff",
    );
    let profile = phux_protocol::caps::BootstrapStreamProfile::NativeState {
        codec: phux_protocol::caps::EngineCodec::LibghosttySnapshotV1,
    };

    let adapted = live_bytes_for_profile(&bytes, caps, profile);

    assert_eq!(adapted, bytes);
    assert_eq!(
        adapted.as_ptr(),
        bytes.as_ptr(),
        "native path keeps the shared bytes"
    );
}

#[test]
fn aggregate_staging_charges_tiny_rewrites_by_retained_capacity() {
    fn kitty_snapshot() -> crate::grid::SnapshotBytes {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(64 * 1024).expect("kitty reserve");
        bytes.extend_from_slice(b"\x1b_Gf=100,a=T;");
        bytes.resize((64 * 1024) - 2, b'A');
        bytes.extend_from_slice(b"\x1b\\");
        crate::grid::SnapshotBytes {
            cols: 80,
            rows: 24,
            bytes,
            scrollback: Vec::new(),
        }
    }
    let caps = ClientCapabilities::default()
        .with_color_support(phux_protocol::caps::ColorSupport::Indexed256)
        .with_image_protocols(phux_protocol::caps::ImageProtocolSet::new());
    let sample = adapt_bootstrap_snapshot(kitty_snapshot(), caps, 2 * 64 * 1024).expect("rewrite");
    let retained_per_pane = sample.retained_bytes;
    let wire_per_pane = sample.payloads.iter().map(bytes::Bytes::len).sum::<usize>();
    assert!(
        retained_per_pane > wire_per_pane,
        "dropped Kitty payload retains rewrite allocation capacity"
    );
    drop(sample);

    let mut budget = BootstrapStagingBudget::with_limits(retained_per_pane * 3, usize::MAX);
    let mut staged = Vec::new();
    for pane in 0..4_u32 {
        let adapted = adapt_bootstrap_snapshot(
            kitty_snapshot(),
            caps,
            retained_per_pane.checked_mul(2).expect("peak budget"),
        )
        .expect("bounded pane rewrite");
        let retained_bytes = adapted.retained_bytes;
        let mut frames = synthesized_bootstrap_frames(
            phux_protocol::ids::ResourceId::local(pane + 1),
            StreamId::new(u64::from(pane) + 1).expect("stream id"),
            BootstrapId::new(u64::from(pane) + 1).expect("bootstrap id"),
            BootstrapStreamProfile::SynthesizedVtRaw,
            BootstrapLimits::new(
                phux_protocol::MAX_BOOTSTRAP_CHUNK_BYTES,
                phux_protocol::DEFAULT_HISTORY_PAGE_BYTES,
            )
            .expect("limits"),
            80,
            24,
            0,
            adapted.payloads,
        )
        .expect("bootstrap frames");
        let result = budget.append_accounted(&mut staged, &mut frames, retained_bytes);
        assert_eq!(result.is_ok(), pane < 3);
    }
    assert_eq!(budget.staged_bytes, retained_per_pane * 3);
}

#[test]
fn synthesized_bootstrap_is_built_completely_before_publication() {
    let terminal_id = phux_protocol::ids::ResourceId::local(7);
    let stream_id = StreamId::new(3).expect("stream id");
    let bootstrap_id = BootstrapId::new(5).expect("bootstrap id");
    let limits = BootstrapLimits::new(3, phux_protocol::DEFAULT_HISTORY_PAGE_BYTES)
        .expect("bounded test limits");
    let frames = synthesized_bootstrap_frames(
        terminal_id.clone(),
        stream_id,
        bootstrap_id,
        BootstrapStreamProfile::SynthesizedVtRaw,
        limits,
        80,
        24,
        11,
        [bytes::Bytes::from_static(b"abcdefg")],
    )
    .expect("build complete bootstrap");

    assert!(matches!(
        frames.first(),
        Some(FrameKind::BootstrapBegin {
            terminal_id: id,
            stream_id: stream,
            bootstrap_id: bootstrap,
            base_seq: 11,
            ..
        }) if id == &terminal_id && *stream == stream_id && *bootstrap == bootstrap_id
    ));
    let chunks: Vec<_> = frames
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::BootstrapChunk {
                chunk_seq, payload, ..
            } => Some((*chunk_seq, payload.as_ref())),
            _ => None,
        })
        .collect();
    assert_eq!(
        chunks,
        vec![
            (0, b"abc".as_slice()),
            (1, b"def".as_slice()),
            (2, b"g".as_slice())
        ]
    );
    assert!(matches!(
        frames.last(),
        Some(FrameKind::BootstrapReady {
            terminal_id: id,
            stream_id: stream,
            bootstrap_id: bootstrap,
            history_cursor: None,
        }) if id == &terminal_id && *stream == stream_id && *bootstrap == bootstrap_id
    ));
}
