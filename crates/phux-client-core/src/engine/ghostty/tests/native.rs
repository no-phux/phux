//! Native decoding, publication, history pages, and capability negotiation.

use super::*;

#[test]
fn pre1_native_codec_is_rejected_before_accepting_payload() {
    let profile = BootstrapStreamProfile::NativeState {
        codec: EngineCodec::LibghosttyPre1SnapshotV1,
    };
    let mut adapter = native_adapter();
    assert!(matches!(
        adapter.start_replica(profile, geometry()),
        Err(GhosttyEngineError::UnsupportedProfile(rejected)) if rejected == profile
    ));
    let advertised = native_bootstrap_capabilities(BootstrapLimits::default());
    assert!(
        advertised
            .native_codecs
            .contains(EngineCodec::LibghosttySnapshotV1)
    );
    assert!(
        !advertised
            .native_codecs
            .contains(EngineCodec::LibghosttyPre1SnapshotV1)
    );
}

#[test]
fn native_decoder_accepts_arbitrary_fragment_cuts_and_multiple_records() {
    let captured = capture_snapshot();
    for width in [1, 2, 3, 7, 31, captured.ready.len()] {
        let (mut adapter, mut replica, mut effects) = started(native_profile());
        let mut bootstrap_progress = BootstrapProgress::Pending;
        for fragment in captured.ready.chunks(width) {
            bootstrap_progress = adapter
                .apply_bootstrap_chunk(&mut replica, fragment, &mut effects)
                .expect("arbitrary bootstrap fragment");
        }
        assert_eq!(bootstrap_progress, BootstrapProgress::Pending, "{width}");
        assert_eq!(
            adapter
                .finish_bootstrap(&mut replica, &mut effects)
                .expect("protocol READY"),
            BootstrapProgress::Ready,
            "{width}"
        );
        apply_history(&mut adapter, &mut replica, &captured, &mut effects);
        assert_eq!(
            replica
                .terminal()
                .expect("finished terminal")
                .title()
                .unwrap(),
            b"checkpoint-title",
            "{width}"
        );
    }
}

#[test]
fn publication_requires_authenticated_ready_and_one_shot_continuation_replay() {
    let captured = capture_snapshot();
    let (mut adapter, mut replica, mut effects) = started(native_profile());
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
        b"published-live"
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
    let (mut adapter, mut replica, mut effects) = published(native_profile(), &captured.ready);
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
        b"live-during-history"
    );
}

#[test]
fn native_truncation_corruption_and_limits_are_typed() {
    let captured = capture_snapshot();
    let bootstrap = &captured.ready;
    let mut corrupt = bootstrap.clone();
    corrupt[0] ^= 0xff;
    // FINISH before any chunk, after a truncated record, or after corrupt bytes.
    for (label, fed) in [
        ("early", None),
        ("truncated", Some(&bootstrap[..bootstrap.len() - 1])),
        ("corrupt", Some(&corrupt[..])),
    ] {
        let (mut adapter, mut replica, mut effects) = started(native_profile());
        if let Some(fed) = fed {
            adapter
                .apply_bootstrap_chunk(&mut replica, fed, &mut effects)
                .unwrap_or_else(|error| panic!("{label}: bytes are buffered until READY: {error}"));
        }
        assert!(
            matches!(
                adapter.finish_bootstrap(&mut replica, &mut effects),
                Err(GhosttyEngineError::Checkpoint { .. })
            ),
            "{label}"
        );
    }

    let mut effects = EngineEffectBuffer::new();
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
    let (mut adapter, mut replica, mut effects) = published(native_profile(), &captured.ready);
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
    for (label, truncated, rows) in [
        (
            "page",
            &captured.history[0].0[..captured.history[0].0.len() - 1],
            captured.history[0].1,
        ),
        ("finish", &captured.finish[..captured.finish.len() - 1], 0),
    ] {
        let (mut adapter, mut replica, mut effects) = published(native_profile(), &captured.ready);
        assert!(
            matches!(
                adapter.apply_history_page(&mut replica, truncated, rows, &mut effects),
                Err(GhosttyEngineError::Checkpoint { .. })
            ),
            "{label}"
        );
        assert_eq!(adapter.bootstrap_staging_bytes(&replica), 0, "{label}");
    }
}

#[test]
fn native_history_row_mismatch_is_rejected_before_terminal_mutation() {
    let captured = capture_snapshot();
    let (page, rows) = &captured.history[0];
    let (mut adapter, mut replica, mut effects) = published(native_profile(), &captured.ready);
    let scrollback = |replica: &GhosttyReplica| {
        replica
            .terminal()
            .expect("published terminal")
            .scrollback_rows()
            .expect("scrollback rows")
    };
    let before = scrollback(&replica);

    assert!(matches!(
        adapter.apply_history_page(&mut replica, page, rows.saturating_add(1), &mut effects,),
        Err(GhosttyEngineError::Checkpoint { .. })
    ));
    assert_eq!(
        scrollback(&replica),
        before,
        "last good terminal remains live"
    );
    assert_eq!(adapter.bootstrap_staging_bytes(&replica), 0);
}

#[test]
fn live_history_clear_does_not_resurrect_deferred_snapshot_rows() {
    for clear in [Some(b"\x1b[3J".as_slice()), Some(b"\x1bc".as_slice()), None] {
        let captured = capture_snapshot();
        let (mut adapter, mut replica, mut effects) = published(native_profile(), &captured.ready);
        match clear {
            Some(bytes) => adapter
                .apply_output(&mut replica, bytes, &mut effects)
                .unwrap(),
            None => adapter.clear_presentation(&mut replica).unwrap(),
        }
        let cleared = replica.terminal().unwrap().scrollback_rows().unwrap();
        apply_history(&mut adapter, &mut replica, &captured, &mut effects);
        assert_eq!(
            replica.terminal().unwrap().scrollback_rows().unwrap(),
            cleared,
            "live clear {clear:?} must also discard deferred pre-clear history"
        );
    }
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
