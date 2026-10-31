use super::*;

#[test]
fn snapshot_split_leaves_history_suffix() {
    let mut source = history_terminal();
    let mut encoded = Vec::new();
    source.encode_snapshot(&mut encoded).expect("encode");
    let ready_at = snapshot_ready_offset(&encoded).expect("ready");
    assert!(
        ready_at < encoded.len(),
        "READY offset {ready_at} consumed the whole {}-byte snapshot; history suffix is empty",
        encoded.len()
    );
}

#[test]
fn advertises_official_progressive_snapshot_v1() {
    assert!(official_snapshot_available());
    let advertised = native_bootstrap_capabilities();
    assert!(
        advertised
            .profiles
            .contains(BootstrapProfileKind::NativeState)
    );
    assert!(
        advertised
            .native_codecs
            .contains(EngineCodec::LibghosttySnapshotV1)
    );
    assert!(
        !advertised
            .native_codecs
            .contains(EngineCodec::LibghosttyCheckpointV2)
    );
    assert_eq!(
        advertised.native_features,
        EngineFeatureSet::required_native()
    );
}

/// READY records are the active area. Extra scrollback must not enlarge
/// the prefix; it is pulled later as frozen history.
#[test]
fn ready_prefix_bytes_do_not_grow_with_scrollback() {
    fn ready_prefix_len(lines: usize) -> usize {
        let mut terminal = GhosttyTerminal::new(80, 24).expect("terminal");
        terminal
            .set_scrollback_max_lines(Some(100_000))
            .expect("history rows");
        terminal
            .set_scrollback_max_bytes(None)
            .expect("history bytes");
        terminal
            .set_continuation_max_bytes(CONTINUATION_LIMIT)
            .expect("continuation");
        for row in 0..lines {
            terminal.vt_write(format!("history-{row:04}\r\n").as_bytes());
        }
        let limits = BootstrapLimits::default();
        let mut manager = NativeTerminalManager::new(terminal, 1).expect("manager");
        let mut capture = manager
            .begin_generation_capture(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
            .expect("capture");
        let mut total = 0usize;
        loop {
            let required = match capture.step(&mut []) {
                Err(NativeStateError::OutOfSpace { required_bytes, .. }) => required_bytes,
                other => panic!("probe: {other:?}"),
            };
            let mut exact = vec![0; required];
            let chunk = capture.step(&mut exact).expect("record");
            total = total.saturating_add(chunk.bytes.len());
            if matches!(chunk.kind, NativeCheckpointChunkKind::Ready) {
                break;
            }
        }
        manager.abort_generation_capture(capture);
        total
    }

    let shallow = ready_prefix_len(80);
    let deep = ready_prefix_len(2_000);
    assert!(
        deep <= shallow.saturating_mul(2).saturating_add(4096),
        "READY prefix grew with scrollback: 80 lines {shallow} bytes, \
             2000 lines {deep} bytes"
    );
}

#[test]
fn shared_generation_is_content_addressed() {
    let limits = BootstrapLimits::new(phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES, 64 * 1024)
        .expect("limits");
    let mut manager = NativeTerminalManager::new(history_terminal(), 1).expect("manager");
    let mut capture = manager
        .capture_generation_bounded(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
        .expect("capture");
    drain_ready(&mut capture);
    let (cursor, seed) = manager.finish_generation_capture(capture).expect("finish");
    let bounds = seed.bounds();
    manager
        .install_generation(
            cursor,
            seed,
            bounds,
            bounds.required_reserved_bytes().expect("res"),
        )
        .expect("install");
    manager.retain_generation(&cursor).expect("retain");
    let first = next_record(&mut manager, &cursor, 0, limits);
    let again = next_record(&mut manager, &cursor, 0, limits);
    assert_eq!(first.bytes.as_ptr(), again.bytes.as_ptr());
    manager.release_generation(&cursor).expect("r1");
    manager.release_generation(&cursor).expect("r2");
    assert!(!manager.generations.contains_key(&cursor));
}

#[test]
fn managed_capture_defers_multiple_pages_and_finish_past_ready() {
    let limits = BootstrapLimits::new(phux_protocol::DEFAULT_BOOTSTRAP_CHUNK_BYTES, 64 * 1024)
        .expect("limits");
    let mut source = terminal(200, 3);
    source
        .set_scrollback_max_lines(Some(5_000))
        .expect("history rows");
    source
        .set_scrollback_max_bytes(None)
        .expect("history bytes");
    for row in 0..3_000 {
        source.vt_write(format!("history-{row:04}\r\n").as_bytes());
    }
    let mut manager = NativeTerminalManager::new(source, 1).expect("manager");
    let mut capture = manager
        .capture_generation_bounded(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
        .expect("capture");
    drain_ready(&mut capture);
    let (cursor, seed) = manager.finish_generation_capture(capture).expect("READY");
    let bounds = seed.bounds();
    manager
        .install_generation(
            cursor,
            seed,
            bounds,
            bounds.required_reserved_bytes().expect("reservation"),
        )
        .expect("install");

    let mut index = 0;
    let mut pages = 0;
    loop {
        let record = next_record(&mut manager, &cursor, index, limits);
        assert!(
            !record.bytes.is_empty(),
            "PAGE and FINISH carry native authentication"
        );
        if record.finish {
            assert_eq!(record.rows, 0);
            break;
        }
        assert!(record.rows > 0);
        pages += 1;
        if pages == 1 {
            manager.vt_write(b"live-between-history-pages\r\n");
        }
        index += 1;
    }
    assert!(pages >= 2, "fixture must produce multiple deferred pages");
}

#[test]
fn repeated_ready_prefixes_receive_distinct_generation_cursors() {
    let limits = BootstrapLimits::default();
    let mut manager = NativeTerminalManager::new(history_terminal(), 2).expect("manager");
    let mut first = manager.capture(limits).expect("first capture");
    drain_ready(&mut first);
    let (first_cursor, first_seed) = manager
        .finish_generation_capture(first)
        .expect("first READY");
    drop(first_seed);

    let mut second = manager.capture(limits).expect("second capture");
    drain_ready(&mut second);
    let (second_cursor, second_seed) = manager
        .finish_generation_capture(second)
        .expect("second READY");
    drop(second_seed);

    assert_ne!(
        first_cursor, second_cursor,
        "generation freshness must not depend on history bytes being encoded before READY"
    );
}

#[test]
fn old_history_cut_waits_while_a_new_prefix_temporarily_owns_the_terminal() {
    let limits = BootstrapLimits::default();
    let mut manager = NativeTerminalManager::new(history_terminal(), 2).expect("manager");
    let mut first = manager.capture(limits).expect("first capture");
    drain_ready(&mut first);
    let (cursor, seed) = manager
        .finish_generation_capture(first)
        .expect("first READY");
    let bounds = seed.bounds();
    manager
        .install_generation(
            cursor,
            seed,
            bounds,
            bounds.required_reserved_bytes().expect("reservation"),
        )
        .expect("install old cut");

    let second = manager.capture(limits).expect("second prefix");
    assert!(matches!(
        manager.history_record_at(
            &cursor,
            0,
            limits.max_history_page_bytes(),
            phux_protocol::MAX_HISTORY_PAGE_ROWS,
        ),
        Err(NativeStateError::ImportBusy)
    ));
    assert!(manager.has_generation(&cursor));

    manager.abort_generation_capture(second);
    let record = next_record(&mut manager, &cursor, 0, limits);
    assert!(!record.bytes.is_empty());
    assert!(manager.has_generation(&cursor));
}

/// Attach cost does not scale with `defaults.history-bytes` (16 MiB costs
/// at most 3x 2 MiB plus 2 ms of noise).
#[test]
fn attach_detach_cost_is_flat_in_retained_history() {
    let limits = BootstrapLimits::default();
    let mut shallow =
        NativeTerminalManager::new(deep_terminal(2 * 1024 * 1024), 4).expect("shallow manager");
    let shallow_cost = best_detach_cost(&mut shallow, limits, 7);
    let mut deep =
        NativeTerminalManager::new(deep_terminal(16 * 1024 * 1024), 4).expect("deep manager");
    let deep_cost = best_detach_cost(&mut deep, limits, 7);

    let allowed = shallow_cost.saturating_mul(3) + std::time::Duration::from_millis(2);
    assert!(
        deep_cost <= allowed,
        "detach cost grew with retained history: 2 MiB {shallow_cost:?}, \
             16 MiB {deep_cost:?}, allowed {allowed:?}; a leased cut encodes \
             nothing at attach"
    );
}

/// A page evicted between cursor issue and request is a typed engine
/// status, never stale bytes.
#[test]
fn history_evicted_under_a_live_lease_tombstones_rather_than_lying() {
    let limits = BootstrapLimits::default();
    let mut manager =
        NativeTerminalManager::new(deep_terminal(2 * 1024 * 1024), 4).expect("native manager");
    let (cursor, max_bytes, max_rows) = install_leased_generation(&mut manager, limits);
    history_at(&mut manager, &cursor, 0, max_bytes, max_rows)
        .expect("HISTORY_BEGIN from a live lease");

    for row in 0..200_000 {
        manager.vt_write(format!("evicting-{row:06}\r\n").as_bytes());
    }

    let error = history_at(&mut manager, &cursor, 1, max_bytes, max_rows)
        .expect_err("a pruned lease cannot serve a page");
    assert!(
        is_lease_tombstone(error),
        "eviction must name its cause; `history_tombstone_reason` turns \
             exactly these into a tombstone the client can act on, and anything \
             else into an opaque CodecFailure. Got {error:?}"
    );
}
