use super::*;

#[test]
fn bootstrap_chunks_obey_even_a_one_byte_budget() {
    let terminal = phux_protocol::ids::ResourceId::local(1);
    let stream = phux_protocol::ids::StreamId::new(1).unwrap();
    let generation = phux_protocol::ids::BootstrapId::new(1).unwrap();
    let cut = AgentSessionBootstrap {
        base_seq: 2,
        resume_after_seq: None,
        records: vec![
            Bytes::from_static(b"first\n"),
            Bytes::from_static(b"second\n"),
        ],
        dropped: 0,
        ended: false,
        native_publications: Vec::new(),
        native_publication_high_water: 0,
    };
    for budget in [1, 3, 7, 13, 64] {
        let limits = BootstrapLimits::new(budget, 1024).unwrap();
        let frames: Vec<_> =
            bootstrap_frames(&terminal, stream, generation, &cut, limits).collect();
        assert!(matches!(
            frames.first(),
            Some(FrameKind::BootstrapBegin { base_seq: 2, .. })
        ));
        assert!(matches!(
            frames.last(),
            Some(FrameKind::BootstrapReady { .. })
        ));
        let mut bytes = Vec::new();
        for (index, frame) in frames[1..frames.len() - 1].iter().enumerate() {
            let FrameKind::BootstrapChunk {
                chunk_seq, payload, ..
            } = frame
            else {
                panic!("unexpected frame: {frame:?}");
            };
            assert_eq!(*chunk_seq as usize, index);
            assert!(!payload.is_empty() && payload.len() <= budget as usize);
            bytes.extend_from_slice(payload);
        }
        assert_eq!(bytes, b"first\nsecond\n");
        if budget >= 7 {
            let chunks: Vec<_> = frames[1..frames.len() - 1]
                .iter()
                .map(|frame| match frame {
                    FrameKind::BootstrapChunk { payload, .. } => payload.as_ref(),
                    _ => unreachable!("checked chunk frames above"),
                })
                .collect();
            assert!(
                chunks.iter().all(|chunk| chunk.ends_with(b"\n")),
                "records that fit must remain whole for older consumers"
            );
        }
    }
}

#[test]
fn empty_agent_bootstrap_has_only_begin_and_ready() {
    let cut = AgentSessionBootstrap {
        base_seq: 0,
        resume_after_seq: None,
        records: Vec::new(),
        dropped: 0,
        ended: false,
        native_publications: Vec::new(),
        native_publication_high_water: 0,
    };
    let frames: Vec<_> = bootstrap_frames(
        &phux_protocol::ids::ResourceId::local(1),
        phux_protocol::ids::StreamId::new(1).unwrap(),
        phux_protocol::ids::BootstrapId::new(1).unwrap(),
        &cut,
        BootstrapLimits::new(1, 1).unwrap(),
    )
    .collect();
    assert_eq!(frames.len(), 2);
    assert!(matches!(frames[0], FrameKind::BootstrapBegin { .. }));
    assert!(matches!(frames[1], FrameKind::BootstrapReady { .. }));
}
