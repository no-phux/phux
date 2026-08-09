//! Conformance: the three libghostty cell projections must agree.
//!
//! The libghostty-snapshot -> cell projection exists three times in this
//! workspace, deliberately:
//!
//!   * `phux-client`'s `attach::render::TerminalRenderer::render_at_cells`
//!     (plus its `to_cell_style` / `cell_color` helpers) — what the human's
//!     glass shows, answering `phux snapshot --rendered`;
//!   * `phux-server`'s `grid::synthesizer`'s `screen_state_with_scrollback`
//!     (plus its `collect_cell` / `cell_color`) — what an agent reads,
//!     answering `phux snapshot --cells`;
//!   * `phux-record`'s `replay::Replayer::sample` (plus its `project_cell`)
//!     — what a recording exports.
//!
//! The duplication is a decision, documented at the top of
//! `crates/phux-record/src/replay.rs`: the only crate all three could import
//! from is `phux-core`, and hoisting the projection there would force the
//! domain crate to take a `libghostty-vt` dependency. Three ~30-line walks is
//! the better trade — *provided something notices when they drift*. Nothing
//! did. This file is that something.
//!
//! Divergence in this code has no loud failure mode. It surfaces months later
//! as "the recording does not match the screen" or "the agent sees different
//! cells than the human", and by then nobody remembers which of the three
//! moved. One corpus of VT byte sequences, fed through all three, compared
//! cell-for-cell, turns that into a red test on the commit that caused it.
//!
//! # Why this file lives in `crates/phux/tests/`
//!
//! The test must see all three projections, and the `phux` binary crate is
//! the only workspace member that already depends on `phux-client`,
//! `phux-server`, and `phux-record` (with `phux-record`'s `render` feature
//! on, which is what gates the replayer). It also already carries
//! `libghostty-vt` as a dev-dependency for the PTY oracles in `examples/`.
//! So this costs no new crate, no new dependency edge, and no new published
//! surface — `phux` is `publish = false`. A dedicated `phux-conformance` test
//! crate was the alternative; it would have added a workspace member whose
//! entire content is this file, and a fourth place to remember to update.
//!
//! # The dirty-bit rule this file obeys
//!
//! `RenderState::update` **consumes** the terminal's dirty bits. Two render
//! states observing one `Terminal` race for them, and the loser reads back a
//! stale cached row body. That is not hypothetical here: it cost this repo
//! two CI flake investigations (`phux-uow0`'s `attach_detach_churn` and
//! `phux-5pyx`'s `route_input_no_resize`), and both fixes are the reason
//! `synthesize` and `screen_state_with_scrollback` build a *fresh*
//! `RenderState` per call instead of using the pooled one — see the body
//! comment at `crates/phux-server/src/grid/synthesizer.rs`.
//!
//! A conformance test is exactly the shape that reintroduces the bug: the
//! obvious implementation drives one `Terminal` and points all three
//! projections at it. This file does not. Each projection gets its **own
//! private `Terminal`**, constructed fresh and fed the identical byte
//! sequence ([`client_frame`], [`server_state`], and `phux-record`'s
//! `Replayer`, which owns its terminal internally and never lends it out).
//! Feeding the same bytes to three emulators is the same test — libghostty is
//! deterministic — without ever putting two `RenderState`s on one grid. If a
//! future case here needs two projections over one terminal, it does not: add
//! a terminal.
//!
//! # Where the three cannot agree, and why
//!
//! Two asymmetries are structural, not drift. Both are asserted explicitly
//! below rather than normalised away in silence:
//!
//!   1. **The server projection is sparse; the other two are dense.** A
//!      `ScreenState` carries `CellInfo`s only for cells with a non-default
//!      style or a semantic mark, and emits *nothing* for a wide glyph's
//!      `SpacerTail` column. A `RenderedFrame` has one cell per column,
//!      including the tail (as the empty string). [`dense_as_screen_state`]
//!      is the declared bridge between the shapes; the tail rule is pinned on
//!      its own by `wide_tail_is_dense_only_and_the_server_omits_it`.
//!   2. **Only the server carries OSC-133 semantics.** `RenderedCell` has no
//!      field for them by construction — a rendered frame is what a screen
//!      looks like, and a prompt mark is not visible. Pinned by
//!      `osc133_semantics_are_server_only`.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};
use phux_client::attach::render::TerminalRenderer;
use phux_core::screen::{
    CellColor, CellInfo, CellStyle, RenderedFrame, SCHEMA_VERSION, ScreenState, SemanticContent,
};
use phux_record::Replayer;
use phux_server::grid::SnapshotSynthesizer;

/// One corpus entry: a grid size plus the VT byte sequence to feed it.
///
/// `bytes` is a `&str` rather than a `&[u8]` so escape sequences and the
/// non-ASCII cases (CJK, combining marks, ZWJ emoji) can sit side by side in
/// one literal; every projection receives `bytes.as_bytes()`.
struct Case {
    /// Names the failure. Printed in every assertion message.
    name: &'static str,
    /// Grid width in cells.
    cols: u16,
    /// Grid height in cells.
    rows: u16,
    /// The VT sequence written into each projection's private terminal.
    bytes: &'static str,
}

/// Scrollback for every terminal in this file.
///
/// Zero, matching `Replayer::new`, which hard-codes it: an export shows the
/// viewport and retaining history would cost memory for pixels never drawn.
/// The other two projections must be given the same budget or a case that
/// scrolls would compare a grid that kept history against one that did not.
/// Only the viewport is under test here; `ScreenState::scrollback` has its
/// own coverage in the synthesizer's unit tests.
const MAX_SCROLLBACK: usize = 0;

/// The shared corpus. Every case runs through all three projections.
///
/// Ordered roughly by what it exercises: styling, then Unicode width and
/// clustering, then screen/scroll/wrap modes, then the cursor, then the
/// degenerate cases. Anything added here is automatically covered by every
/// comparison below, which is the point — a new VT feature gets conformance
/// coverage by appending one row.
const CORPUS: &[Case] = &[
    // The settled screen. A recording that opens on an idle terminal, an
    // agent polling a pane nothing has written to. All three must agree that
    // nothing is there, which is a weaker claim than it sounds: it pins the
    // blank-cell grapheme (`" "`, not `""`) and the default style.
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
    // SGR 38;5 / 48;5 palette. The projections must keep the INDEX, not the
    // RGB the terminal would resolve it to; `palette_identity_survives_all_three`
    // asserts that directly. Covers a cube index (196), a low ANSI index via
    // the 256 form (9), and the bright-ANSI shorthand (SGR 91), which
    // libghostty also reports as a palette color.
    Case {
        name: "sgr_palette_256",
        cols: 24,
        rows: 3,
        bytes: "\x1b[38;5;196m\x1b[48;5;21mPAL\x1b[0m\x1b[38;5;9mA\x1b[0m\x1b[91mB\x1b[0m",
    },
    // Every boolean attribute `CellStyle` carries, each isolated to its own
    // column so a projection that dropped one is localised to a single cell.
    // Order: bold, faint, italic, underline, blink, inverse, invisible,
    // strikethrough, overline.
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
    // Wide CJK glyphs. The base cell carries the cluster; its SpacerTail
    // column is the EMPTY STRING in the dense projections and absent from the
    // sparse one. Trailing ASCII proves column accounting survives the width-2
    // advance.
    Case {
        name: "wide_cjk",
        cols: 20,
        rows: 3,
        bytes: "日本語 ok",
    },
    // A styled wide glyph: the background must reach the tail column too, so
    // this is where a projection that forgot to style tails would show up.
    // The trailing "X" carries its own (different) style deliberately — an
    // unstyled one is dropped by the server's sparse filter and could not
    // witness that both shapes put it at column 2.
    Case {
        name: "wide_cjk_styled",
        cols: 20,
        rows: 3,
        bytes: "\x1b[41m\x1b[1m語\x1b[0m\x1b[4mX\x1b[0m",
    },
    // A wide glyph that does not fit in the last column: libghostty parks a
    // `SpacerHead` (width 1, empty grapheme) in the vacated column and moves
    // the glyph to column 0 of the next row. SpacerHead is NOT SpacerTail —
    // it is a real column in all three projections.
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
    // OSC-133 shell-integration marks, with styling on top. The semantic
    // marks are server-only (see the module docs); this case is here so the
    // STYLE comparison runs with them present, and so
    // `osc133_semantics_are_server_only` has a case to point at.
    Case {
        name: "osc133_prompt_and_input",
        cols: 20,
        rows: 3,
        bytes: "\x1b]133;A\x07\x1b[32m$ \x1b[0m\x1b]133;B\x07ls -l",
    },
];

/// A fresh terminal of `case`'s dimensions with `case`'s bytes written in.
///
/// Every caller gets its OWN terminal; see the dirty-bit rule in the module
/// docs. `vt_write` is infallible by libghostty's contract (malformed input
/// is logged, not rejected).
fn fed_terminal(case: &Case) -> GhosttyTerminal<'static, 'static> {
    let mut term = GhosttyTerminal::new(TerminalOptions {
        cols: case.cols,
        rows: case.rows,
        max_scrollback: MAX_SCROLLBACK,
    })
    .expect("terminal construction");
    term.vt_write(case.bytes.as_bytes());
    term
}

/// Projection 1: the client's dense `RenderedFrame` (`phux snapshot --rendered`).
///
/// Rendered at origin `(0, 0)` with the clip set to the full grid, which is
/// the single-pane composition — the multi-pane offsets are the compositor's
/// job (`attach::rendered`), not this projection's, and adding them here
/// would test the compositor instead of the cell walk.
fn client_frame(case: &Case) -> RenderedFrame {
    let term = fed_terminal(case);
    let mut renderer = TerminalRenderer::new().expect("terminal renderer");
    let mut frame = RenderedFrame::blank(case.cols, case.rows);
    // `render_at_cells` returns the cursor rather than storing it: the
    // compositor elects which pane's cursor becomes the frame's. With one
    // pane, that election is the identity.
    frame.cursor = renderer
        .render_at_cells(&term, &mut frame, (0, 0), (case.cols, case.rows))
        .expect("render_at_cells");
    frame
}

/// Projection 2: the server's sparse `ScreenState` (`phux snapshot --cells`).
///
/// `cells = true` is required — without it the styles are never collected and
/// there is nothing to compare.
fn server_state(case: &Case) -> ScreenState {
    let term = fed_terminal(case);
    let synth = SnapshotSynthesizer::new().expect("snapshot synthesizer");
    synth
        .screen_state_with_scrollback(&term, 0, None, true)
        .expect("screen_state_with_scrollback")
}

/// Projection 3: the recorder's dense `RenderedFrame` (`phux rec`).
///
/// The `Replayer` owns its terminal and never lends it out, so this is the
/// one projection that could not share a grid even if this file wanted it to.
fn replay_frame(case: &Case) -> RenderedFrame {
    let mut replayer = Replayer::new(case.cols, case.rows).expect("replayer");
    replayer.feed(case.bytes.as_bytes());
    replayer
        .sample()
        .expect("sample")
        // The first sample never returns `None`, whatever the dirty bit says
        // — rule 2 of `phux-record`'s replay module docs, which exists so a
        // recording that opens on a settled screen still renders a frame.
        // `settled_empty` is the corpus case that would catch its loss.
        .expect("the first sample always yields a frame")
        .frame
}

/// Bridge the dense shape to the sparse one: what `ScreenState` the server
/// MUST produce for a grid that renders as `frame`.
///
/// This is the declared translation between the two projections' shapes, and
/// the only place the structural asymmetry is allowed to live:
///
///   * **Lines** are the row's graphemes concatenated and right-trimmed. A
///     wide glyph's tail contributes the empty string and a blank cell
///     contributes `" "`, which is exactly what the server's own walk builds
///     (it skips tails and pushes `' '` for an empty grapheme).
///   * **Cells** are sparse: a cell is emitted only when its style is
///     non-default, and never for a tail column. `semantic` is always `None`
///     here because a `RenderedCell` structurally cannot carry it;
///     [`styled_cells_only`] reduces the server's list to the same dimension
///     before matching, and `osc133_semantics_are_server_only` covers what
///     that reduction sets aside.
///   * **Columns** are dense-frame column indices, which already equal the
///     server's `col_index`: the server advances by 2 across a wide base and
///     skips its tail, while the dense frame spends one index on each. Both
///     land on the same next column, and that shared coordinate space is what
///     `cursor.x` lives in too.
fn dense_as_screen_state(frame: &RenderedFrame, pane: u32) -> ScreenState {
    let mut lines: Vec<String> = Vec::with_capacity(usize::from(frame.rows));
    let mut cells: Vec<CellInfo> = Vec::new();
    for row in 0..frame.rows {
        let mut line = String::new();
        for col in 0..frame.cols {
            let cell = frame.cell(row, col).expect("dense cell in range");
            line.push_str(&cell.grapheme);
            // The tail of a wide glyph: dense-only. The server emits no
            // `CellInfo` for it even when it is styled — the base cell's
            // entry already describes the whole glyph.
            if cell.grapheme.is_empty() {
                continue;
            }
            if cell.style != CellStyle::default() {
                cells.push(CellInfo {
                    col,
                    row,
                    semantic: None,
                    style: cell.style,
                });
            }
        }
        lines.push(line.trim_end().to_owned());
    }
    ScreenState {
        schema_version: SCHEMA_VERSION,
        pane,
        cols: frame.cols,
        rows: frame.rows,
        cursor: frame.cursor.clone(),
        lines,
        scrollback: Vec::new(),
        cells: Some(cells),
        ..ScreenState::default()
    }
}

/// Reduce the server's cells to the dimension the dense projections can
/// express: styled cells, with the semantic mark stripped.
///
/// The filter is the second half of asymmetry (2) and is NOT a convenience.
/// The server's sparse filter admits a cell when it has a non-default style
/// **or** a semantic mark, so an OSC-133 shell emits `CellInfo`s for
/// otherwise-plain prompt and input glyphs. Those cells describe something
/// real that simply is not visible, and a dense frame — which records what
/// the glass shows — has no way to report them. Dropping them here is the
/// only honest comparison; keeping them would fail `osc133_prompt_and_input`
/// for a reason that is a design decision, not drift.
///
/// What this deliberately does NOT relax: any server cell with a non-default
/// style survives the filter and must match the dense frame exactly. A
/// projection that started dropping styles would still fail.
fn styled_cells_only(cells: &[CellInfo]) -> Vec<CellInfo> {
    cells
        .iter()
        .filter(|cell| cell.style != CellStyle::default())
        .map(|cell| CellInfo {
            col: cell.col,
            row: cell.row,
            semantic: None,
            style: cell.style,
        })
        .collect()
}

/// Render `frame` as one string per row, for assertion messages that a human
/// can read without decoding a `Vec<RenderedCell>`.
fn debug_rows(frame: &RenderedFrame) -> Vec<String> {
    (0..frame.rows)
        .map(|row| {
            (0..frame.cols)
                .filter_map(|col| frame.cell(row, col).map(|cell| cell.grapheme.clone()))
                .collect()
        })
        .collect()
}

/// The two dense projections must be identical, whole struct and all.
///
/// This is the strongest of the three comparisons and the one to read first
/// on a failure: client and recorder produce the same type, so there is no
/// shape difference to explain away. Equality includes `schema_version`,
/// dimensions, every cell's grapheme and style, and the cursor.
#[test]
fn client_and_replay_frames_are_identical() {
    for case in CORPUS {
        let client = client_frame(case);
        let replay = replay_frame(case);
        assert_eq!(
            client.cols, replay.cols,
            "[{}] client/replay disagree on width",
            case.name
        );
        assert_eq!(
            client.rows, replay.rows,
            "[{}] client/replay disagree on height",
            case.name
        );
        assert_eq!(
            client.cursor, replay.cursor,
            "[{}] client/replay disagree on the cursor",
            case.name
        );
        // Per-cell first: a whole-frame `assert_eq!` on a 20x4 grid prints
        // 80 cells and names none of them.
        for row in 0..client.rows {
            for col in 0..client.cols {
                assert_eq!(
                    client.cell(row, col),
                    replay.cell(row, col),
                    "[{}] client/replay disagree at (row {row}, col {col})\n\
                     client rows: {:?}\nreplay rows: {:?}",
                    case.name,
                    debug_rows(&client),
                    debug_rows(&replay),
                );
            }
        }
        assert_eq!(
            client, replay,
            "[{}] client/replay frames differ outside the per-cell walk",
            case.name
        );
    }
}

/// The server's sparse projection must describe the same screen the dense
/// ones do, once the shapes are bridged by [`dense_as_screen_state`].
#[test]
fn server_state_matches_the_dense_projections() {
    for case in CORPUS {
        let frame = client_frame(case);
        let expected = dense_as_screen_state(&frame, 0);
        let actual = server_state(case);

        assert_eq!(
            (actual.cols, actual.rows),
            (expected.cols, expected.rows),
            "[{}] server/dense disagree on dimensions",
            case.name
        );
        assert_eq!(
            actual.lines,
            expected.lines,
            "[{}] server text lines differ from the dense frame\ndense rows: {:?}",
            case.name,
            debug_rows(&frame),
        );
        assert_eq!(
            actual.cursor, expected.cursor,
            "[{}] server cursor differs from the dense frame's",
            case.name
        );

        let actual_cells = actual
            .cells
            .as_deref()
            .expect("cells = true populates Some(..)");
        let expected_cells = expected
            .cells
            .as_deref()
            .expect("dense_as_screen_state always populates Some(..)");
        assert_eq!(
            styled_cells_only(actual_cells),
            expected_cells,
            "[{}] server sparse cells differ from the dense frame's styled cells\n\
             dense rows: {:?}",
            case.name,
            debug_rows(&frame),
        );
    }
}

/// The recorder's frame must satisfy the same server bridge the client's
/// does.
///
/// Implied by the two tests above by transitivity, and kept anyway: it makes
/// the triangle explicit, so a future change that weakens
/// `client_and_replay_frames_are_identical` cannot quietly leave the
/// recorder unchecked against the server.
#[test]
fn replay_frame_matches_the_server_projection() {
    for case in CORPUS {
        let expected = dense_as_screen_state(&replay_frame(case), 0);
        let actual = server_state(case);
        assert_eq!(
            actual.lines, expected.lines,
            "[{}] server text lines differ from the recorder's frame",
            case.name
        );
        assert_eq!(
            actual.cursor, expected.cursor,
            "[{}] server cursor differs from the recorder's frame",
            case.name
        );
        assert_eq!(
            styled_cells_only(
                actual
                    .cells
                    .as_deref()
                    .expect("cells = true populates Some(..)")
            ),
            expected
                .cells
                .expect("dense_as_screen_state always populates Some(..)"),
            "[{}] server sparse cells differ from the recorder's styled cells",
            case.name
        );
    }
}

/// A 256-palette color must stay a palette INDEX in all three projections.
///
/// This is a deliberate choice, not an accident of the walk, and it is
/// load-bearing at both ends: the client keeps the index so a re-themed
/// terminal repaints correctly, and `phux-record`'s rasterizer resolves
/// `CellColor::Palette` through the *recording's* theme so a cast exported
/// under two themes paints each correctly. Flattening to RGB at any one of
/// the three boundaries would bake the capture-time palette in — and would
/// still look plausible, which is why it needs a test rather than a review.
#[test]
fn palette_identity_survives_all_three() {
    let case = CORPUS
        .iter()
        .find(|case| case.name == "sgr_palette_256")
        .expect("the palette case is in the corpus");

    let client = client_frame(case);
    let replay = replay_frame(case);
    let server = server_state(case);

    // "PAL" is written with fg 196 / bg 21; the first cell is enough to pin
    // both channels.
    let client_cell = client.cell(0, 0).expect("client cell (0, 0)");
    let replay_cell = replay.cell(0, 0).expect("replay cell (0, 0)");
    for (which, style) in [("client", client_cell.style), ("replay", replay_cell.style)] {
        assert_eq!(
            style.fg,
            CellColor::Palette { index: 196 },
            "{which} flattened an SGR 38;5;196 foreground instead of keeping the index",
        );
        assert_eq!(
            style.bg,
            CellColor::Palette { index: 21 },
            "{which} flattened an SGR 48;5;21 background instead of keeping the index",
        );
    }

    let server_cell = server
        .cells
        .as_deref()
        .expect("cells = true populates Some(..)")
        .iter()
        .find(|cell| (cell.row, cell.col) == (0, 0))
        .expect("server cell (0, 0)");
    assert_eq!(
        server_cell.style.fg,
        CellColor::Palette { index: 196 },
        "server flattened an SGR 38;5;196 foreground instead of keeping the index",
    );
    assert_eq!(
        server_cell.style.bg,
        CellColor::Palette { index: 21 },
        "server flattened an SGR 48;5;21 background instead of keeping the index",
    );
}

/// A wide glyph's tail column: dense projections carry it as the EMPTY
/// STRING; the sparse one omits it entirely.
///
/// This is asymmetry (1) from the module docs, asserted rather than assumed.
/// The empty string is not cosmetic — emitting `" "` there instead would
/// shift every later column of any line containing CJK, and emitting nothing
/// would break the `cells[row * cols + col]` indexing contract
/// `RenderedFrame` documents. The server, whose consumer indexes by the
/// `(row, col)` it is handed, has no such column to describe.
#[test]
fn wide_tail_is_dense_only_and_the_server_omits_it() {
    let case = CORPUS
        .iter()
        .find(|case| case.name == "wide_cjk_styled")
        .expect("the styled wide-glyph case is in the corpus");

    let client = client_frame(case);
    let replay = replay_frame(case);

    // Column 0 is the wide base and carries the whole cluster; column 1 is
    // its tail and carries nothing; column 2 is the next real glyph.
    for (which, frame) in [("client", &client), ("replay", &replay)] {
        let base = frame.cell(0, 0).expect("wide base cell");
        let tail = frame.cell(0, 1).expect("wide tail cell");
        let next = frame.cell(0, 2).expect("cell after the wide glyph");
        assert_eq!(
            base.grapheme, "語",
            "{which}: the wide base must carry the whole cluster",
        );
        assert_eq!(
            tail.grapheme, "",
            "{which}: a SpacerTail must be the empty string, not a space",
        );
        assert_eq!(
            next.grapheme, "X",
            "{which}: the glyph after a wide one must land at column 2",
        );
        assert_eq!(
            tail.style, base.style,
            "{which}: the tail inherits the base cell's style, so a background \
             painted across a wide glyph covers both of its columns",
        );
    }

    let server = server_state(case);
    let cells = server
        .cells
        .as_deref()
        .expect("cells = true populates Some(..)");
    assert!(
        cells.iter().any(|cell| (cell.row, cell.col) == (0, 0)),
        "the server must describe the wide base at column 0, got {cells:?}",
    );
    assert!(
        !cells.iter().any(|cell| (cell.row, cell.col) == (0, 1)),
        "the server must NOT emit a CellInfo for the wide glyph's tail column \
         even though the tail is styled — the base entry already describes the \
         glyph, and column 1 does not exist in the sparse coordinate space; \
         got {cells:?}",
    );
    // Both shapes still agree on where the NEXT glyph is: the dense frame
    // spent one index on the tail, the server advanced two across the base.
    assert!(
        cells.iter().any(|cell| (cell.row, cell.col) == (0, 2)),
        "the styled glyph after a wide one must report column 2 in the sparse \
         projection too, got {cells:?}",
    );
}

/// OSC-133 semantic marks are the server's alone, by construction.
///
/// This is asymmetry (2) from the module docs. `RenderedCell` has no
/// `semantic` field and should not grow one: a rendered frame answers "what
/// does the glass show", and a prompt mark shows nothing. The agent surface
/// (ADR-0022) is where the distinction between prompt, input, and output
/// earns its keep. Pinning it here means a future attempt to align the three
/// projections by deleting the server's semantic collection fails loudly
/// instead of passing as "now they match".
#[test]
fn osc133_semantics_are_server_only() {
    let case = CORPUS
        .iter()
        .find(|case| case.name == "osc133_prompt_and_input")
        .expect("the OSC-133 case is in the corpus");

    let server = server_state(case);
    let cells = server
        .cells
        .as_deref()
        .expect("cells = true populates Some(..)");
    assert!(
        cells
            .iter()
            .any(|cell| cell.semantic == Some(SemanticContent::Prompt)),
        "the server projection must surface the OSC-133 ;A prompt region, got {cells:?}",
    );
    assert!(
        cells
            .iter()
            .any(|cell| cell.semantic == Some(SemanticContent::Input)),
        "the server projection must surface the OSC-133 ;B input region, got {cells:?}",
    );

    // The concrete shape of the divergence: the "ls -l" input glyphs are
    // plain text — no SGR at all — and appear in the sparse projection ONLY
    // because they carry a mark. A dense frame cannot report them, because
    // there is nothing about those cells to report; they look exactly like
    // any other letter on the glass. This is what `styled_cells_only` sets
    // aside, and asserting it here is what makes that filter a declared
    // decision rather than a way to get the loop green.
    let semantic_only: Vec<&CellInfo> = cells
        .iter()
        .filter(|cell| cell.style == CellStyle::default())
        .collect();
    assert!(
        !semantic_only.is_empty(),
        "the OSC-133 case must produce at least one unstyled, mark-only cell — \
         otherwise this test is not exercising the asymmetry it claims to, got {cells:?}",
    );
    assert!(
        semantic_only.iter().all(|cell| cell.semantic.is_some()),
        "an unstyled server cell can only exist because of a semantic mark; \
         one without a mark means the sparse filter changed, got {semantic_only:?}",
    );

    // Every one of those cells is a glyph the dense frame DOES draw — the
    // asymmetry is in the annotation, not the content. Cross-checking the
    // grapheme is what proves the two projections are describing the same
    // screen while disagreeing about how much they can say about it.
    let frame = client_frame(case);
    for cell in &semantic_only {
        let dense = frame
            .cell(cell.row, cell.col)
            .expect("a server-marked cell is inside the dense frame");
        assert!(
            !dense.grapheme.is_empty(),
            "the dense frame must still draw the glyph at (row {}, col {}) that the \
             server annotated; only the mark is missing, not the cell",
            cell.row,
            cell.col,
        );
        assert_eq!(
            dense.style, cell.style,
            "the dense frame and the server must agree on the STYLE of a \
             mark-only cell at (row {}, col {}); only `semantic` is server-only",
            cell.row, cell.col,
        );
    }
}
