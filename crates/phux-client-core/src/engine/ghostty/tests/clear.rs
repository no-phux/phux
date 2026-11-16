//! Clear at the adapter boundary, independent of the retired C ABI.

use super::*;

fn pending_replica(
    profile: BootstrapStreamProfile,
    pending: &[u8],
) -> (GhosttyAdapter, GhosttyReplica, EngineEffectBuffer) {
    let mut bytes = b"old\r\nvisible".to_vec();
    bytes.extend_from_slice(pending);
    if profile == native_profile() {
        let mut source = GhosttyTerminal::new(80, 4).unwrap();
        source
            .set_continuation_max_bytes(CONTINUATION_LIMIT)
            .unwrap();
        source.vt_write(&bytes);
        let capture = EncodedSnapshot::capture(&mut source, capture_limits()).unwrap();
        let ready_end = capture.prefix_records.last().unwrap().end;
        bytes = capture.bytes[..ready_end].to_vec();
    }
    let mut adapter = native_adapter();
    let mut replica = adapter.start_replica(profile, geometry()).unwrap();
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, &bytes, &mut effects)
        .unwrap();
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .unwrap();
    effects.clear();
    (adapter, replica, effects)
}

fn first_codepoint(replica: &GhosttyReplica) -> u32 {
    replica
        .terminal()
        .unwrap()
        .grid_ref(libghostty_vt::point::Point::Active(PointCoordinate {
            x: 0,
            y: 0,
        }))
        .unwrap()
        .cell()
        .unwrap()
        .codepoint()
        .unwrap()
}

#[test]
fn clear_preserves_partial_input_in_native_and_synthesized_replicas() {
    let cases: &[(&[u8], &[u8], char)] = &[
        (b"\x1b[3", b"1mX", 'X'),
        (b"\x1b]2;pending ", b"title\x07X", 'X'),
        (b"\x1bP1;2|pending", b"body\x1b\\X", 'X'),
        (b"\x1bP$q", b"m\x1b\\X", 'X'),
        (b"\xe2", b"\x82\xac", '€'),
        (b"\xe2\x82", b"\xac", '€'),
        (b"\xf0", b"\x9f\x98\x80", '😀'),
        (b"\xf0\x9f", b"\x98\x80", '😀'),
        (b"\xf0\x9f\x98", b"\x80", '😀'),
    ];
    for profile in [
        native_profile(),
        BootstrapStreamProfile::SynthesizedVtRaw,
        BootstrapStreamProfile::SynthesizedVtStateSync,
    ] {
        for &(prefix, suffix, expected) in cases {
            let (mut adapter, mut replica, mut effects) = pending_replica(profile, prefix);
            adapter.clear_presentation(&mut replica).unwrap();
            assert!(
                effects.as_slice().is_empty(),
                "Clear emits no replica replies"
            );
            assert_eq!(first_codepoint(&replica), 0);
            assert_eq!(replica.terminal().unwrap().scrollback_rows().unwrap(), 0);
            adapter
                .apply_output(&mut replica, suffix, &mut effects)
                .unwrap();
            assert_eq!(first_codepoint(&replica), u32::from(expected));
        }
    }
}
