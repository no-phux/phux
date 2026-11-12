//! Tests for `surface_input`.

use super::*;

const DISPLAY: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 1512.0,
    h: 982.0,
};

#[test]
fn a_display_at_2x_halves_the_frame_pixels() {
    assert_eq!(
        to_screen((3024, 1964), DISPLAY, 100, 50),
        Some((50.0, 25.0))
    );
    assert_eq!(to_screen((0, 0), DISPLAY, 1, 1), None);
}

#[test]
fn a_window_adds_its_origin_and_scales_its_pixels() {
    let window = Rect {
        x: 300.0,
        y: 102.0,
        w: 1320.0,
        h: 840.0,
    };
    // A Retina capture: 2640x1680 pixels of a 1320x840 point window.
    assert_eq!(
        to_screen((2640, 1680), window, 200, 100),
        Some((400.0, 152.0))
    );
    // `--max-width 660` on it: each frame pixel is two points.
    assert_eq!(to_screen((660, 420), window, 100, 50), Some((500.0, 202.0)));
}

#[test]
fn a_point_outside_the_frame_clamps_to_its_last_pixel() {
    let (x, y) = to_screen((1512, 982), DISPLAY, 5000, 5000).unwrap();
    assert_eq!((x, y), (1511.0, 981.0));
}

#[test]
fn a_click_is_move_down_up_and_a_scroll_turns_under_the_pointer() {
    let click = events(
        &DocumentInput::Click { x: 100, y: 50 },
        (3024, 1964),
        DISPLAY,
    );
    assert_eq!(
        click,
        [
            Event::Move(50.0, 25.0),
            Event::LeftDown(50.0, 25.0),
            Event::LeftUp(50.0, 25.0)
        ]
    );
    let scroll = events(
        &DocumentInput::Scroll { dx: 0, dy: 40 },
        (3024, 1964),
        DISPLAY,
    );
    assert_eq!(
        scroll,
        [Event::Move(756.0, 491.0), Event::Scroll { dy: -20, dx: 0 }]
    );
    let nav = DocumentInput::Navigate {
        url: "https://phux.sh/".into(),
    };
    assert!(events(&nav, (1, 1), DISPLAY).is_empty());
    let paste = |chord| DocumentInput::Paste {
        text: "x".into(),
        chord,
    };
    assert_eq!(events(&paste(true), (1, 1), DISPLAY), [Event::Chord('v')]);
    let selection = |cut| DocumentInput::CopySelection { reply: 1, cut };
    assert_eq!(
        events(&selection(false), (1, 1), DISPLAY),
        [Event::Chord('c')]
    );
    assert_eq!(
        events(&selection(true), (1, 1), DISPLAY),
        [Event::Chord('x')]
    );
    let all = events(&DocumentInput::SelectAll, (1, 1), DISPLAY);
    assert_eq!(all, [Event::Chord('a')]);
    assert_eq!(all[0].to_string(), "Cmd+A");
    // A Linux terminal copies on Ctrl+Shift+C, cannot cut, and has no select-all.
    assert_eq!(chord_letter(&selection(true), true), Some('c'));
    assert_eq!(chord_letter(&DocumentInput::SelectAll, true), None);
    assert!(events(&paste(false), (1, 1), DISPLAY).is_empty());
    let copy = DocumentInput::CopyClipboard { reply: 1 };
    assert!(events(&copy, (1, 1), DISPLAY).is_empty());
}

#[test]
fn a_drag_presses_moves_through_the_path_and_releases_at_the_end() {
    assert_eq!(drag_path((0, 0), (80, 8)).len(), 9);
    assert_eq!(drag_path((80, 8), (0, 0))[4], (40, 4));
    let drag = events(
        &DocumentInput::Drag {
            from: (100, 50),
            to: (200, 50),
        },
        (3024, 1964),
        DISPLAY,
    );
    assert_eq!(
        drag[..2],
        [Event::Move(50.0, 25.0), Event::LeftDown(50.0, 25.0)]
    );
    assert_eq!(drag[2], Event::LeftDragged(56.0, 25.0));
    assert_eq!(drag[9], Event::LeftDragged(100.0, 25.0));
    assert_eq!(drag[10], Event::LeftUp(100.0, 25.0));
    assert_eq!(drag.len(), 11);
}

#[test]
fn text_goes_in_chunks_a_keyboard_event_can_carry() {
    let long = "a".repeat(45);
    let chunks = text_chunks(&long);
    assert_eq!(
        chunks.iter().map(String::len).collect::<Vec<_>>(),
        [20, 20, 5]
    );
    // An emoji is two UTF-16 units and is never split.
    let emoji = format!("{}😀", "b".repeat(19));
    assert_eq!(text_chunks(&emoji), ["b".repeat(19), "😀".to_owned()]);
    assert!(text_chunks("").is_empty());
}

#[test]
fn every_key_has_its_virtual_key_code() {
    let mut unique: Vec<u16> = DocumentKey::ALL.iter().map(|key| key_code(*key)).collect();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), DocumentKey::ALL.len());
    #[cfg(target_os = "macos")]
    {
        use core_graphics::event::KeyCode;
        assert_eq!(key_code(DocumentKey::Enter), KeyCode::RETURN);
        assert_eq!(key_code(DocumentKey::Tab), KeyCode::TAB);
        assert_eq!(key_code(DocumentKey::Backspace), KeyCode::DELETE);
        assert_eq!(key_code(DocumentKey::Escape), KeyCode::ESCAPE);
        assert_eq!(key_code(DocumentKey::ArrowLeft), KeyCode::LEFT_ARROW);
        assert_eq!(key_code(DocumentKey::ArrowRight), KeyCode::RIGHT_ARROW);
        assert_eq!(key_code(DocumentKey::ArrowDown), KeyCode::DOWN_ARROW);
        assert_eq!(key_code(DocumentKey::ArrowUp), KeyCode::UP_ARROW);
    }
}

#[test]
fn the_dry_run_frame_is_a_160x100_png() {
    assert_eq!(phux_client::tree::png_size(DRY_RUN_FRAME), Some((160, 100)));
}

#[test]
fn window_info_reads_bounds_and_owner_or_nothing() {
    let (rect, pid) = window_info(r#"{"x":300,"y":102,"w":1320,"h":840,"pid":93904}"#).unwrap();
    assert_eq!((rect.x, rect.w, pid), (300.0, 1320.0, 93904));
    assert_eq!(window_info(""), None);
}
