//! The escape sequences every projection must agree on.

/// One corpus entry: a grid size plus the VT bytes to feed it (a `&str` so
/// escapes and non-ASCII cases share one literal).
pub(super) struct Case {
    /// Names the failure. Printed in every assertion message.
    pub(super) name: &'static str,
    /// Grid width in cells.
    pub(super) cols: u16,
    /// Grid height in cells.
    pub(super) rows: u16,
    /// The VT sequence written into each projection's private terminal.
    pub(super) bytes: &'static str,
}

/// Scrollback for every terminal: zero, matching `Replayer::new`, so all three
/// compare the same viewport.
pub(super) const MAX_SCROLLBACK: usize = 0;

/// The shared corpus, run through all three projections: styling, Unicode
/// width and clustering, screen/scroll/wrap modes, cursor, degenerate cases.
/// A new VT feature gets conformance coverage by appending one row.
pub(super) const CORPUS: &[Case] = &[
    // The settled screen: pins the blank-cell grapheme (`" "`) and default
    // style.
    Case {
        name: "settled_empty",
        cols: 20,
        rows: 4,
        bytes: "",
    },
    // Unstyled text — the control. If this one fails, nothing below is
    // diagnostic.
    Case {
        name: "plain_text",
        cols: 20,
        rows: 3,
        bytes: "hello world",
    },
    // SGR 38;2 / 48;2 truecolor. Both directions of `CellColor::Rgb`, and a
    // reset back to `CellColor::Default` on the same row so the projections
    // must also agree on where the styled run *stops*.
    Case {
        name: "sgr_truecolor",
        cols: 24,
        rows: 3,
        bytes: "\x1b[38;2;255;128;0m\x1b[48;2;0;32;64mTRUE\x1b[0m plain",
    },
    // 256-palette colors (cube 196, low 9, bright SGR 91): projections keep the
    // index, not RGB.
    Case {
        name: "sgr_palette_256",
        cols: 24,
        rows: 3,
        bytes: "\x1b[38;5;196m\x1b[48;5;21mPAL\x1b[0m\x1b[38;5;9mA\x1b[0m\x1b[91mB\x1b[0m",
    },
    // Every boolean attribute, one per column: bold, faint, italic, underline,
    // blink, inverse, invisible, strikethrough, overline.
    Case {
        name: "sgr_attributes",
        cols: 24,
        rows: 3,
        bytes: "\x1b[1mb\x1b[0m\x1b[2mf\x1b[0m\x1b[3mi\x1b[0m\x1b[4mu\x1b[0m\x1b[5mk\x1b[0m\
                \x1b[7mv\x1b[0m\x1b[8mh\x1b[0m\x1b[9ms\x1b[0m\x1b[53mo\x1b[0m",
    },
    // Underline STYLE variants (double, curly, dotted, dashed). `CellStyle`
    // collapses all of them to one bool by design (see its type docs), so the
    // interesting claim is that all three collapse them the same way.
    Case {
        name: "sgr_underline_styles",
        cols: 24,
        rows: 3,
        bytes: "\x1b[21mD\x1b[4:3mC\x1b[4:4mT\x1b[4:5mA\x1b[24mN",
    },
    // Wide CJK: the tail column is `""` in dense projections and absent from
    // the sparse one; trailing ASCII checks column accounting.
    Case {
        name: "wide_cjk",
        cols: 20,
        rows: 3,
        bytes: "日本語 ok",
    },
    // A styled wide glyph: the background must reach the tail; the styled `X`
    // proves both shapes put it at column 2.
    Case {
        name: "wide_cjk_styled",
        cols: 20,
        rows: 3,
        bytes: "\x1b[41m\x1b[1m語\x1b[0m\x1b[4mX\x1b[0m",
    },
    // A wide glyph that does not fit the last column: a `SpacerHead` (a real
    // column) is parked and the glyph wraps.
    Case {
        name: "wide_glyph_soft_wraps_at_margin",
        cols: 4,
        rows: 3,
        bytes: "\x1b[1mabc你d",
    },
    // Combining marks: base + U+0301 / U+0300 must land in one cell as a
    // multi-scalar cluster, not two cells and not a dropped mark.
    Case {
        name: "combining_marks",
        cols: 20,
        rows: 3,
        bytes: "e\u{0301}cole a\u{0300} co\u{0302}te\u{0301}",
    },
    // ZWJ sequences: a family emoji is one cluster of seven scalars. All
    // three must join it identically or an export shows a different number of
    // people than the screen did.
    Case {
        name: "zwj_emoji",
        cols: 20,
        rows: 3,
        bytes: "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467} x",
    },
    // Alt screen, entered and left painted. `?1049h` clears the alt buffer,
    // so all three must be looking at the alt grid, not the primary one they
    // would still see if they read the wrong screen.
    Case {
        name: "alt_screen_entered",
        cols: 20,
        rows: 4,
        bytes: "primary text\x1b[?1049h\x1b[HALT",
    },
    // ...and back out. The primary content must be the answer again, which is
    // the case that catches a projection caching the alt grid.
    Case {
        name: "alt_screen_round_trip",
        cols: 20,
        rows: 4,
        bytes: "primary text\x1b[?1049h\x1b[HALT\x1b[?1049l",
    },
    // DECSTBM scroll region: five lines through a three-line region, so rows
    // 1..=3 have scrolled and rows 0 and 4 have not. A projection reading the
    // wrong row band lands here.
    Case {
        name: "scroll_region",
        cols: 20,
        rows: 6,
        bytes: "top\x1b[2;4r\x1b[2Hone\r\ntwo\r\nthree\r\nfour\r\nfive",
    },
    // DECAWM on (the default): writing past the right margin soft-wraps.
    // The final glyph also leaves the cursor in libghostty's pending-wrap
    // state, which is the interesting cursor case.
    Case {
        name: "decawm_wrap_at_margin",
        cols: 8,
        rows: 3,
        bytes: "abcdefghijklmn",
    },
    // Exactly-fills-the-row: the cursor sits at the right margin with the
    // wrap pending and NOTHING has moved to row 1 yet. All three must report
    // the same cursor column for it.
    Case {
        name: "decawm_pending_wrap",
        cols: 8,
        rows: 3,
        bytes: "abcdefgh",
    },
    // DECAWM off: the last column is overwritten in place instead of
    // wrapping, so row 1 stays empty.
    Case {
        name: "decawm_off_no_wrap",
        cols: 8,
        rows: 3,
        bytes: "\x1b[?7labcdefghijklmn",
    },
    // Cursor parked somewhere non-trivial by CUP, with content elsewhere.
    Case {
        name: "cursor_position",
        cols: 20,
        rows: 4,
        bytes: "\x1b[3;5Hmark\x1b[2;2H",
    },
    // DECTCEM off. Invisible to the grid — not one cell changes — so a
    // projection that reads visibility from the wrong place fails only here.
    Case {
        name: "cursor_hidden",
        cols: 20,
        rows: 3,
        bytes: "\x1b[?25lhidden",
    },
    // ...and back on, so `visible: true` is not passing by default.
    Case {
        name: "cursor_shown_again",
        cols: 20,
        rows: 3,
        bytes: "\x1b[?25l\x1b[?25hshown",
    },
    // More lines than the viewport: content scrolls off the top. With
    // `MAX_SCROLLBACK == 0` the scrolled rows are gone in all three.
    Case {
        name: "scrolled_off_top",
        cols: 20,
        rows: 3,
        bytes: "one\r\ntwo\r\nthree\r\nfour\r\nfive",
    },
    // Paint, erase, repaint. `ED 2` must leave no styled residue behind for
    // the sparse projection to report and the dense ones to have cleared.
    Case {
        name: "erase_then_repaint",
        cols: 20,
        rows: 3,
        bytes: "\x1b[41mfilled\x1b[0m\x1b[2J\x1b[Hafter",
    },
    // OSC-133 marks with styling on top: the style comparison runs with them
    // present (the marks are server-only).
    Case {
        name: "osc133_prompt_and_input",
        cols: 20,
        rows: 3,
        bytes: "\x1b]133;A\x07\x1b[32m$ \x1b[0m\x1b]133;B\x07ls -l",
    },
];
