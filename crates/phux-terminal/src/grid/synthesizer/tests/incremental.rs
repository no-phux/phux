use super::*;

/// A mode-only change (no glyph or cursor edit beyond what the mode itself
/// does) still trips the reference diff, which re-emits the mode.
#[test]
fn reference_diff_trips_on_a_mode_only_change() {
    for (label, setup, change, needles) in [
        (
            "leaving the 1049 alt screen",
            b"\x1b[?1049halt body".as_slice(),
            b"\x1b[?1049l".as_slice(),
            ["?1049l"].as_slice(),
        ),
        (
            "a bare mouse-mode toggle",
            b"steady body",
            b"\x1b[?1000h\x1b[?1006h",
            &["?1000h", "?1006h"],
        ),
    ] {
        let mut t = fresh(20, 4);
        t.vt_write(setup);
        let mut synth = SnapshotSynthesizer::new().expect("synth");
        let mut reference = ConsumerReference::new();
        synth
            .prime_reference(&t, &mut reference)
            .expect("prime_reference");

        t.vt_write(change);

        let diff = synth
            .synthesize_against_reference(&t, &mut reference)
            .expect("diff");
        let bytes = String::from_utf8_lossy(&diff.bytes);
        for needle in needles {
            assert!(
                bytes.contains(needle),
                "{label}: the diff epilogue must re-emit {needle}; bytes={bytes:?}",
            );
        }
    }
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
