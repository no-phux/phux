//! Native decoding, publication, history pages, and capability negotiation.

use super::*;

#[test]
fn native_decoder_accepts_arbitrary_fragment_cuts_and_multiple_records() {
    let captured = capture_snapshot();
    for width in [1, 2, 3, 7, 31, captured.ready.len()] {
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        let mut bootstrap_progress = BootstrapProgress::Pending;
        for fragment in captured.ready.chunks(width) {
            bootstrap_progress = adapter
                .apply_bootstrap_chunk(&mut replica, fragment, &mut effects)
                .expect("arbitrary bootstrap fragment");
        }
        assert_eq!(bootstrap_progress, BootstrapProgress::Pending);
        assert_eq!(
            adapter
                .finish_bootstrap(&mut replica, &mut effects)
                .expect("protocol READY"),
            BootstrapProgress::Ready
        );
        apply_history(&mut adapter, &mut replica, &captured, &mut effects);
        assert_eq!(
            replica
                .terminal()
                .expect("finished terminal")
                .title()
                .unwrap(),
            "checkpoint-title"
        );
    }
}

#[test]
fn publication_requires_authenticated_ready_and_one_shot_continuation_replay() {
    let captured = capture_snapshot();
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    let mut effects = EngineEffectBuffer::new();
    assert!(matches!(
        adapter.apply_output(&mut replica, b"too early", &mut effects),
        Err(GhosttyEngineError::LiveOutputBeforeReady)
    ));
    assert_eq!(
        adapter
            .apply_bootstrap_chunk(&mut replica, &captured.ready, &mut effects)
            .expect("prefix bytes"),
        BootstrapProgress::Pending
    );
    assert!(replica.terminal().is_none());
    assert!(matches!(
        adapter.apply_output(&mut replica, b"not published", &mut effects),
        Err(GhosttyEngineError::LiveOutputBeforeReady)
    ));
    assert!(matches!(
        adapter.clear_presentation(&mut replica),
        Err(GhosttyEngineError::LiveOutputBeforeReady)
    ));
    assert_eq!(
        adapter
            .finish_bootstrap(&mut replica, &mut effects)
            .expect("protocol READY validates native READY"),
        BootstrapProgress::Ready
    );
    adapter
        .apply_output(&mut replica, b"\x1b]2;published-live\x07", &mut effects)
        .expect("published live output");
    assert_eq!(
        replica.terminal().unwrap().title().unwrap(),
        "published-live"
    );
    effects.clear();
    adapter
        .apply_output(&mut replica, b"\x1b[5n", &mut effects)
        .expect("native DSR query after READY publication");
    assert!(matches!(
        effects.as_slice(),
        [EngineEffect::Damage(EngineDamage::Full)]
    ));
}

#[test]
fn live_output_is_applied_between_later_history_pages() {
    let captured = capture_snapshot();
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, &captured.ready, &mut effects)
        .expect("checkpoint bootstrap record");
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .expect("protocol READY");
    adapter
        .apply_output(
            &mut replica,
            b"\x1b]2;live-during-history\x07",
            &mut effects,
        )
        .expect("live output after READY");
    for (page, rows) in &captured.history {
        adapter
            .apply_history_page(&mut replica, page, *rows, &mut effects)
            .expect("history page");
        adapter
            .apply_output(&mut replica, b"interleaved-live", &mut effects)
            .expect("live output between history pages");
    }
    assert_eq!(
        replica
            .terminal()
            .expect("finished terminal")
            .title()
            .unwrap(),
        "live-during-history"
    );
}

#[test]
fn native_truncation_corruption_and_limits_are_typed() {
    let captured = capture_snapshot();
    let bootstrap = &captured.ready;
    let mut effects = EngineEffectBuffer::new();

    let mut adapter = native_adapter();
    let mut early = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    assert!(matches!(
        adapter.finish_bootstrap(&mut early, &mut effects),
        Err(GhosttyEngineError::Checkpoint { .. })
    ));

    let mut adapter = native_adapter();
    let mut truncated = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    adapter
        .apply_bootstrap_chunk(
            &mut truncated,
            &bootstrap[..bootstrap.len() - 1],
            &mut effects,
        )
        .expect("truncated fragment is buffered");
    assert!(matches!(
        adapter.finish_bootstrap(&mut truncated, &mut effects),
        Err(GhosttyEngineError::Checkpoint { .. })
    ));

    let mut corrupt = bootstrap.clone();
    corrupt[0] ^= 0xff;
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    adapter
        .apply_bootstrap_chunk(&mut replica, &corrupt, &mut effects)
        .expect("corrupt bytes are buffered until READY");
    assert!(matches!(
        adapter.finish_bootstrap(&mut replica, &mut effects),
        Err(GhosttyEngineError::Checkpoint { .. })
    ));

    let tiny = BootstrapLimits::new(1, 1).expect("tiny valid limits");
    let mut adapter = GhosttyAdapter::new(tiny);
    let mut replica = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    assert!(matches!(
        adapter.apply_bootstrap_chunk(&mut replica, bootstrap, &mut effects),
        Err(GhosttyEngineError::PayloadLimitExceeded { limit: 1, .. })
    ));

    let mut adapter = GhosttyAdapter::new(tiny);
    let mut replica = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    let mut limit_error = None;
    for byte in bootstrap {
        if let Err(error) =
            adapter.apply_bootstrap_chunk(&mut replica, std::slice::from_ref(byte), &mut effects)
        {
            limit_error = Some(error);
            break;
        }
    }
    assert!(limit_error.is_none());
    assert!(matches!(
        adapter.finish_bootstrap(&mut replica, &mut effects),
        Ok(BootstrapProgress::Ready) | Err(GhosttyEngineError::Checkpoint { .. })
    ));
}

#[test]
fn native_history_finish_rejects_trailing_and_post_finish_pages() {
    let captured = capture_snapshot();
    let mut finish = captured.finish.clone();
    finish.extend_from_slice(b"trailing");
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, &captured.ready, &mut effects)
        .expect("checkpoint through READY");
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .expect("protocol READY");
    for (page, rows) in &captured.history {
        adapter
            .apply_history_page(&mut replica, page, *rows, &mut effects)
            .expect("complete history page");
    }
    adapter
        .apply_history_page(&mut replica, &finish, 0, &mut effects)
        .expect_err("trailing bytes after FINISH are rejected");
    assert!(replica.terminal().is_some());
}

#[test]
fn native_history_rejects_truncated_page_and_finish_and_releases_feed() {
    let captured = capture_snapshot();
    for (truncated, rows) in [
        (
            &captured.history[0].0[..captured.history[0].0.len() - 1],
            captured.history[0].1,
        ),
        (&captured.finish[..captured.finish.len() - 1], 0),
    ] {
        let mut adapter = native_adapter();
        let mut replica = adapter
            .start_replica(native_profile(), geometry())
            .expect("native replica");
        let mut effects = EngineEffectBuffer::new();
        adapter
            .apply_bootstrap_chunk(&mut replica, &captured.ready, &mut effects)
            .expect("READY bytes");
        adapter
            .finish_bootstrap(&mut replica, &mut effects)
            .expect("protocol READY");
        assert!(matches!(
            adapter.apply_history_page(&mut replica, truncated, rows, &mut effects),
            Err(GhosttyEngineError::Checkpoint { .. })
        ));
        assert_eq!(adapter.bootstrap_staging_bytes(&replica), 0);
    }
}

#[test]
fn native_history_row_mismatch_is_rejected_before_terminal_mutation() {
    let captured = capture_snapshot();
    let (page, rows) = &captured.history[0];
    let mut adapter = native_adapter();
    let mut replica = adapter
        .start_replica(native_profile(), geometry())
        .expect("native replica");
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, &captured.ready, &mut effects)
        .expect("READY bytes");
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .expect("protocol READY");
    let before = replica
        .terminal()
        .expect("published terminal")
        .scrollback_rows()
        .expect("scrollback rows");

    assert!(matches!(
        adapter.apply_history_page(&mut replica, page, rows.saturating_add(1), &mut effects,),
        Err(GhosttyEngineError::Checkpoint { .. })
    ));
    assert_eq!(
        replica
            .terminal()
            .expect("last good terminal remains live")
            .scrollback_rows()
            .expect("scrollback rows"),
        before
    );
    assert_eq!(adapter.bootstrap_staging_bytes(&replica), 0);
}

#[test]
fn native_profile_is_rejected_when_the_runtime_contract_is_unavailable() {
    let mut adapter = native_adapter();
    adapter.native_available = false;
    assert!(matches!(
        adapter.start_replica(native_profile(), geometry()),
        Err(GhosttyEngineError::UnsupportedProfile(
            BootstrapStreamProfile::NativeState {
                codec: EngineCodec::LibghosttySnapshotV1,
            }
        ))
    ));
}

#[test]
fn native_capabilities_require_the_exact_runtime_contract() {
    let limits = BootstrapLimits::default();
    let capabilities = native_bootstrap_capabilities(limits);
    assert!(
        capabilities
            .native_codecs
            .contains(EngineCodec::LibghosttySnapshotV1)
    );
    assert!(
        !capabilities
            .native_codecs
            .contains(EngineCodec::LibghosttyCheckpointV2)
    );
    assert_eq!(
        capabilities.native_features,
        EngineFeatureSet::required_native()
    );
    assert_eq!(capabilities.limits, limits);
}
