use super::*;

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
