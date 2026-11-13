//! Tests for `surface_wayland`: the mapping and the bytes, since no
//! compositor runs here.

use phux_client::tree::DocumentKey;

use super::*;

const fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x, y, w, h }
}

/// A 4K panel at 2x (1920x1080 logical) left of a 1080p panel at 1x turned
/// upright (1080x1920), as `hyprctl -j monitors` places them.
const LAYOUT: Rect = rect(0.0, 0.0, 3000.0, 1920.0);

fn place(frame: Rect, window: Option<&str>) -> Place {
    Place {
        frame,
        window: window.map(str::to_owned),
        layout: LAYOUT,
    }
}

fn click(x: u32, y: u32, image: (u32, u32), place: &Place) -> Op {
    ops(&DocumentInput::Click { x, y }, image, Some(place)).unwrap()[0].clone()
}

const fn mv(x: u32, y: u32) -> Op {
    Op::Move {
        x,
        y,
        w: 3000,
        h: 1920,
    }
}

#[test]
fn a_click_moves_within_the_whole_layout_then_presses() {
    let display = place(LAYOUT, None);
    // `display` is grim's stitch at the highest scale: 6000x3840.
    let got = ops(
        &DocumentInput::Click { x: 4000, y: 200 },
        (6000, 3840),
        Some(&display),
    );
    assert_eq!(
        got,
        Ok(vec![mv(2000, 100), Op::Button(true), Op::Button(false)])
    );
    // The same under `--max-width 1500`.
    assert_eq!(click(1000, 50, (1500, 960), &display), mv(2000, 100));
    // `display:HDMI-A-1` alone, natively 1080x1920: its origin is added.
    let hdmi = place(rect(1920.0, 0.0, 1080.0, 1920.0), None);
    assert_eq!(click(10, 20, (1080, 1920), &hdmi), mv(1930, 20));
    // `display:DP-1`, captured at 2x then downscaled to 960 wide.
    let dp = place(rect(0.0, 0.0, 1920.0, 1080.0), None);
    assert_eq!(click(480, 270, (960, 540), &dp), mv(960, 540));
    // Past the edge clamps to the last image pixel.
    assert_eq!(click(9999, 9999, (960, 540), &dp), mv(1918, 1078));
    assert!(ops(&DocumentInput::Click { x: 1, y: 1 }, (10, 10), None).is_err());
}

#[test]
fn a_layout_left_of_zero_is_measured_from_its_corner() {
    let layout = rect(-1920.0, -200.0, 3840.0, 1280.0);
    let left = Place {
        frame: rect(-1920.0, -200.0, 1920.0, 1080.0),
        window: None,
        layout,
    };
    assert_eq!(
        click(0, 0, (1920, 1080), &left),
        Op::Move {
            x: 0,
            y: 0,
            w: 3840,
            h: 1280
        }
    );
}

#[test]
fn a_window_scroll_points_at_it_and_scales_the_delta() {
    let window = place(rect(12.0, 34.0, 800.0, 600.0), Some("0xa1"));
    let got = ops(
        &DocumentInput::Scroll { dx: 0, dy: 200 },
        (1600, 1200),
        Some(&window),
    );
    assert_eq!(
        got,
        Ok(vec![mv(412, 334), Op::Scroll { dx: 0.0, dy: 100.0 }])
    );
    let display = place(LAYOUT, None);
    let got = ops(
        &DocumentInput::Scroll { dx: -30, dy: 0 },
        (3000, 1920),
        Some(&display),
    );
    assert_eq!(got, Ok(vec![Op::Scroll { dx: -30.0, dy: 0.0 }]));
}

fn keymap_and_codes(op: &Op) -> (&str, &[u32]) {
    match op {
        Op::Keys {
            keymap,
            codes,
            mods: 0,
            held,
        } if held.is_empty() => (keymap, codes),
        other => panic!("not keys: {other:?}"),
    }
}

#[test]
fn text_types_through_a_keymap_of_its_own_characters() {
    let got = ops(
        &DocumentInput::Type {
            text: "aba\tÉ\u{7}\n".into(),
        },
        (1, 1),
        None,
    )
    .unwrap();
    assert_eq!(got.len(), 1);
    let (keymap, codes) = keymap_and_codes(&got[0]);
    // a b a Tab É Return; the bell has no key.
    assert_eq!(codes, [1, 2, 1, 3, 4, 5]);
    for line in [
        "maximum = 14;",
        "<K1> = 9;",
        "<K5> = 13;",
        "key <K1> {[U0061]};",
        "key <K2> {[U0062]};",
        "key <K3> {[Tab]};",
        "key <K4> {[U00C9]};",
        "key <K5> {[Return]};",
        "xkb_types \"phux\" { include \"complete\" };",
    ] {
        assert!(keymap.contains(line), "{line} missing from\n{keymap}");
    }
    assert!(
        ops(
            &DocumentInput::Type {
                text: String::new()
            },
            (1, 1),
            None
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn long_text_splits_where_the_keycodes_run_out() {
    let text: String = (0..250)
        .filter_map(|i| char::from_u32(0x4e00 + i))
        .collect();
    let got = ops(&DocumentInput::Type { text }, (1, 1), None).unwrap();
    assert_eq!(got.len(), 2);
    let (first, codes) = keymap_and_codes(&got[0]);
    assert_eq!(codes.len(), KEYMAP_KEYS);
    assert!(first.contains("maximum = 209;"));
    assert_eq!(keymap_and_codes(&got[1]).1, (1..=50).collect::<Vec<_>>());
}

#[test]
fn every_key_presses_its_named_keysym() {
    for (key, sym) in [
        (DocumentKey::Enter, "Return"),
        (DocumentKey::Tab, "Tab"),
        (DocumentKey::Backspace, "BackSpace"),
        (DocumentKey::Escape, "Escape"),
        (DocumentKey::ArrowLeft, "Left"),
        (DocumentKey::ArrowUp, "Up"),
        (DocumentKey::ArrowRight, "Right"),
        (DocumentKey::ArrowDown, "Down"),
    ] {
        let got = ops(&DocumentInput::Key { key }, (1, 1), None).unwrap();
        let (keymap, codes) = keymap_and_codes(&got[0]);
        assert_eq!(codes, [1]);
        assert!(keymap.contains(&format!("key <K1> {{[{sym}]}};")), "{sym}");
    }
}

#[test]
fn requests_are_little_endian_words_with_padded_strings() {
    // wl_registry.bind(7, "wl_seat", 1, 9)
    let bytes = message(2, 0, &[Arg::U(7), Arg::S("wl_seat"), Arg::U(1), Arg::U(9)]);
    let mut want = vec![2, 0, 0, 0, 0, 0, 32, 0, 7, 0, 0, 0, 8, 0, 0, 0];
    want.extend_from_slice(b"wl_seat\0");
    want.extend_from_slice(&[1, 0, 0, 0, 9, 0, 0, 0]);
    assert_eq!(bytes, want);
    // axis(time 5, vertical, -1.5): 24.8 fixed point.
    let bytes = message(4, 3, &[Arg::U(5), Arg::U(0), Arg::Fixed(-1.5)]);
    assert_eq!(&bytes[4..8], &[3, 0, 20, 0]);
    assert_eq!(&bytes[16..20], &(-384i32).to_le_bytes());
}

#[test]
fn a_drag_presses_at_the_start_moves_through_the_path_and_releases() {
    let display = place(LAYOUT, None);
    let got = ops(
        &DocumentInput::Drag {
            from: (0, 0),
            to: (1600, 800),
        },
        (3000, 1920),
        Some(&display),
    )
    .unwrap();
    assert_eq!(got.len(), 11);
    assert_eq!(got[..2], [mv(0, 0), Op::Button(true)]);
    assert_eq!(got[2], mv(200, 100));
    assert_eq!(got[9..], [mv(1600, 800), Op::Button(false)]);
    let drag = DocumentInput::Drag {
        from: (0, 0),
        to: (1, 1),
    };
    assert!(ops(&drag, (10, 10), None).is_err());
}

#[test]
fn a_chord_holds_control_and_shift_for_a_terminal_on_a_real_key() {
    for (letter, shift, mask, code) in [
        ('v', false, 4, 47),
        ('v', true, 5, 47),
        ('c', true, 5, 46),
        ('a', false, 4, 30),
    ] {
        let Op::Keys {
            keymap,
            codes,
            mods,
            held,
        } = chord(letter, shift)
        else {
            panic!("not keys");
        };
        assert_eq!((codes, mods), (vec![code], mask));
        // Left Control (and Left Shift) go down first, as real keys.
        assert_eq!(held, if shift { vec![29, 42] } else { vec![29] });
        let upper = letter.to_ascii_uppercase();
        let xkb = code + 8;
        for line in [
            format!("maximum = {};", xkb.max(50)),
            format!("<K> = {xkb};"),
            format!("key <K> {{[{letter}, {upper}]}};"),
            "<LCTL> = 37;".to_owned(),
            "<LFSH> = 50;".to_owned(),
            "modifier_map Control { <LCTL> };".to_owned(),
            "modifier_map Shift { <LFSH> };".to_owned(),
        ] {
            assert!(keymap.contains(&line), "{line} missing from\n{keymap}");
        }
    }
    // zwp_virtual_keyboard_v1.modifiers(depressed 4, 0, 0, 0)
    let bytes = message(9, 2, &[Arg::U(4), Arg::U(0), Arg::U(0), Arg::U(0)]);
    assert_eq!(&bytes[..8], &[9, 0, 0, 0, 2, 0, 24, 0]);
    assert_eq!(&bytes[8..12], &4u32.to_le_bytes());
}
