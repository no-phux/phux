use super::*;

/// `cup_len` matches what `write_cup` writes at every digit boundary, so
/// a diff buffer sized from it is exact.
#[test]
fn cup_len_matches_the_written_sequence() {
    for row in [0, 8, 9, 98, 99, 998, 999, 9_998, 9_999, u16::MAX] {
        for col in [0, 9, 99, u16::MAX] {
            let mut out = Vec::new();
            write_cup(&mut out, row, col);
            assert_eq!(cup_len(row, col), out.len(), "row {row} col {col}");
        }
    }
}

/// Assembled diffs keep their byte layout: toggle, then each changed row
/// as CUP + pen reset + body, then the epilogue.
#[test]
fn assemble_diff_lays_out_toggle_rows_and_epilogue() {
    let bodies = vec![b"zero".to_vec(), b"one".to_vec(), b"two".to_vec()];
    let out = assemble_diff(b"T", &[0, 2], &bodies, b"E");
    assert_eq!(out, b"T\x1b[1;1H\x1b[0mzero\x1b[3;1H\x1b[0mtwoE");
}

#[test]
fn synthesizer_returns_dimensions() {
    let terminal = fresh(80, 24);
    let snap = synthesize(&terminal).expect("synth");
    assert_eq!(snap.cols, 80);
    assert_eq!(snap.rows, 24);
    // First bytes should be the reset prelude.
    assert!(snap.bytes.starts_with(b"\x1b[!p\x1b[2J\x1b[H"));
    // No scrollback requested ⇒ no scrollback-priming bytes.
    assert!(snap.scrollback.is_empty());
}

#[test]
fn bounded_synthesis_rejects_tiny_source_budget() {
    let terminal = fresh(80, 24);
    assert!(matches!(
        SnapshotSynthesizer::synthesize_bounded(
            &terminal,
            4,
            phux_protocol::caps::ImageProtocolSet::all()
        ),
        Err(SynthesisError::LimitExceeded)
    ));
}

#[test]
fn synthesizer_round_trips_via_libghostty() {
    // Replayed snapshot reproduces the cursor position.
    let mut a = fresh(20, 5);
    a.vt_write(b"hello\r\nworld");
    let synth = synthesize(&a).expect("synth");

    let mut b = fresh(synth.cols, synth.rows);
    b.vt_write(&synth.bytes);

    // Both terminals should report cursor at the end of "world" on row 1.
    let ax = a.cursor_x().expect("cursor_x a");
    let ay = a.cursor_y().expect("cursor_y a");
    let bx = b.cursor_x().expect("cursor_x b");
    let by = b.cursor_y().expect("cursor_y b");
    assert_eq!((ax, ay), (bx, by), "cursor position should round-trip");
}

/// Reusing one synthesizer across attaches still yields the full grid
/// after a prior `synthesize()` consumed the dirty bits.
#[test]
fn synthesize_reused_across_calls_emits_full_snapshot_each_time() {
    let mut t = fresh(20, 5);
    let synth = SnapshotSynthesizer::new().expect("synth");

    // First attach: blank grid (no content yet).
    let snap1 = synth.synthesize(&t).expect("synth1");
    assert!(
        !String::from_utf8_lossy(&snap1.bytes).contains("MARKER"),
        "blank grid should not carry MARKER yet",
    );

    // Content arrives with NO trailing newline (like `printf MARKER`).
    t.vt_write(b"MARKER");

    // Second attach via the SAME synthesizer: the snapshot MUST be a full
    // repaint carrying MARKER, not a delta against the consumed dirty bits.
    let snap2 = synth.synthesize(&t).expect("synth2");
    let body = String::from_utf8_lossy(&snap2.bytes);
    assert!(
        body.contains("MARKER"),
        "phux-uow0: reused synthesizer must emit a FULL snapshot including \
             content written after the first call; got: {body:?}",
    );

    // And it must round-trip into a fresh terminal's grid.
    let mut b = fresh(snap2.cols, snap2.rows);
    b.vt_write(&snap2.bytes);
    assert_eq!(render_grid(&b)[0], "MARKER              ");
}

/// Another render state consuming the dirty bits before the snapshot
/// must not blank it: `synthesize()` is full, not a delta.
#[test]
fn full_snapshot_survives_another_consumer_eating_dirty_bits() {
    let mut t = fresh(20, 5);
    let snap_synth = SnapshotSynthesizer::new().expect("snap_synth");

    // Client 1 attaches: synthesize the (blank) grid; this consumes dirty
    // and leaves snap_synth's reference clean.
    let _ = snap_synth.synthesize(&t).expect("synth client1");

    // Content arrives with no trailing newline (printf-style).
    t.vt_write(b"MARKER");

    // Client 2's register_consumer primes a SEPARATE per-consumer reference,
    // whose update consumes the Terminal's freshly-set dirty bits.
    let mut other = SnapshotSynthesizer::new().expect("other");
    let _ = other.screen_state(&t, 0).expect("other screen_state");

    // Client 2's snapshot via the SHARED synthesizer must still carry MARKER.
    let snap = snap_synth.synthesize(&t).expect("synth client2");
    let body = String::from_utf8_lossy(&snap.bytes);
    assert!(
        body.contains("MARKER"),
        "phux-uow0: a full snapshot must emit the whole grid even when another \
             consumer already consumed the per-row dirty bits; got: {body:?}",
    );
}

/// A wide glyph's tail is skipped, not replayed as a space.
#[test]
fn synthesizer_skips_wide_cell_tails() {
    let mut a = fresh(10, 2);
    // Two CJK glyphs (4 columns wide total) followed by ASCII.
    a.vt_write("你好ab".as_bytes());

    // Sanity: the source grid should contain both wide glyphs.
    let src_grid = render_grid(&a);
    assert_eq!(src_grid[0], "你好ab    ", "source grid layout");

    let synth = synthesize(&a).expect("synth");
    let mut b = fresh(synth.cols, synth.rows);
    b.vt_write(&synth.bytes);

    let dst_grid = render_grid(&b);
    assert_eq!(
        src_grid, dst_grid,
        "grid must round-trip through synthesizer for wide glyphs"
    );

    // Bytes must NOT contain a stray space between the two wide
    // glyphs (`你` followed by ` ` would be the wide-tail bug).
    let bytes_str = String::from_utf8_lossy(&synth.bytes);
    assert!(
        bytes_str.contains("你好"),
        "synthesized bytes should contain consecutive wide glyphs, got: {bytes_str:?}"
    );
    assert!(
        !bytes_str.contains("你 好"),
        "synthesized bytes must not insert a space between wide glyphs (wide-tail bug)"
    );
}

/// Mixed CJK, emoji, and ASCII round-trip cell-for-cell.
#[test]
fn synthesizer_round_trips_cjk_and_emoji() {
    let mut a = fresh(20, 4);
    // Row 0: CJK + ASCII. Row 1: pure emoji. Row 2: mixed
    // emoji/ASCII. The CRLF sequences keep row layout deterministic.
    a.vt_write("東 hello\r\n".as_bytes());
    a.vt_write("😀😀😀\r\n".as_bytes());
    a.vt_write("a😀b".as_bytes());

    let src_grid = render_grid(&a);

    let synth = synthesize(&a).expect("synth");
    let mut b = fresh(synth.cols, synth.rows);
    b.vt_write(&synth.bytes);

    let dst_grid = render_grid(&b);
    assert_eq!(
        src_grid, dst_grid,
        "CJK + emoji content must round-trip through the synthesizer"
    );

    assert!(
        src_grid[0].starts_with('東'),
        "source row 0 should start with 東, got {:?}",
        src_grid[0]
    );
}
