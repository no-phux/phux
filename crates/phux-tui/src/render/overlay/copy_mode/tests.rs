use phux_protocol::input::key::{KeyAction, ModSet};

use super::*;

fn press(key: PhysicalKey, mods: ModSet) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

fn mouse(action: MouseAction, button: MouseButton, x: f64, y: f64) -> MouseEvent {
    MouseEvent {
        action,
        button,
        x,
        y,
        mods: ModSet::empty(),
    }
}

fn wheel(button: MouseButton) -> MouseEvent {
    mouse(MouseAction::Press, button, 0.0, 0.0)
}

fn corners(overlay: &CopyModeOverlay) -> (u16, u16, u16, u16) {
    let sel = overlay
        .copy_selection()
        .expect("copy-mode always has a selection");
    (sel.start_row, sel.start_col, sel.end_row, sel.end_col)
}

#[test]
fn keys_map_to_grabs_scrolls_and_exits() {
    let none = ModSet::empty();
    for (key, mods, expected) in [
        (PhysicalKey::Y, none, Some(SelectionGrab::Word)),
        (
            PhysicalKey::Y,
            ModSet::SHIFT,
            Some(SelectionGrab::LineSemantic),
        ),
        (PhysicalKey::A, ModSet::SHIFT, Some(SelectionGrab::All)),
        (PhysicalKey::BracketRight, none, Some(SelectionGrab::Output)),
        (PhysicalKey::Enter, none, Some(SelectionGrab::Rect)),
        (PhysicalKey::A, none, None),
    ] {
        // Cursor at (2, 5) so engine-derived grabs carry it.
        let cmd = CopyModeOverlay::new(2, 5, 80, 24).handle_key(&press(key, mods));
        match (cmd, expected) {
            (OverlayCommand::Copy(req), Some(grab)) => {
                assert_eq!(req.grab, grab, "{key:?} {mods:?}");
                assert_eq!((req.cursor_row, req.cursor_col), (2, 5));
            }
            (OverlayCommand::Stay, None) => {}
            (other, _) => panic!("{key:?} {mods:?}: {other:?}"),
        }
    }
    let mut overlay = CopyModeOverlay::new(2, 5, 80, 24);
    let key = |overlay: &mut CopyModeOverlay, k| overlay.handle_key(&press(k, none));
    assert_eq!(
        key(&mut overlay, PhysicalKey::PageUp),
        OverlayCommand::ScrollViewport(-23)
    );
    assert_eq!(
        key(&mut overlay, PhysicalKey::PageDown),
        OverlayCommand::ScrollViewport(23)
    );
    assert_eq!(
        key(&mut overlay, PhysicalKey::Escape),
        OverlayCommand::Dismiss
    );
    // Arrows at the edges scroll the viewport instead of moving.
    let mut top = CopyModeOverlay::new(0, 5, 80, 24);
    assert_eq!(
        key(&mut top, PhysicalKey::ArrowUp),
        OverlayCommand::ScrollViewport(-1)
    );
    let mut bottom = CopyModeOverlay::new(23, 5, 80, 24);
    assert_eq!(
        key(&mut bottom, PhysicalKey::ArrowDown),
        OverlayCommand::ScrollViewport(1)
    );
    assert_eq!(bottom.cursor_row, 23);
}

/// Tab cycles Char -> Line -> Rect -> Char; Rect requests block
/// extraction, Char and Line linear, and Line spans whole rows on both
/// the highlight and the copy request (ADR-0045).
#[test]
fn tab_cycles_modes_and_each_mode_shapes_the_selection() {
    let mut overlay = CopyModeOverlay::new(1, 3, 80, 24);
    overlay.move_cursor(1, 2);
    assert_eq!(corners(&overlay), (1, 3, 2, 5));
    assert!(!overlay.copy_request().rectangle, "Char is linear");

    let tab = press(PhysicalKey::Tab, ModSet::empty());
    assert_eq!(overlay.handle_key(&tab), OverlayCommand::Stay);
    assert_eq!(overlay.mode, SelectionMode::Line);
    assert_eq!(corners(&overlay), (1, 0, 2, 79));
    let req = overlay.copy_request();
    assert_eq!(
        (req.start_row, req.start_col, req.end_row, req.end_col),
        (1, 0, 2, 79)
    );
    assert!(!req.rectangle, "Line is linear");

    overlay.handle_key(&tab);
    assert_eq!(overlay.mode, SelectionMode::Rect);
    let req = overlay.copy_request();
    assert!(req.rectangle && req.grab == SelectionGrab::Rect);
    overlay.handle_key(&tab);
    assert_eq!(overlay.mode, SelectionMode::Char, "wraps");
}

#[test]
fn arrows_move_the_cursor_and_shift_extends() {
    let overlay = CopyModeOverlay::new(100, 100, 80, 24);
    assert_eq!(
        (overlay.cursor_row, overlay.cursor_col),
        (23, 79),
        "clamped"
    );
    assert_eq!(
        corners(&CopyModeOverlay::new(5, 10, 80, 24)),
        (5, 10, 5, 10)
    );

    let mut overlay = CopyModeOverlay::new(2, 3, 80, 24);
    overlay.handle_key(&press(PhysicalKey::ArrowRight, ModSet::empty()));
    assert_eq!(corners(&overlay), (2, 4, 2, 4), "plain arrows move");
    overlay.handle_key(&press(PhysicalKey::ArrowRight, ModSet::SHIFT));
    assert_eq!(corners(&overlay), (2, 4, 2, 5), "shift extends");
    // A backwards range normalizes.
    let range = CellRange::from_points(5, 10, 2, 3);
    assert_eq!(
        (
            range.start_row,
            range.start_col,
            range.end_row,
            range.end_col
        ),
        (2, 3, 5, 10)
    );
}

#[test]
fn mouse_drags_select_and_copy_while_a_click_exits() {
    let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
    assert_eq!(
        overlay.handle_mouse(&wheel(MouseButton::Four)),
        OverlayCommand::ScrollViewport(-WHEEL_SCROLL_LINES)
    );
    assert_eq!(
        overlay.handle_mouse(&wheel(MouseButton::Five)),
        OverlayCommand::ScrollViewport(WHEEL_SCROLL_LINES)
    );

    let anchor = ScreenSelectionPoint { col: 4, row: 123 };
    overlay.set_mouse_anchor_screen(anchor);
    overlay.handle_mouse(&mouse(MouseAction::Press, MouseButton::Left, 4.0, 2.0));
    overlay.handle_mouse(&mouse(MouseAction::Motion, MouseButton::Left, 8.0, 3.0));
    assert_eq!(corners(&overlay), (2, 4, 3, 8));
    let OverlayCommand::Copy(req) =
        overlay.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 8.0, 3.0))
    else {
        panic!("a dragged release copies");
    };
    assert_eq!(req.grab, SelectionGrab::Rect);
    assert_eq!(
        req.mouse_anchor_screen,
        Some(anchor),
        "the stable press point"
    );

    // A click without a drag must exit rather than trap the keyboard.
    let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
    overlay.handle_mouse(&mouse(MouseAction::Press, MouseButton::Left, 4.0, 2.0));
    assert_eq!(
        overlay.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 4.0, 2.0)),
        OverlayCommand::Dismiss
    );
}

#[test]
fn repeat_presses_on_one_cell_count_to_three_then_start_over() {
    let pane = ResourceId::local(1);
    let t0 = Instant::now();
    let mut last = None;
    let counts: Vec<u8> = (0..4)
        .map(|i| ClickRecord::press(&mut last, &pane, 5, 2, t0 + Duration::from_millis(100 * i)))
        .collect();
    assert_eq!(counts, [1, 2, 3, 1]);
}

#[test]
fn a_slow_moved_or_other_pane_press_is_a_single_click() {
    let pane = ResourceId::local(1);
    let at = Instant::now();
    let first = || {
        let mut last = None;
        ClickRecord::press(&mut last, &pane, 5, 2, at);
        last
    };
    let slow = at + REPEAT_CLICK + Duration::from_millis(1);
    assert_eq!(
        ClickRecord::press(&mut first(), &pane, 5, 2, slow),
        1,
        "too slow"
    );
    assert_eq!(
        ClickRecord::press(&mut first(), &pane, 6, 2, at),
        1,
        "another cell"
    );
    let other = ResourceId::local(2);
    assert_eq!(
        ClickRecord::press(&mut first(), &other, 5, 2, at),
        1,
        "another pane"
    );
}

/// A repeated click's release copies the dispatcher's word or line grab
/// at the pressed cell; dragging after it still copies the drag.
#[test]
fn a_repeated_click_copies_its_grab_at_the_pressed_cell() {
    let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
    overlay.set_click_grab(Some(SelectionGrab::Word));
    overlay.handle_mouse(&mouse(MouseAction::Press, MouseButton::Left, 6.0, 3.0));
    let OverlayCommand::Copy(req) =
        overlay.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 6.0, 3.0))
    else {
        panic!("a double-click copies its word");
    };
    assert_eq!(
        (req.grab, req.cursor_row, req.cursor_col),
        (SelectionGrab::Word, 3, 6)
    );

    let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
    overlay.set_click_grab(Some(SelectionGrab::Line));
    overlay.handle_mouse(&mouse(MouseAction::Press, MouseButton::Left, 6.0, 3.0));
    overlay.handle_mouse(&mouse(MouseAction::Motion, MouseButton::Left, 9.0, 3.0));
    let OverlayCommand::Copy(req) =
        overlay.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 9.0, 3.0))
    else {
        panic!("a drag copies");
    };
    assert_eq!(
        req.grab,
        SelectionGrab::Rect,
        "the drag wins over the click grab"
    );
}

/// While a drag wheel-scrolls, the highlight stays on the clicked cell
/// (it moves down as older rows appear); the copy still resolves the
/// full-screen press point.
#[test]
fn mouse_drag_highlight_stays_with_the_clicked_cell_while_scrolling() {
    let mut overlay = CopyModeOverlay::new(6, 5, 80, 24);
    overlay.set_mouse_anchor_screen(ScreenSelectionPoint { col: 5, row: 106 });
    overlay.handle_mouse(&mouse(MouseAction::Press, MouseButton::Left, 5.0, 6.0));
    overlay.handle_mouse(&wheel(MouseButton::Four));
    let (start_row, _, end_row, _) = corners(&overlay);
    assert_eq!((start_row, end_row), (6, 9));
    assert_eq!(
        overlay.copy_request().mouse_anchor_screen.map(|p| p.row),
        Some(106)
    );
}

/// Clamps, Line mode's right edge, and the page span all follow a resize:
/// a stale-large corner makes the copy resolve to nothing, a stale-small
/// one leaves new cells unreachable. An in-bounds selection survives.
#[test]
fn a_resize_reclamps_and_rederives_pane_geometry() {
    let mut overlay = CopyModeOverlay::new(20, 70, 80, 24);
    overlay.move_cursor(3, 9);
    overlay.on_viewport_resize(60, 18);
    assert_eq!((overlay.cursor_row, overlay.cursor_col), (17, 59));
    assert_eq!((overlay.anchor_row, overlay.anchor_col), (17, 59));

    let mut overlay = CopyModeOverlay::new(0, 0, 60, 20);
    overlay.on_viewport_resize(100, 30);
    overlay.move_cursor(i16::MAX, i16::MAX);
    assert_eq!((overlay.cursor_row, overlay.cursor_col), (29, 99));

    let mut overlay = CopyModeOverlay::new(1, 0, 60, 20);
    overlay.cycle_mode();
    assert_eq!(overlay.copy_request().end_col, 59);
    overlay.on_viewport_resize(100, 30);
    assert_eq!(overlay.copy_request().end_col, 99);
    assert_eq!(overlay.page_scroll_delta(), 29);

    let mut overlay = CopyModeOverlay::new(2, 3, 60, 20);
    overlay.move_cursor(1, 2);
    overlay.on_viewport_resize(100, 30);
    assert_eq!(corners(&overlay), (2, 3, 3, 5));
}

fn typed(overlay: &mut CopyModeOverlay, text: &str) -> OverlayCommand {
    let mut last = OverlayCommand::Stay;
    for ch in text.chars() {
        last = overlay.handle_key(&KeyEvent {
            text: Some(ch.to_string()),
            ..press(PhysicalKey::A, ModSet::empty())
        });
    }
    last
}

fn found(start: (u32, u16), end: (u32, u16)) -> SearchMatch {
    SearchMatch {
        start_row: start.0,
        start_col: start.1,
        end_row: end.0,
        end_col: end.1,
    }
}

/// `/` and `?` open a search line that edits like a prompt; Enter asks the
/// dispatcher to search from the cursor, Esc closes only the line.
#[test]
fn slash_and_question_mark_type_a_search_and_enter_runs_it() {
    let none = ModSet::empty();
    let mut overlay = CopyModeOverlay::new(3, 4, 80, 24);
    assert_eq!(typed(&mut overlay, "/fop"), OverlayCommand::Stay);
    overlay.handle_key(&press(PhysicalKey::Backspace, none));
    overlay.handle_paste("o\n");
    assert_eq!(overlay.search_status().as_deref(), Some("/foo_"));
    assert_eq!(
        overlay.handle_key(&press(PhysicalKey::Enter, none)),
        OverlayCommand::Search(CopySearchRequest {
            needle: "foo".to_owned(),
            backward: false,
            cursor_row: 3,
            cursor_col: 4,
            pane_rows: 24,
        })
    );

    typed(&mut overlay, "?bar");
    assert_eq!(overlay.search_status().as_deref(), Some("?bar_"));
    assert_eq!(
        overlay.handle_key(&press(PhysicalKey::Escape, none)),
        OverlayCommand::Stay
    );
    assert_eq!(overlay.search_status().as_deref(), Some("/foo: no match"));
    assert_eq!(
        overlay.handle_key(&press(PhysicalKey::Escape, none)),
        OverlayCommand::Dismiss,
        "a second Esc leaves copy-mode"
    );
}

/// A result selects the current hit (so Enter copies it); `n` repeats in
/// the search's direction from there and `N` reverses it.
#[test]
fn a_search_result_selects_the_hit_and_n_repeats_it() {
    let none = ModSet::empty();
    let mut overlay = CopyModeOverlay::new(0, 0, 80, 24);
    assert_eq!(
        overlay.handle_key(&press(PhysicalKey::N, none)),
        OverlayCommand::Stay
    );
    typed(&mut overlay, "?ab");
    let OverlayCommand::Search(req) = overlay.handle_key(&press(PhysicalKey::Enter, none)) else {
        panic!("expected a search");
    };
    assert!(req.backward);
    overlay.apply_search_result(CopySearchResult::Found {
        matches: vec![found((100, 2), (100, 3)), found((107, 70), (108, 1))],
        current: 1,
        top: 100,
    });
    assert_eq!(corners(&overlay), (7, 70, 8, 1));
    assert_eq!((overlay.cursor_row, overlay.cursor_col), (7, 70));
    assert_eq!(overlay.search_status().as_deref(), Some("?ab 2/2"));
    let view = overlay.search_view().expect("searching");
    assert_eq!((view.matches.len(), view.current), (2, Some(1)));

    for (mods, backward) in [(none, true), (ModSet::SHIFT, false)] {
        assert_eq!(
            overlay.handle_key(&press(PhysicalKey::N, mods)),
            OverlayCommand::Search(CopySearchRequest {
                needle: "ab".to_owned(),
                backward,
                cursor_row: 7,
                cursor_col: 70,
                pane_rows: 24,
            }),
            "{mods:?}"
        );
    }
    // An empty search line repeats the last search.
    typed(&mut overlay, "/");
    assert!(matches!(
        overlay.handle_key(&press(PhysicalKey::Enter, none)),
        OverlayCommand::Search(CopySearchRequest { ref needle, backward: true, .. })
            if needle == "ab"
    ));
    overlay.apply_search_result(CopySearchResult::NotFound);
    assert_eq!(overlay.search_status().as_deref(), Some("?ab: no match"));
}
