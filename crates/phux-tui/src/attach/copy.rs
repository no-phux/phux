//! Client-local copy-mode extraction and clipboard emission (ADR-0030: a
//! client-side projection, not a wire tier). On commit, the overlay's
//! viewport [`CopyRequest`] becomes a one-shot [`Selection`] on the focused
//! pane's own terminal, formatted to text and written to the host clipboard
//! via OSC 52.

use std::io::{self, Write};

use libghostty_vt::{
    Terminal as GhosttyTerminal,
    fmt::Format,
    screen::GridRef,
    selection::{FormatOptions, SelectLineOptions, SelectWordOptions, Selection},
    terminal::{Point, PointCoordinate},
};

use crate::render::overlay::{CopyRequest, ScreenSelectionPoint, SelectionGrab};

/// Base64 alphabet (RFC 4648 §4, standard, with `+`/`/` and `=` padding).
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// The plain text of `req`'s selection in `terminal`: `Rect` builds a
/// two-corner selection (rectangular when `req.rectangle`); the other grabs
/// use libghostty's `select_*` at the overlay cursor (`Output` needs OSC-133
/// zones). `None` when nothing is selectable or a call fails (best-effort).
#[must_use]
pub fn extract_selection_text(
    terminal: &GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<String> {
    let selection = resolve_selection(terminal, req)?;
    let bytes = terminal
        .format_selection_alloc(
            None,
            FormatOptions::new()
                .with_emit_format(Format::Plain)
                .with_trim(true)
                .with_unwrap(true)
                .with_selection(&selection),
        )
        .ok()??;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The one-shot [`Selection`] `req`'s grab names, or `None`.
fn resolve_selection<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<Selection<'t>> {
    match req.grab {
        SelectionGrab::Rect => select_drag_rect(terminal, req),
        SelectionGrab::All => terminal.select_all().ok().flatten(),
        SelectionGrab::Word => select_word_at_cursor(terminal, req),
        SelectionGrab::Line | SelectionGrab::LineSemantic => select_line_at_cursor(terminal, req),
        SelectionGrab::Output => select_output_at_cursor(terminal, req),
    }
}

/// Map the overlay's inclusive `(row, col)` viewport rectangle onto two
/// [`Point::Viewport`] grid references and build the two-corner [`Selection`]
/// (rectangular when `req.rectangle`).
fn select_drag_rect<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<Selection<'t>> {
    // Endpoints are inclusive (see `Selection::new`); the overlay's
    // CellRange is already normalized so start <= end and both ends
    // name real cells.
    let start = req.mouse_anchor_screen.map_or_else(
        || viewport_grid_ref(terminal, req.start_col, req.start_row),
        |point| screen_grid_ref(terminal, point),
    )?;
    let end = viewport_grid_ref(terminal, req.end_col, req.end_row)?;
    Some(Selection::new(start, end, req.rectangle))
}

/// Resolve a stored mouse press point against the full primary screen. This
/// is valid for both scrollback and active rows, unlike `Point::History`.
fn screen_grid_ref<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    point: ScreenSelectionPoint,
) -> Option<GridRef<'t>> {
    terminal
        .grid_ref(Point::Screen(PointCoordinate {
            x: point.col,
            y: point.row,
        }))
        .ok()
}

/// libghostty's own word selection at the overlay cursor.
fn select_word_at_cursor<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<Selection<'t>> {
    let cursor = viewport_cursor_ref(terminal, req)?;
    terminal
        .select_word(SelectWordOptions::new(cursor))
        .ok()
        .flatten()
}

/// libghostty's own line selection at the overlay cursor, honoring the
/// OSC-133 semantic-prompt boundary for [`SelectionGrab::LineSemantic`].
fn select_line_at_cursor<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<Selection<'t>> {
    let cursor = viewport_cursor_ref(terminal, req)?;
    let opts = SelectLineOptions::new(cursor)
        .with_semantic_prompt_boundary(req.grab == SelectionGrab::LineSemantic);
    terminal.select_line(opts).ok().flatten()
}

/// libghostty's own command-output selection at the overlay cursor.
///
/// Best-effort: no OSC-133 zones -> `None` -> silent no-op copy.
fn select_output_at_cursor<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<Selection<'t>> {
    let cursor = viewport_cursor_ref(terminal, req)?;
    terminal.select_output(cursor).ok().flatten()
}

/// The grid reference under the overlay cursor
/// (`req.cursor_row`/`req.cursor_col`) — the anchor every engine-derived grab
/// resolves from.
fn viewport_cursor_ref<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> Option<GridRef<'t>> {
    viewport_grid_ref(terminal, req.cursor_col, req.cursor_row)
}

/// An overlay `(col, row)` as a viewport grid reference (the coordinates
/// index the visible viewport the user selected from).
fn viewport_grid_ref<'t>(
    terminal: &'t GhosttyTerminal<'_, '_>,
    col: u16,
    row: u16,
) -> Option<GridRef<'t>> {
    terminal
        .grid_ref(Point::Viewport(PointCoordinate {
            x: col,
            y: u32::from(row),
        }))
        .ok()
}

/// An OSC 52 "set clipboard" sequence: `ESC ] 52 ; c ; <base64> BEL`.
/// Whether the host honors it is up to the host.
#[must_use]
pub fn osc52_set_clipboard(text: &str) -> Vec<u8> {
    let encoded = base64_encode(text.as_bytes());
    let mut out = Vec::with_capacity(encoded.len() + 8);
    out.extend_from_slice(b"\x1b]52;c;");
    out.extend_from_slice(encoded.as_bytes());
    out.push(0x07); // BEL terminator
    out
}

/// The copy-mode extraction bridge (ADR-0045): resolve `req` against the
/// focused pane's own engine and, when the selection is non-empty, write an
/// OSC 52 clipboard sequence to `out` (the host terminal). The one seam where
/// copy-mode touches the engine; nothing goes on the wire. Best-effort.
pub fn copy_to_host_clipboard<W: Write>(
    out: &mut W,
    terminal: &GhosttyTerminal<'_, '_>,
    req: CopyRequest,
) -> io::Result<()> {
    let Some(text) = extract_selection_text(terminal, req) else {
        return Ok(());
    };
    if text.is_empty() {
        return Ok(());
    }
    out.write_all(&osc52_set_clipboard(&text))?;
    out.flush()
}

/// Standard padded base64 (RFC 4648), hand-rolled to avoid a dependency on a
/// cold path.
fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        out.push(char::from(B64[usize::from(b0 >> 2)]));
        out.push(char::from(B64[usize::from(((b0 & 0b11) << 4) | (b1 >> 4))]));
        if chunk.len() > 1 {
            out.push(char::from(
                B64[usize::from(((b1 & 0b1111) << 2) | (b2 >> 6))],
            ));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char::from(B64[usize::from(b2 & 0b11_1111)]));
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use libghostty_vt::{
        Terminal as GhosttyTerminal,
        terminal::{PointSpace, ScrollViewport},
    };

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
    fn osc52_empty_text_is_well_formed() {
        // An empty payload is still a valid (no-op) set; callers skip empty
        // selections upstream, but the encoder must not panic or malform.
        assert_eq!(osc52_set_clipboard(""), b"\x1b]52;c;\x07");
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
}
