use super::*;

/// A 1049 alt-screen snapshot re-establishes `?1049h`, before the
/// cursor restore.
#[test]
fn snapshot_reestablishes_alt_screen_1049() {
    let mut t = fresh(20, 4);
    t.vt_write(b"\x1b[?1049h");
    t.vt_write(b"alt-screen body");

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

/// One snapshot round-trip: `writes` set up the source, whose modes are
/// first checked against `source`; the snapshot bytes must contain every
/// `emits`; and a mirror replaying them must match `mirror`.
struct ModeCase {
    label: &'static str,
    writes: &'static [u8],
    source: &'static [(Mode, &'static str, bool)],
    emits: &'static [&'static str],
    mirror: &'static [(Mode, &'static str, bool)],
}

/// Mouse tracking, alt scroll, and the alt-screen modes survive the
/// snapshot; a mode the pane never set stays off.
#[test]
fn snapshot_round_trips_terminal_modes() {
    let cases = [
        ModeCase {
            // The DECSET set opencode / Claude Code were probed to use.
            label: "mouse tracking",
            writes: b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006htui body",
            source: &[],
            emits: &[],
            mirror: &[
                (Mode::NORMAL_MOUSE, "1000", true),
                (Mode::BUTTON_MOUSE, "1002", true),
                (Mode::ANY_MOUSE, "1003", true),
                (Mode::SGR_MOUSE, "1006", true),
            ],
        },
        ModeCase {
            label: "plain pane never asked for the mouse",
            writes: b"plain shell",
            source: &[],
            emits: &[],
            mirror: &[
                (Mode::X10_MOUSE, "9", false),
                (Mode::NORMAL_MOUSE, "1000", false),
                (Mode::BUTTON_MOUSE, "1002", false),
                (Mode::ANY_MOUSE, "1003", false),
            ],
        },
        ModeCase {
            label: "alt-scroll opt-out",
            writes: b"\x1b[?1049h\x1b[?1007l",
            source: &[(Mode::ALT_SCROLL, "1007", false)],
            emits: &[],
            mirror: &[(Mode::ALT_SCROLL, "1007", false)],
        },
        ModeCase {
            label: "legacy 47 alt screen",
            writes: b"\x1b[?47hlegacy alt",
            source: &[(Mode::ALT_SCREEN_LEGACY, "47", true)],
            emits: &["?47h"],
            mirror: &[],
        },
        ModeCase {
            label: "primary screen reports every alt mode off",
            writes: b"primary content",
            source: &[],
            emits: &["?47l", "?1047l", "?1049l"],
            mirror: &[],
        },
    ];
    for case in cases {
        let label = case.label;
        let mut t = fresh(20, 4);
        t.vt_write(case.writes);
        for &(mode, code, on) in case.source {
            assert_eq!(
                t.mode(mode).expect("source mode"),
                on,
                "{label}: source ?{code}"
            );
        }

        let snap = synthesize(&t).expect("synth");
        let bytes = String::from_utf8_lossy(&snap.bytes);
        for needle in case.emits {
            assert!(
                bytes.contains(needle),
                "{label}: must emit {needle}; bytes={bytes:?}"
            );
        }
        let mut mirror = fresh(snap.cols, snap.rows);
        mirror.vt_write(&snap.bytes);
        for &(mode, code, on) in case.mirror {
            assert_eq!(
                mirror.mode(mode).expect("mirror mode"),
                on,
                "{label}: mirror ?{code} after replaying the snapshot",
            );
        }
    }
}
