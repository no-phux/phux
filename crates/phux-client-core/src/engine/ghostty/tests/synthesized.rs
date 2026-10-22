//! Synthesized profiles, staging capacity, and live bell/clipboard effects.

use super::*;

#[test]
fn native_staging_capacity_reports_owned_feed_allocation() {
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    assert_eq!(adapter.bootstrap_staging_bytes(&replica), 0);
    let payload = b"incomplete snapshot fragment";
    adapter
        .apply_bootstrap_chunk(&mut replica, payload, &mut EngineEffectBuffer::new())
        .expect("bounded fragment");
    assert!(adapter.bootstrap_staging_bytes(&replica) >= payload.len());
}

#[test]
fn unpublished_synthesized_clear_is_rejected() {
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(BootstrapStreamProfile::SynthesizedVtRaw, geometry())
        .unwrap();
    assert!(matches!(
        adapter.clear_presentation(&mut replica),
        Err(GhosttyEngineError::LiveOutputBeforeReady)
    ));
}

#[test]
fn synthesized_profiles_write_borrowed_bytes_and_reject_history() {
    for profile in [
        BootstrapStreamProfile::SynthesizedVtRaw,
        BootstrapStreamProfile::SynthesizedVtStateSync,
    ] {
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(profile, geometry())
            .expect("synth replica");
        let mut effects = EngineEffectBuffer::new();
        assert_eq!(
            adapter
                .apply_bootstrap_chunk(&mut replica, b"\x1b]2;synth-title\x07", &mut effects)
                .expect("bootstrap bytes"),
            BootstrapProgress::Pending
        );
        assert_eq!(
            adapter
                .finish_bootstrap(&mut replica, &mut effects)
                .expect("protocol finish"),
            BootstrapProgress::Finished
        );
        assert_eq!(
            replica.terminal().expect("synth terminal").title().unwrap(),
            "synth-title"
        );
        assert!(matches!(
            adapter.apply_history_page(&mut replica, b"history", 1, &mut effects),
            Err(GhosttyEngineError::HistoryUnsupported(actual)) if actual == profile
        ));
        assert!(matches!(
            adapter.apply_bootstrap_chunk(&mut replica, b"late", &mut effects),
            Err(GhosttyEngineError::InputAfterFinish)
        ));
    }
}

#[test]
fn synthesized_terminal_queries_emit_no_replica_replies() {
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(BootstrapStreamProfile::SynthesizedVtRaw, geometry())
        .expect("synth replica");
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, b"\x1b[5n", &mut effects)
        .expect("bootstrap DSR query");
    assert!(effects.as_slice().is_empty());
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .expect("publish synthesized terminal");
    assert!(matches!(
        effects.as_slice(),
        [EngineEffect::Status(crate::engine::EngineStatus::Title(title))] if title.is_empty()
    ));
    effects.clear();
    adapter
        .apply_output(&mut replica, b"\x1b[5n", &mut effects)
        .expect("live DSR query");
    assert!(matches!(
        effects.as_slice(),
        [EngineEffect::Damage(EngineDamage::Full)]
    ));
}

#[test]
fn live_bells_reach_effects_in_synthesized_and_native_replicas() {
    let captured = capture_snapshot();
    for profile in [BootstrapStreamProfile::SynthesizedVtRaw, native_profile()] {
        let mut adapter = native_adapter();
        let mut replica = adapter.start_replica(profile, geometry()).unwrap();
        let mut effects = EngineEffectBuffer::new();
        let bytes = if profile == BootstrapStreamProfile::SynthesizedVtRaw {
            b"historical bell\x07".as_slice()
        } else {
            &captured.ready
        };
        adapter
            .apply_bootstrap_chunk(&mut replica, bytes, &mut effects)
            .unwrap();
        adapter
            .finish_bootstrap(&mut replica, &mut effects)
            .unwrap();
        assert!(!effects.as_slice().iter().any(|effect| matches!(
            effect,
            EngineEffect::Status(crate::engine::EngineStatus::Bell)
        )));
        assert_live_bell(&mut adapter, &mut replica, &mut effects);
        if profile != BootstrapStreamProfile::SynthesizedVtRaw {
            apply_history(&mut adapter, &mut replica, &captured, &mut effects);
            assert_live_bell(&mut adapter, &mut replica, &mut effects);
        }
    }
}

/// ADR-0158: an OSC 52 write fires only from live output, once, as the
/// decoded text; one replayed in bootstrap bytes never does.
#[test]
fn clipboard_writes_fire_from_live_output_only() {
    let osc52 = b"\x1b]52;c;aGVsbG8=\x07".as_slice();
    let clipboard = |effects: &EngineEffectBuffer| -> Vec<String> {
        effects
            .as_slice()
            .iter()
            .filter_map(|effect| match effect {
                EngineEffect::Status(crate::engine::EngineStatus::ClipboardWrite(text)) => {
                    Some(text.0.clone())
                }
                _ => None,
            })
            .collect()
    };
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(BootstrapStreamProfile::SynthesizedVtRaw, geometry())
        .unwrap();
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, osc52, &mut effects)
        .unwrap();
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .unwrap();
    assert!(
        clipboard(&effects).is_empty(),
        "replayed history must not copy"
    );

    effects.clear();
    adapter
        .apply_output(&mut replica, osc52, &mut effects)
        .unwrap();
    assert_eq!(clipboard(&effects), ["hello"]);

    effects.clear();
    adapter
        .apply_output(&mut replica, b"quiet", &mut effects)
        .unwrap();
    assert!(clipboard(&effects).is_empty(), "a write fires once");
}

fn assert_live_bell(
    adapter: &mut GhosttyAdapter,
    replica: &mut GhosttyReplica,
    effects: &mut EngineEffectBuffer,
) {
    effects.clear();
    adapter
        .apply_output(replica, b"\x1b]2;title\x07", effects)
        .unwrap();
    assert!(!effects.as_slice().iter().any(|effect| matches!(
        effect,
        EngineEffect::Status(crate::engine::EngineStatus::Bell)
    )));
    effects.clear();
    adapter.apply_output(replica, b"\x07", effects).unwrap();
    assert_eq!(
        effects
            .as_slice()
            .iter()
            .filter(|effect| matches!(
                effect,
                EngineEffect::Status(crate::engine::EngineStatus::Bell)
            ))
            .count(),
        1
    );
    effects.clear();
    adapter.apply_output(replica, b"quiet", effects).unwrap();
    assert!(!effects.as_slice().iter().any(|effect| matches!(
        effect,
        EngineEffect::Status(crate::engine::EngineStatus::Bell)
    )));
}
