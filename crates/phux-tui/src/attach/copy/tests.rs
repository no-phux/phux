use super::*;
use libghostty_vt::{Terminal as GhosttyTerminal, terminal::ScrollViewport};

fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
    {
        let mut terminal = GhosttyTerminal::new(cols, rows).expect("Terminal::new");
        terminal
            .set_scrollback_max_lines(Some(100))
            .expect("Terminal::new");
        terminal
    }
}

/// A two-corner [`SelectionGrab::Rect`] request over the inclusive
/// rectangle `(start_row,start_col)..=(end_row,end_col)`, linear.
fn rect_req(start_row: u16, start_col: u16, end_row: u16, end_col: u16) -> CopyRequest {
    CopyRequest {
        start_row,
        start_col,
        end_row,
        end_col,
        mouse_anchor_screen: None,
        rectangle: false,
        cursor_row: end_row,
        cursor_col: end_col,
        grab: SelectionGrab::Rect,
    }
}

/// An engine-derived request resolving at cursor `(row, col)` with `grab`.
/// The two-corner rectangle is collapsed onto the cursor (unused by the
/// engine-derived path).
fn grab_req(grab: SelectionGrab, row: u16, col: u16) -> CopyRequest {
    CopyRequest {
        start_row: row,
        start_col: col,
        end_row: row,
        end_col: col,
        mouse_anchor_screen: None,
        rectangle: false,
        cursor_row: row,
        cursor_col: col,
        grab,
    }
}

/// RFC 4648 §10 test vectors.
#[test]
fn base64_rfc4648_vectors() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
}

#[test]
fn base64_encodes_high_bytes() {
    // 0xFB 0xFF 0xBF exercises all 1-bits across the chunk boundaries.
    assert_eq!(base64_encode(&[0xFB, 0xFF, 0xBF]), "+/+/");
}

#[test]
fn osc52_wraps_base64_in_set_clipboard() {
    let seq = osc52_set_clipboard("foo");
    assert_eq!(seq, b"\x1b]52;c;Zm9v\x07");
}

#[test]
fn extract_single_word_from_viewport() {
    let mut t = fresh(20, 3);
    t.vt_write(b"hello world");
    // "hello" occupies viewport row 0, cols 0..=4 (inclusive).
    let req = rect_req(0, 0, 0, 4);
    assert_eq!(extract_selection_text(&t, req).as_deref(), Some("hello"));
}

#[test]
fn mouse_drag_copy_keeps_its_first_cell_after_scrolling() {
    let mut t = fresh(12, 3);
    for n in 0..8 {
        let suffix = if n == 7 { "" } else { "\r\n" };
        t.vt_write(format!("line{n:02}{suffix}").as_bytes());
    }

    // The first click lands on an older visible row, then the user
    // scrolls back to the live end before releasing on line07.
    t.scroll_viewport(ScrollViewport::Delta(-3));
    let first = t
        .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: 0 }))
        .expect("first visible cell");
    let anchor = t
        .point_from_grid_ref(&first, PointSpace::Screen)
        .expect("screen conversion")
        .expect("screen point");
    t.scroll_viewport(ScrollViewport::Bottom);

    let mut req = rect_req(0, 0, 2, 5);
    req.mouse_anchor_screen = Some(ScreenSelectionPoint {
        col: anchor.x,
        row: anchor.y,
    });
    assert_eq!(
        extract_selection_text(&t, req).as_deref(),
        Some("line02\nline03\nline04\nline05\nline06\nline07"),
        "copy must span the original press point, not the same viewport row after scrolling"
    );
}

/// A two-corner request that is rectangular (block) rather than linear.
fn block_req(start_row: u16, start_col: u16, end_row: u16, end_col: u16) -> CopyRequest {
    CopyRequest {
        rectangle: true,
        ..rect_req(start_row, start_col, end_row, end_col)
    }
}

#[test]
fn extract_block_vs_linear_disagree_on_the_column_band() {
    let mut t = fresh(20, 3);
    // Row 0: "abcd", row 1: "efgh".
    t.vt_write(b"abcd\r\nefgh");
    // Block keeps columns 1..=2 per row; linear wraps and also takes 'd'
    // and 'e'.
    let block = extract_selection_text(&t, block_req(0, 1, 1, 2)).expect("block text");
    assert!(block.contains('b') && block.contains('c'), "got {block:?}");
    assert!(block.contains('f') && block.contains('g'), "got {block:?}");
    assert!(
        !block.contains('d'),
        "block excludes the wrap col: {block:?}"
    );
    assert!(
        !block.contains('e'),
        "block excludes the wrap col: {block:?}"
    );
    assert!(
        !block.contains('a') && !block.contains('h'),
        "got {block:?}"
    );

    let linear = extract_selection_text(&t, rect_req(0, 1, 1, 2)).expect("linear text");
    assert!(
        linear.contains('d'),
        "linear spans to the row end: {linear:?}"
    );
    assert!(linear.contains('e'), "linear wraps onto row 1: {linear:?}");
}

#[test]
fn extract_block_normalizes_inverted_column_corners() {
    // An inverted-column block drag (start_col 5 > end_col 2) must copy
    // exactly the [2, 5] band the highlight shows.
    let mut t = fresh(20, 4);
    t.vt_write(b"0123456789\r\nabcdefghij\r\nABCDEFGHIJ\r\nqrstuvwxyz");
    let text = extract_selection_text(&t, block_req(0, 5, 3, 2)).expect("inverted block text");
    assert_eq!(text, "2345\ncdef\nCDEF\nstuv");
}

#[test]
fn extract_spanning_two_rows_linear() {
    let mut t = fresh(20, 3);
    // Row 0: "abc", row 1: "def" (explicit CR/LF placement).
    t.vt_write(b"abc\r\ndef");
    let req = rect_req(0, 0, 1, 2);
    let text = extract_selection_text(&t, req).expect("some text");
    assert!(text.contains("abc"), "got {text:?}");
    assert!(text.contains("def"), "got {text:?}");
}

#[test]
fn copy_to_host_clipboard_emits_osc52() {
    let mut t = fresh(20, 3);
    t.vt_write(b"hi");
    let mut out: Vec<u8> = Vec::new();
    let req = rect_req(0, 0, 0, 1);
    copy_to_host_clipboard(&mut out, &t, req).expect("write");
    // "hi" -> base64 "aGk=" wrapped in OSC 52.
    assert_eq!(out, b"\x1b]52;c;aGk=\x07");
}

#[test]
fn copy_to_host_clipboard_blank_span_writes_nothing() {
    let t = fresh(20, 3); // no output: viewport is all blanks
    let mut out: Vec<u8> = Vec::new();
    let req = rect_req(1, 0, 1, 5);
    copy_to_host_clipboard(&mut out, &t, req).expect("write");
    assert!(
        out.is_empty(),
        "blank selection should emit nothing, got {out:?}"
    );
}

#[test]
fn grab_word_extracts_word_under_cursor() {
    let mut t = fresh(20, 3);
    t.vt_write(b"hello world");
    // Cursor inside "world" (col 8, row 0) -> select_word yields "world".
    let req = grab_req(SelectionGrab::Word, 0, 8);
    assert_eq!(extract_selection_text(&t, req).as_deref(), Some("world"));
}

#[test]
fn grab_line_extracts_whole_line() {
    let mut t = fresh(20, 3);
    t.vt_write(b"alpha beta\r\ngamma");
    // Cursor anywhere on row 0 -> the whole first line.
    let req = grab_req(SelectionGrab::Line, 0, 2);
    let text = extract_selection_text(&t, req).expect("line text");
    assert!(text.contains("alpha"), "got {text:?}");
    assert!(text.contains("beta"), "got {text:?}");
    assert!(!text.contains("gamma"), "must not bleed row 1: {text:?}");
}

#[test]
fn grab_all_extracts_all_content() {
    let mut t = fresh(20, 3);
    t.vt_write(b"first\r\nsecond");
    // select_all ignores the cursor; both rows are captured.
    let req = grab_req(SelectionGrab::All, 0, 0);
    let text = extract_selection_text(&t, req).expect("all text");
    assert!(text.contains("first"), "got {text:?}");
    assert!(text.contains("second"), "got {text:?}");
}

#[test]
fn grab_line_semantic_bounds_at_prompt() {
    let mut t = fresh(40, 3);
    // OSC-133 ; A -> prompt start, then prompt text + typed input on row 0.
    t.vt_write(b"\x1b]133;A\x07$ ");
    t.vt_write(b"\x1b]133;B\x07ls -la");
    // Semantic-line select at the cursor: derives a selection bounded by
    // the OSC-133 prompt-state changes rather than the raw display line.
    let req = grab_req(SelectionGrab::LineSemantic, 0, 4);
    let text = extract_selection_text(&t, req).expect("semantic line text");
    assert!(text.contains("ls -la"), "got {text:?}");
}

#[test]
fn grab_output_extracts_command_output_zone() {
    let mut t = fresh(40, 4);
    // Prompt + input (row 0), then command output marked by OSC-133 ; C.
    t.vt_write(b"\x1b]133;A\x07$ ");
    t.vt_write(b"\x1b]133;B\x07cat f\r\n");
    t.vt_write(b"\x1b]133;C\x07the-output-line\r\n");
    // Cursor on the output row -> select_output captures the output span.
    let req = grab_req(SelectionGrab::Output, 1, 3);
    let text = extract_selection_text(&t, req).expect("output text");
    assert!(text.contains("the-output-line"), "got {text:?}");
}

#[test]
fn grab_output_without_zones_is_noop() {
    let mut t = fresh(20, 3);
    // No OSC-133 marks: select_output has no zone to resolve -> None.
    t.vt_write(b"plain text");
    let req = grab_req(SelectionGrab::Output, 0, 2);
    assert_eq!(extract_selection_text(&t, req), None);
}

/// Search picks the next hit after the cursor, or the previous one, and
/// wraps at either end (vi, tmux).
#[test]
fn pick_match_steps_and_wraps() {
    let hit = |row, col| SearchMatch {
        start_row: row,
        start_col: col,
        end_row: row,
        end_col: col + 1,
    };
    let hits = [hit(2, 0), hit(5, 3), hit(5, 9)];
    assert_eq!(pick_match(&hits, (5, 3), false), Some(2));
    assert_eq!(pick_match(&hits, (5, 3), true), Some(0));
    assert_eq!(pick_match(&hits, (5, 9), false), Some(0), "wraps forward");
    assert_eq!(pick_match(&hits, (2, 0), true), Some(2), "wraps backward");
    assert_eq!(pick_match(&[], (0, 0), false), None);
    assert_eq!(pick_match(&[], (0, 0), true), None);
}

/// `viewport_top` counts in the same history space the document search
/// reports hits in: the live bottom sits past the scrollback, and
/// scrolling up moves it.
#[test]
fn viewport_top_tracks_the_scrolled_viewport() {
    let mut terminal = fresh(10, 4);
    for n in 0..20 {
        terminal.vt_write(format!("row{n:02}\r\n").as_bytes());
    }
    assert_eq!(viewport_top(&terminal), Some(17));
    terminal.scroll_viewport(ScrollViewport::Delta(-5));
    assert_eq!(viewport_top(&terminal), Some(12));
    terminal.scroll_viewport(ScrollViewport::Top);
    assert_eq!(viewport_top(&terminal), Some(0));
}

/// A keyboard selection anchored low and extended upward through history:
/// the anchor is the later corner and is no longer in the viewport.
#[test]
fn pinned_anchor_below_a_scrolled_cursor_copies_the_span_between() {
    let mut t = fresh(12, 3);
    for n in 0..8 {
        let suffix = if n == 7 { "" } else { "\r\n" };
        t.vt_write(format!("line{n:02}{suffix}").as_bytes());
    }
    // Anchor on the end of line06 (viewport row 1 of the live screen).
    let top = viewport_screen_top(&t).expect("screen top");
    let anchor = ScreenSelectionPoint {
        col: 5,
        row: top + 1,
    };
    // `g`'s scroll reaches the oldest line without overflowing.
    t.scroll_viewport(ScrollViewport::Delta(-(i32::MAX as isize)));
    assert_eq!(viewport_screen_top(&t), Some(0));

    let mut req = rect_req(0, 0, 0, 0);
    req.mouse_anchor_screen = Some(anchor);
    assert_eq!(
        extract_selection_text(&t, req).as_deref(),
        Some("line00\nline01\nline02\nline03\nline04\nline05\nline06"),
    );
}

#[test]
fn viewport_row_cells_reads_one_codepoint_per_cell() {
    let mut t = fresh(8, 2);
    t.vt_write(b"ab c");
    assert_eq!(
        viewport_row_cells(&t, 0, 6),
        [
            u32::from('a'),
            u32::from('b'),
            u32::from(' '),
            u32::from('c'),
            0,
            0
        ]
    );
}
