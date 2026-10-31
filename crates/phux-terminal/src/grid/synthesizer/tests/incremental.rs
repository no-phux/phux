use super::*;

/// Resize between two pooled ticks, with a fresh-state walk interleaved:
/// the second tick must report the new dims and content.
#[test]
fn prepare_tick_serves_fresh_rows_after_resize() {
    let mut term = fresh(10, 2);
    term.vt_write(b"AA");
    let mut synth = SnapshotSynthesizer::new().expect("synth");

    let (c0, r0, _) = synth.prepare_tick(&term).expect("tick0");
    assert_eq!((c0, r0), (10, 2));
    assert!(
        synth.tick_rows.iter().any(|b| b.contains(&b'A')),
        "first tick should render the 'A' content"
    );

    // Grow the grid and overwrite row 0, then race a fresh-state walk
    // that consumes the terminal's per-row dirty bits.
    term.resize(10, 4, 0, 0).expect("resize");
    term.vt_write(b"\x1b[1;1HZZ");
    let _ = synth.synthesize(&term).expect("fresh walk");

    let (c1, r1, _) = synth.prepare_tick(&term).expect("tick1");
    assert_eq!((c1, r1), (10, 4), "second tick must observe the new dims");
    assert_eq!(
        synth.tick_rows.len(),
        4,
        "row buffer resized to the new grid"
    );
    assert!(
        synth.tick_rows.iter().any(|b| b.contains(&b'Z')),
        "post-resize tick must serve the fresh 'ZZ', not the stale cache"
    );
    assert_eq!(
        synth.pool.last_dims(),
        Some((10, 4)),
        "pool tracks the live dims"
    );
}

/// The reference diff trips on a 1049 -> primary transition.
#[test]
fn reference_diff_trips_on_alt_screen_transition() {
    let mut t = fresh(20, 4);
    t.vt_write(b"\x1b[?1049h");
    t.vt_write(b"alt body");

    let mut synth = SnapshotSynthesizer::new().expect("synth");
    let mut reference = ConsumerReference::new();
    synth
        .prime_reference(&t, &mut reference)
        .expect("prime_reference");

    // Leave the alt screen: a mode-only change, no row content edits
    // beyond what 1049's restore does.
    t.vt_write(b"\x1b[?1049l");

    let diff = synth
        .synthesize_against_reference(&t, &mut reference)
        .expect("diff");
    assert!(
        !diff.bytes.is_empty(),
        "a 1049->primary transition must produce a non-empty diff",
    );
    let bytes = String::from_utf8_lossy(&diff.bytes);
    assert!(
        bytes.contains("?1049l"),
        "the diff epilogue must re-emit ?1049l on leaving the alt screen; bytes={bytes:?}",
    );
}

/// A bare mouse-mode toggle (no row or cursor change) still diffs.
#[test]
fn reference_diff_trips_on_a_bare_mouse_mode_toggle() {
    let mut t = fresh(20, 4);
    t.vt_write(b"steady body");

    let mut synth = SnapshotSynthesizer::new().expect("synth");
    let mut reference = ConsumerReference::new();
    synth
        .prime_reference(&t, &mut reference)
        .expect("prime_reference");

    // Mode-only change: no glyphs, no cursor movement.
    t.vt_write(b"\x1b[?1000h\x1b[?1006h");

    let diff = synth
        .synthesize_against_reference(&t, &mut reference)
        .expect("diff");
    let bytes = String::from_utf8_lossy(&diff.bytes);
    assert!(
        bytes.contains("?1000h") && bytes.contains("?1006h"),
        "a bare mouse-mode toggle must re-emit the epilogue; bytes={bytes:?}",
    );
}

/// An unchanged terminal diffs to nothing, repeatedly.
#[test]
fn reference_diff_empty_when_unchanged() {
    let mut t = fresh(40, 10);
    t.vt_write(b"steady state line one\r\nand line two");

    let mut synth = SnapshotSynthesizer::new().expect("synth");
    let mut reference = ConsumerReference::new();
    synth
        .prime_reference(&t, &mut reference)
        .expect("prime_reference");

    for n in 0..3 {
        let diff = synth
            .synthesize_against_reference(&t, &mut reference)
            .expect("diff");
        assert!(
            diff.bytes.is_empty(),
            "unchanged terminal must diff empty on call {n}, got {:?}",
            String::from_utf8_lossy(&diff.bytes),
        );
    }
}

/// Every fresh-render-state walk drains the terminal's dirty bits; the
/// next tick must rebuild the pool rather than serve its cached rows.
#[test]
fn prepare_tick_serves_rows_a_foreign_walk_drained() {
    type ForeignWalk = fn(&mut SnapshotSynthesizer<'static>, &GhosttyTerminal<'static, 'static>);
    let walks: [(&str, ForeignWalk); 3] = [
        ("synthesize", |s, t| drop(s.synthesize(t).expect("walk"))),
        ("synthesize_with_scrollback", |s, t| {
            drop(s.synthesize_with_scrollback(t, Some(1)).expect("walk"));
        }),
        ("prime_reference", |s, t| {
            s.prime_reference(t, &mut ConsumerReference::new())
                .expect("walk");
        }),
    ];
    for (name, walk) in walks {
        let mut t = fresh(10, 3);
        t.vt_write(b"AA");
        let mut synth = SnapshotSynthesizer::new().expect("synth");
        synth.prepare_tick(&t).expect("tick0");
        t.vt_write(b"\x1b[2;1HZZ");
        walk(&mut synth, &t);
        synth.prepare_tick(&t).expect("tick1");
        assert!(
            synth.tick_rows[1].contains(&b'Z'),
            "{name}: the tick served the pool's stale row 1: {:?}",
            String::from_utf8_lossy(&synth.tick_rows[1]),
        );
        assert_eq!(synth.last_rendered_rows, 3, "{name}: a full render");
    }
}

/// A tick after a one-row write re-renders that row only, and a clean
/// terminal re-renders nothing.
#[test]
fn prepare_tick_renders_only_rows_the_pool_rebuilt() {
    let mut t = fresh(10, 4);
    // Park the cursor on row 1 first: moving it dirties the row it leaves.
    t.vt_write(b"one\r\ntwo\r\nthree\x1b[2;1H");
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    synth.prepare_tick(&t).expect("first tick");
    assert_eq!(synth.last_rendered_rows, 4, "the first tick is full");

    synth.prepare_tick(&t).expect("clean tick");
    assert_eq!(synth.last_rendered_rows, 0, "a clean tick renders nothing");

    t.vt_write(b"TWO");
    synth.prepare_tick(&t).expect("partial tick");
    assert_eq!(synth.last_rendered_rows, 1, "only the written row");
    assert!(synth.tick_rows[1].starts_with(b"\x1b[0mTWO"));
    assert!(synth.tick_rows[0].starts_with(b"\x1b[0mone"), "kept row 0");
}

/// The agent detector's and `GET_SCREEN`'s viewport projection reads the
/// pool without clearing its flags (phux-69pq.14): it sees the live
/// write, and the next tick still re-renders exactly the written row.
#[test]
fn screen_state_leaves_the_next_tick_incremental() {
    let mut t = fresh(10, 4);
    t.vt_write(b"one\r\ntwo\r\nthree\x1b[2;1H");
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    synth.prepare_tick(&t).expect("first tick");

    t.vt_write(b"TWO");
    let screen = synth.screen_state(&t, 0).expect("projection");
    assert_eq!(screen.lines[1], "TWO", "the projection sees the write");
    // Twice: a clean re-read must not lose the pending row either.
    let _ = synth.screen_state(&t, 0).expect("second projection");
    synth.prepare_tick(&t).expect("partial tick");
    assert_eq!(synth.last_rendered_rows, 1, "only the written row");
    assert!(synth.tick_rows[1].starts_with(b"\x1b[0mTWO"));
}

/// A projection after a foreign walk honours the pending rebuild, so it
/// cannot serve the rows that walk drained, and the rebuild's full
/// redraw still reaches the next tick.
#[test]
fn screen_state_after_a_foreign_walk_serves_live_rows() {
    let mut t = fresh(10, 3);
    t.vt_write(b"AA");
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    synth.prepare_tick(&t).expect("tick0");
    t.vt_write(b"\x1b[2;1HZZ");
    drop(synth.synthesize(&t).expect("foreign walk"));
    let screen = synth.screen_state(&t, 0).expect("projection");
    assert_eq!(screen.lines[1], "ZZ", "the projection served a stale row");
    synth.prepare_tick(&t).expect("tick1");
    assert!(synth.tick_rows[1].contains(&b'Z'));
    assert_eq!(synth.last_rendered_rows, 3, "the rebuild renders in full");
}
