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

/// (case, screen size, bytes written, request, must contain, must not contain).
type ExtractCase<'a> = (
    &'a str,
    (u16, u16),
    &'a [u8],
    CopyRequest,
    &'a [&'a str],
    &'a [&'a str],
);

/// Each request over a written screen, as the text it must contain and
/// must not. A rectangle takes its cells (linear across rows); a word grab
/// the word under the cursor; a line grab its whole row and never the next;
/// `All` ignores the cursor; `LineSemantic` is bounded by the OSC-133 prompt
/// marks; `Output` takes the OSC-133 C zone.
#[test]
fn extraction_resolves_each_request_over_the_screen() {
    use SelectionGrab::{All, Line, LineSemantic, Output};
    const PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07";
    let output = [PROMPT, b"cat f\r\n\x1b]133;C\x07the-output-line\r\n"].concat();
    let semantic = [PROMPT, b"ls -la"].concat();
    #[rustfmt::skip] // a case table, one row per case
    let cases: [ExtractCase<'_>; 4] = [
        ("line grab", (20, 3), b"alpha beta\r\ngamma", grab_req(Line, 0, 2), &["alpha", "beta"], &["gamma"]),
        ("all grab", (20, 3), b"first\r\nsecond", grab_req(All, 0, 0), &["first", "second"], &[]),
        ("semantic line", (40, 3), &semantic, grab_req(LineSemantic, 0, 4), &["ls -la"], &[]),
        ("output zone", (40, 4), &output, grab_req(Output, 1, 3), &["the-output-line"], &[]),
    ];
    for (case, (cols, rows), bytes, req, has, lacks) in cases {
        let mut t = fresh(cols, rows);
        t.vt_write(bytes);
        let text = extract_selection_text(&t, req).unwrap_or_else(|| panic!("{case}: no text"));
        for part in has {
            assert!(
                text.contains(part),
                "{case}: {part:?} missing from {text:?}"
            );
        }
        for part in lacks {
            assert!(
                !text.contains(part),
                "{case}: {part:?} leaked into {text:?}"
            );
        }
    }
}

/// Exact extractions over `hello world`: a rectangle over cols 0..=4, and
/// a word grab inside "world".
#[test]
fn extraction_of_a_single_word_is_exact() {
    let mut t = fresh(20, 3);
    t.vt_write(b"hello world");
    let cases = [
        ("rect", rect_req(0, 0, 0, 4), "hello"),
        ("word grab", grab_req(SelectionGrab::Word, 0, 8), "world"),
    ];
    for (case, req, want) in cases {
        assert_eq!(
            extract_selection_text(&t, req).as_deref(),
            Some(want),
            "{case}"
        );
    }
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

/// A full-screen program (an editor, an agent's interface) draws on the
/// alternate screen, which has no scrollback of its own. Copy mode reads
/// what that screen shows, and the shell's lines come back when it exits.
#[test]
fn alternate_screen_copies_what_the_program_shows() {
    let mut t = fresh(20, 3);
    t.vt_write(b"shell line\r\n");
    t.vt_write(b"\x1b[?1049h\x1b[H\x1b[2Jeditor text\r\nsecond row");

    assert_eq!(
        extract_selection_text(&t, rect_req(0, 0, 1, 9)).as_deref(),
        Some("editor text\nsecond row")
    );
    assert_eq!(
        extract_selection_text(&t, grab_req(SelectionGrab::Word, 0, 1)).as_deref(),
        Some("editor")
    );
    let all = extract_selection_text(&t, grab_req(SelectionGrab::All, 0, 0)).unwrap_or_default();
    assert!(!all.contains("shell line"), "{all:?}");

    t.vt_write(b"\x1b[?1049l");
    assert_eq!(
        extract_selection_text(&t, rect_req(0, 0, 0, 9)).as_deref(),
        Some("shell line")
    );
}
