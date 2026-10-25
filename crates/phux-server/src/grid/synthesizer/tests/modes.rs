use super::*;

/// A 1049 alt-screen snapshot re-establishes `?1049h`, before the
/// cursor restore.
#[test]
fn snapshot_reestablishes_alt_screen_1049() {
    let mut t = fresh(20, 4);
    t.vt_write(b"\x1b[?1049h");
    t.vt_write(b"alt-screen body");

    // libghostty keeps 47 and 1049 as distinct bits.
    assert!(
        t.mode(Mode::ALT_SCREEN_SAVE).expect("mode 1049"),
        "?1049h should set ALT_SCREEN_SAVE",
    );
    assert!(
        !t.mode(Mode::ALT_SCREEN_LEGACY).expect("mode 47"),
        "1049 must not set the legacy-47 bit",
    );

    let snap = synthesize(&t).expect("synth");
    let bytes = String::from_utf8_lossy(&snap.bytes);

    assert!(
        bytes.contains("?1049h"),
        "snapshot must re-emit ?1049h so the mirror lands on the alt screen; bytes={bytes:?}",
    );
    assert!(
        !bytes.contains("?47h"),
        "47 is off; snapshot must not assert ?47h; bytes={bytes:?}",
    );
    // `?25h`/`?25l` follows the cursor restore, so it marks "cursor
    // re-established".
    let pos_1049 = bytes.find("?1049h").expect("?1049h present");
    let pos_cursor_vis = bytes
        .find("?25h")
        .or_else(|| bytes.find("?25l"))
        .expect("epilogue cursor-visibility present");
    assert!(
        pos_1049 < pos_cursor_vis,
        "?1049h (at {pos_1049}) must precede the epilogue cursor block (at {pos_cursor_vis}); bytes={bytes:?}",
    );
}

/// Mouse-tracking modes survive the snapshot, checked on the replayed
/// mirror's mode state.
#[test]
fn snapshot_reestablishes_mouse_tracking_modes() {
    // The DECSET set opencode / Claude Code were probed to use.
    let mut t = fresh(20, 4);
    t.vt_write(b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h");
    t.vt_write(b"tui body");

    let snap = synthesize(&t).expect("synth");
    let mut mirror = fresh(snap.cols, snap.rows);
    mirror.vt_write(&snap.bytes);

    for (mode, code) in [
        (Mode::NORMAL_MOUSE, "1000"),
        (Mode::BUTTON_MOUSE, "1002"),
        (Mode::ANY_MOUSE, "1003"),
        (Mode::SGR_MOUSE, "1006"),
    ] {
        assert!(
            mirror.mode(mode).expect("mirror mode"),
            "mirror must have ?{code}h after replaying the snapshot",
        );
    }
}

/// A pane that never asked for the mouse replays with tracking off.
#[test]
fn snapshot_leaves_mouse_tracking_off_for_a_plain_pane() {
    let mut t = fresh(20, 4);
    t.vt_write(b"plain shell");

    let snap = synthesize(&t).expect("synth");
    let mut mirror = fresh(snap.cols, snap.rows);
    mirror.vt_write(&snap.bytes);

    for (mode, code) in [
        (Mode::X10_MOUSE, "9"),
        (Mode::NORMAL_MOUSE, "1000"),
        (Mode::BUTTON_MOUSE, "1002"),
        (Mode::ANY_MOUSE, "1003"),
    ] {
        assert!(
            !mirror.mode(mode).expect("mirror mode"),
            "?{code} must stay off for a pane that never enabled the mouse",
        );
    }
}

/// An app's `?1007l` opt-out survives the snapshot.
#[test]
fn snapshot_preserves_an_alt_scroll_opt_out() {
    let mut t = fresh(20, 4);
    t.vt_write(b"\x1b[?1049h\x1b[?1007l");
    assert!(
        !t.mode(Mode::ALT_SCROLL).expect("mode 1007"),
        "?1007l should clear ALT_SCROLL",
    );

    let snap = synthesize(&t).expect("synth");
    let mut mirror = fresh(snap.cols, snap.rows);
    mirror.vt_write(&snap.bytes);

    assert!(
        !mirror.mode(Mode::ALT_SCROLL).expect("mirror mode 1007"),
        "the mirror must inherit the ?1007l opt-out",
    );
}

/// Legacy `?47h` round-trips.
#[test]
fn snapshot_reestablishes_alt_screen_47_legacy() {
    let mut t = fresh(20, 4);
    t.vt_write(b"\x1b[?47h");
    t.vt_write(b"legacy alt");
    assert!(t.mode(Mode::ALT_SCREEN_LEGACY).expect("mode 47"));

    let snap = synthesize(&t).expect("synth");
    let bytes = String::from_utf8_lossy(&snap.bytes);
    assert!(
        bytes.contains("?47h"),
        "snapshot must re-emit ?47h for a legacy alt-screen program; bytes={bytes:?}",
    );
}

/// On the primary screen every alt-screen mode is reported off.
#[test]
fn snapshot_primary_screen_emits_all_alt_modes_off() {
    let mut t = fresh(20, 4);
    t.vt_write(b"primary content");

    let snap = synthesize(&t).expect("synth");
    let bytes = String::from_utf8_lossy(&snap.bytes);
    assert!(bytes.contains("?47l"), "47 off on primary; bytes={bytes:?}");
    assert!(
        bytes.contains("?1047l"),
        "1047 off on primary; bytes={bytes:?}",
    );
    assert!(
        bytes.contains("?1049l"),
        "1049 off on primary; bytes={bytes:?}",
    );
}
