use super::*;

/// The screen state of a fresh `cols`x`rows` terminal after `bytes`.
fn screen(cols: u16, rows: u16, bytes: &[u8], scrollback: Option<u32>, cells: bool) -> ScreenState {
    let mut t = fresh(cols, rows);
    t.vt_write(bytes);
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    synth
        .screen_state_with_scrollback(&t, 1, scrollback, cells)
        .expect("screen_state_with_scrollback")
}

/// The sparse styled-cell projection of `bytes` on a fresh terminal.
fn styled_cells(cols: u16, rows: u16, bytes: &[u8]) -> Vec<phux_core::screen::CellInfo> {
    screen(cols, rows, bytes, None, true)
        .cells
        .expect("cells = true populates Some(..)")
}

#[test]
fn screen_state_projects_text_lines_and_dims() {
    // The agent-surface read path: walk the grid into structured text.
    let mut t = fresh(20, 5);
    t.vt_write(b"hello\r\nworld");
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    let screen = synth.screen_state(&t, 7).expect("screen_state");

    assert_eq!(screen.schema_version, SCHEMA_VERSION);
    assert_eq!(screen.pane, 7, "pane id is stamped from the argument");
    assert_eq!((screen.cols, screen.rows), (20, 5));
    assert_eq!(screen.lines.len(), 5, "one entry per grid row");
    assert_eq!(screen.lines[0], "hello");
    assert_eq!(screen.lines[1], "world");
    // Trailing blank rows trim to empty strings.
    assert_eq!(screen.lines[4], "");
    // Cursor lands just past "world" on row 1 (0-based).
    let cursor = screen.cursor.expect("cursor resolvable in viewport");
    assert_eq!((cursor.x, cursor.y), (5, 1));
}

#[test]
fn screen_state_without_cells_leaves_cells_none() {
    // The default read path (cells = false) must not allocate the
    // cells projection — back-compat with the pre-phux-8yl shape.
    let screen = screen(20, 3, b"hello", None, false);
    assert!(screen.cells.is_none(), "cells = false leaves cells None");
}

#[test]
fn screen_state_cells_collects_styles_sparsely() {
    // Bold-red "HI" then plain "ok": only the styled cells appear.
    // ESC[1;31m = bold + red fg; "HI"; ESC[0m reset; " ok".
    let cells = styled_cells(20, 2, b"\x1b[1;31mHI\x1b[0m ok");

    assert_eq!(
        cells.len(),
        2,
        "only the two bold-red cells are emitted, got {cells:?}",
    );
    for (i, cell) in cells.iter().enumerate() {
        assert_eq!((cell.row, cell.col), (0, u16::try_from(i).unwrap()));
        assert!(cell.style.bold, "bold cell {i}");
        assert!(!cell.style.italic, "not italic {i}");
        // ANSI `31` is palette slot 1 (red); the explicit per-cell
        // palette index is preserved rather than collapsed to RGB.
        assert_eq!(
            cell.style.fg,
            CellColor::Palette { index: 1 },
            "ANSI red keeps its palette identity",
        );
        assert_eq!(cell.style.bg, CellColor::Default, "no explicit bg");
        assert!(
            cell.semantic.is_none(),
            "no OSC-133 marks written -> Output collapses to None",
        );
    }
}

#[test]
fn screen_state_cells_captures_osc133_semantic_marks() {
    // OSC 133 A (prompt) then B (input): cells carry the semantic mark.
    // OSC 133 ;A starts the "$ " prompt, ;B the "ls" input.
    let cells = styled_cells(40, 2, b"\x1b]133;A\x07$ \x1b]133;B\x07ls");

    let prompt_marked = cells
        .iter()
        .any(|c| matches!(c.semantic, Some(SemanticContent::Prompt)));
    let input_marked = cells
        .iter()
        .any(|c| matches!(c.semantic, Some(SemanticContent::Input)));
    assert!(
        prompt_marked,
        "OSC-133 ;A region must surface a Prompt cell, got {cells:?}",
    );
    assert!(
        input_marked,
        "OSC-133 ;B region must surface an Input cell, got {cells:?}",
    );
}

#[test]
fn screen_state_cells_reports_true_column_after_wide_glyph() {
    // Styled cell right of a wide glyph reports its true grid column.
    // Unstyled wide glyph (你, two columns) then a bold "X" at col 2.
    let cells = styled_cells(20, 2, "你\x1b[1mX".as_bytes());

    // The wide glyph is unstyled and dropped by the sparse filter, so the
    // bold X is the only emitted cell; it must report col 2, not col 1.
    let x = cells
        .iter()
        .find(|c| c.style.bold)
        .expect("bold X after the wide glyph must be emitted");
    assert_eq!(
        (x.row, x.col),
        (0, 2),
        "styled cell after a double-width glyph must report true column 2, got {cells:?}",
    );
}

#[test]
fn screen_state_cells_accounts_for_spacer_head_at_soft_wrap() {
    // A wide glyph that does not fit wraps, leaving a width-1 SpacerHead:
    //   row 0:  a(0) b(1) c(2) SpacerHead(3)
    //   row 1:  你(0,wide) SpacerTail(1) d(2)
    let cells = styled_cells(4, 3, "\x1b[1mabc你d".as_bytes());

    let coords: Vec<(u16, u16)> = cells.iter().map(|c| (c.row, c.col)).collect();
    assert_eq!(
        coords,
        vec![(0, 0), (0, 1), (0, 2), (0, 3), (1, 0), (1, 2)],
        "SpacerHead is a width-1 cell at the wrapped row's last column; \
             the wide glyph restarts column accounting at col 0 of the next \
             row and its SpacerTail (row 1, col 1) is skipped, got {cells:?}",
    );
}

/// History is oldest-first; a bounded request keeps the rows nearest the
/// viewport and flags what it clipped; no request (or no history) yields
/// none and leaves the viewport as the plain read sees it.
#[test]
fn screen_state_with_scrollback_windows_history() {
    let five = b"line1\r\nline2\r\nline3\r\nline4\r\nline5";
    let cases: &[(u16, Option<u32>, &[&str], bool)] = &[
        (3, Some(SCROLLBACK_ALL), &["line1", "line2"], false),
        (2, Some(SCROLLBACK_ALL), &["line1", "line2", "line3"], false),
        (2, Some(3), &["line1", "line2", "line3"], false),
        (2, Some(2), &["line2", "line3"], true),
        (2, None, &[], false),
        (6, Some(SCROLLBACK_ALL), &[], false),
    ];
    for &(rows, window, history, truncated) in cases {
        let got = screen(20, rows, five, window, false);
        assert_eq!(got.scrollback, history, "{rows} rows, {window:?}");
        assert_eq!(got.truncated, truncated, "{rows} rows, {window:?}");
        let reason = truncated.then_some(TRUNCATED_ROW_WINDOW);
        assert_eq!(got.truncated_reason.as_deref(), reason);
        assert_eq!(
            got.lines.len(),
            usize::from(rows),
            "viewport stays full height"
        );
    }
    let mut t = fresh(20, 3);
    t.vt_write(five);
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    let plain = synth.screen_state(&t, 0).expect("screen_state");
    assert_eq!(plain.lines, ["line3", "line4", "line5"]);
    assert!(plain.scrollback.is_empty());
}

/// A long line soft-wraps; unwrapping joins it back into the written
/// text, which no single painted row contains.
#[test]
fn screen_state_reports_soft_wrap_and_unwraps_to_the_written_line() {
    let screen = screen(10, 4, b"abcdefghijklmnop", None, false);

    let wrap = screen.soft_wrap.as_ref().expect("wrap info is reported");
    assert_eq!(
        wrap.lines,
        vec![0],
        "row 0 continues onto row 1, got {screen:?}",
    );
    assert_eq!(screen.lines[0], "abcdefghij");
    assert_eq!(screen.lines[1], "klmnop");
    assert!(
        !screen.lines.iter().any(|l| l.contains("ijkl")),
        "the straddling substring is absent from the rows as painted",
    );
    assert_eq!(
        screen.unwrapped_rows()[0],
        "abcdefghijklmnop",
        "unwrapping restores the line as written",
    );
}

/// A wide glyph wrapped past a `SpacerHead` still reports the wrap, and the
/// join does not include the spacer's blank.
#[test]
fn screen_state_unwraps_across_a_wide_glyph_at_the_wrap_boundary() {
    let screen = screen(4, 3, "abc你d".as_bytes(), None, false);

    let wrap = screen.soft_wrap.as_ref().expect("wrap info is reported");
    assert_eq!(wrap.lines, vec![0], "got {screen:?}");
    assert_eq!(
        screen.lines[0], "abc",
        "the SpacerHead is a blank final column and trims away",
    );
    assert_eq!(screen.lines[1], "你d");
    assert_eq!(
        screen.unwrapped_rows()[0],
        "abc你d",
        "the wide glyph rejoins its line with no stray spacer column",
    );
}

/// History rows report wrap bits too, and a run across the seam joins.
#[test]
fn screen_state_reports_soft_wrap_in_scrollback() {
    // 3-row viewport; a long first line wraps into two rows and the
    // later lines push both into history.
    let bytes = b"abcdefghijklmnop\r\nsecond\r\nthird\r\nfourth\r\nfifth";
    let screen = screen(10, 3, bytes, Some(SCROLLBACK_ALL), false);

    let wrap = screen.soft_wrap.as_ref().expect("wrap info is reported");
    assert_eq!(
        wrap.scrollback,
        vec![0],
        "history row 0 continues onto history row 1, got {screen:?}",
    );
    assert_eq!(
        screen.unwrapped_rows()[0],
        "abcdefghijklmnop",
        "the wrapped history line rejoins",
    );
}

/// The OSC 0/2 title rides back when set, and is absent — not an empty
/// string — when the pane never set one (ADR-0077 §3).
#[test]
fn screen_state_reports_the_osc_title_when_set() {
    let mut t = fresh(20, 2);
    let mut synth = SnapshotSynthesizer::new().expect("synth");
    assert!(
        synth
            .screen_state(&t, 0)
            .expect("screen_state")
            .title
            .is_none(),
        "an unset title is absence, not an empty string",
    );

    t.vt_write(b"\x1b]0;claude - phux\x07");
    assert_eq!(
        synth.screen_state(&t, 0).expect("screen_state").title,
        Some("claude - phux".to_owned()),
    );
}
