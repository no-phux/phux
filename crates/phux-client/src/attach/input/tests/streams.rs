//! Stateful read boundaries, mouse, focus, paste, and recovery behavior.
use super::*;

#[test]
fn mouse_reports_decode() {
    use MouseAction::{Motion, Press, Release};
    use MouseButton::{Five, Four, Left, Middle, Right, Unknown};
    let shift_alt_ctrl = ModSet::SHIFT | ModSet::ALT | ModSet::CTRL;
    #[allow(clippy::type_complexity, reason = "test table")]
    let cases: &[(&[u8], MouseAction, MouseButton, ModSet, f64, f64)] = &[
        // SGR: 1-indexed cells become 0-indexed coordinates.
        (b"\x1b[<0;5;3M", Press, Left, ModSet::empty(), 4.0, 2.0),
        (b"\x1b[<0;5;3m", Release, Left, ModSet::empty(), 4.0, 2.0),
        (b"\x1b[<2;1;1M", Press, Right, ModSet::empty(), 0.0, 0.0),
        (b"\x1b[<1;1;1M", Press, Middle, ModSet::empty(), 0.0, 0.0),
        (b"\x1b[<32;10;5M", Motion, Left, ModSet::empty(), 9.0, 4.0),
        (b"\x1b[<35;1;1M", Motion, Unknown, ModSet::empty(), 0.0, 0.0),
        (b"\x1b[<64;1;1M", Press, Four, ModSet::empty(), 0.0, 0.0),
        (b"\x1b[<65;1;1M", Press, Five, ModSet::empty(), 0.0, 0.0),
        (b"\x1b[<28;1;1M", Press, Left, shift_alt_ctrl, 0.0, 0.0),
        // X10 raw bytes: every value offset by 32; a release names no button.
        (
            b"\x1b[M\x20\x21\x21",
            Press,
            Left,
            ModSet::empty(),
            0.0,
            0.0,
        ),
        (
            b"\x1b[M\x23\x25\x23",
            Release,
            Unknown,
            ModSet::empty(),
            4.0,
            2.0,
        ),
        (b"\x1b[M\x24\x21\x21", Press, Left, ModSet::SHIFT, 0.0, 0.0),
        // A high payload byte is data, not a new sequence.
        (
            b"\x1b[M\x20\x21\xff",
            Press,
            Left,
            ModSet::empty(),
            0.0,
            222.0,
        ),
        // urxvt-1015: decimal params, button offset by 32.
        (b"\x1b[32;5;3M", Press, Left, ModSet::empty(), 4.0, 2.0),
        (b"\x1b[35;5;3M", Release, Unknown, ModSet::empty(), 4.0, 2.0),
        (b"\x1b[96;1;1M", Press, Four, ModSet::empty(), 0.0, 0.0),
    ];
    for &(input, action, button, mods, x, y) in cases {
        let ev = one_mouse(input);
        assert_eq!(
            (ev.action, ev.button, ev.mods),
            (action, button, mods),
            "{input:?}"
        );
        assert!((ev.x - x).abs() < f64::EPSILON && (ev.y - y).abs() < f64::EPSILON);
    }
}

#[test]
fn focus_reports_decode() {
    for (input, expected) in [
        (b"\x1b[I", FocusEvent::Gained),
        (b"\x1b[O", FocusEvent::Lost),
    ] {
        assert!(matches!(one_event(input), InputEvent::Focus(f) if f == expected));
    }
}

#[test]
fn bracketed_paste_keeps_its_payload_verbatim() {
    for payload in [
        b"hello world".as_slice(),
        b"red\x1b[31mthing\x1b[0mend",
        b"a\x1bb",
    ] {
        let mut framed = b"\x1b[200~".to_vec();
        framed.extend_from_slice(payload);
        framed.extend_from_slice(b"\x1b[201~");
        let paste = one_paste(&framed);
        assert_eq!(paste.data, payload);
        assert_eq!(paste.trust, PasteTrust::Trusted);
    }
}

/// A read boundary can fall anywhere; splitting must not change the events.
#[test]
fn every_read_split_decodes_like_one_read() {
    let inputs: &[&[u8]] = &[
        b"\x1b[1;5A",
        b"\x1b[<0;5;3M",
        b"\x1b[M\x20\x21\x21",
        b"\x1b[97;1:1:97:769u",
        "😀".as_bytes(),
        b"\x1b[200~if true; then\r\n\tprintf 'hi'\n\nfi\n\x1b[201~",
    ];
    for input in inputs {
        let whole = StdinParser::new().feed(input);
        for split in 1..input.len() {
            let mut p = StdinParser::new();
            let mut evs = p.feed(&input[..split]);
            evs.extend(p.feed(&input[split..]));
            assert_eq!(evs, whole, "input {input:?} split at {split}");
            assert!(is_ground(&p));
        }
    }
}

#[test]
fn malformed_or_unknown_sequences_drop_and_recover() {
    // An over-long CSI is abandoned; the parser must not stay stuck in it.
    let mut overlong = b"\x1b[".to_vec();
    overlong.extend(std::iter::repeat_n(b'1', 300));
    let mut p = StdinParser::new();
    p.feed(&overlong);
    assert!(is_ground(&p));

    for input in [
        b"\x1b[1;2z".as_slice(),
        b"\x1b[u",
        // A two-param urxvt-shaped report, an unknown `~` key, and a
        // focus letter carrying params all drop.
        b"\x1b[5;3M",
        b"\x1b[99~",
        b"\x1b[1I",
        b"\x1b]0;title\x07",
        b"\x1bPdata\x1b\\",
    ] {
        let mut p = StdinParser::new();
        assert!(p.feed(input).is_empty(), "input {input:?}");
        assert!(is_ground(&p), "input {input:?}");
    }
}

#[test]
fn a_lone_esc_waits_for_flush_and_only_it_arms_the_timer() {
    let mut p = StdinParser::new();
    assert!(!p.esc_pending());
    assert!(p.feed(b"\x1b").is_empty(), "bare ESC must wait for flush");
    assert!(p.esc_pending());
    let flushed = p.flush();
    assert!(matches!(&flushed[..], [InputEvent::Key(k)] if k.key == PhysicalKey::Escape));
    assert!(is_ground(&p));

    // ESC ESC: the first is a complete Escape, the second still waits.
    let evs = p.feed(b"\x1b\x1b");
    assert!(matches!(&evs[..], [InputEvent::Key(k)] if k.key == PhysicalKey::Escape));
    assert!(p.esc_pending());
    // ... and a following byte turns it into an Alt chord.
    let evs = p.feed(b"x");
    assert!(matches!(&evs[..], [InputEvent::Key(k)] if k.mods == ModSet::ALT));

    // A partial CSI neither arms the timer nor yields to a flush.
    assert!(p.feed(b"\x1b[1;").is_empty());
    assert!(!p.esc_pending());
    assert!(p.flush().is_empty());
    assert_eq!(p.feed(b"5A").len(), 1);
}

/// The attach loop feeds one retained buffer: `feed_into` and `flush_into`
/// must append, or a batch loses every event before the last read.
#[test]
fn feed_into_and_flush_into_append() {
    let mut p = StdinParser::new();
    let mut out = Vec::new();
    p.feed_into(b"ab", &mut out);
    p.feed_into(b"c\x1b", &mut out);
    p.flush_into(&mut out);
    assert_eq!(out.len(), 4);
    assert!(matches!(&out[3], InputEvent::Key(k) if k.key == PhysicalKey::Escape));
}
