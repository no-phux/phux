//! `SnapshotSynthesizer` round-trips on bare libghostty terminals (no PTY, no
//! server): the per-consumer reference diff, applied to a mirror,
//! reconstructs the source across churn and resizes (ADR-0018).

use phux_server::grid::{ConsumerReference, SCROLLBACK_ALL, SnapshotSynthesizer, synthesize};

use super::common::{fresh, render_grid};

/// Mirror primed from `synthesize`, fed each reference diff, must equal the
/// source after every step: churn, alt-screen enter/leave, grow resize, and
/// sparse edits across distant rows.
#[test]
fn reference_diff_roundtrips_through_churn_alt_screen_and_resize() {
    enum Step {
        Write(Vec<u8>),
        Resize(u16, u16),
    }
    let sparse: Vec<u8> = [5u16, 15, 25]
        .iter()
        .flat_map(|r| format!("\x1b[{};1H\x1b[2KEDIT-{r}", r + 1).into_bytes())
        .collect();
    let steps = [
        Step::Write(b"\r\nmore primary output".to_vec()),
        Step::Write(b"\x1b[?1049h\x1b[2J\x1b[Halt screen!!!\r\nalt line two".to_vec()),
        Step::Write(b"\x1b[?1049l\r\nback on primary".to_vec()),
        Step::Resize(40, 30),
        Step::Write(b"\r\nafter grow".to_vec()),
        Step::Write(sparse),
    ];

    let mut src = fresh(40, 10);
    src.vt_write(b"initial primary content\r\nsecond line");
    let snap = synthesize(&src).unwrap();
    let mut mirror = fresh(snap.cols, snap.rows);
    mirror.vt_write(&snap.bytes);
    let mut synth = SnapshotSynthesizer::new().unwrap();
    let mut reference = ConsumerReference::new();
    synth.prime_reference(&src, &mut reference).unwrap();

    for (n, step) in steps.iter().enumerate() {
        match step {
            Step::Write(bytes) => src.vt_write(bytes),
            Step::Resize(cols, rows) => src.resize(*cols, *rows, 8, 16).unwrap(),
        }
        let diff = synth
            .synthesize_against_reference(&src, &mut reference)
            .unwrap();
        mirror.resize(diff.cols, diff.rows, 8, 16).unwrap();
        mirror.vt_write(&diff.bytes);
        assert_eq!(
            render_grid(&src),
            render_grid(&mirror),
            "diverged after step {n}"
        );
    }
    let unchanged = synth
        .synthesize_against_reference(&src, &mut reference)
        .unwrap();
    assert!(unchanged.bytes.is_empty(), "no change -> empty diff");

    src.resize(10, 3, 8, 16).unwrap();
    let shrunk = synth
        .synthesize_against_reference(&src, &mut reference)
        .unwrap();
    assert_eq!((shrunk.cols, shrunk.rows), (10, 3));
}

/// Scrollback projection equals what libghostty retains and clamps
/// oversized requests, never over-reading.
#[test]
fn scrollback_projection_is_bounded_by_retained_history() {
    let mut t = libghostty_vt::Terminal::new(10, 2).unwrap();
    t.set_scrollback_max_lines(Some(5)).unwrap();
    for i in 0..100u32 {
        t.vt_write(format!("L{i}\r\n").as_bytes());
    }
    let total = t.scrollback_rows().unwrap();
    let synth = SnapshotSynthesizer::new().unwrap();
    for want in [SCROLLBACK_ALL, u32::MAX] {
        let screen = synth
            .screen_state_with_scrollback(&t, 0, Some(want), false)
            .unwrap();
        assert_eq!(screen.scrollback.len(), total, "request {want}");
    }
}
