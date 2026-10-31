//! Conformance: the three libghostty cell projections must agree.
//!
//! The snapshot-to-cell projection exists in `phux-client`'s renderer (the
//! human's glass, `snapshot --rendered`), `phux-server`'s synthesizer (what an
//! agent reads, `snapshot --cells`), and `phux-record`'s replayer (what a
//! recording exports). Hoisting it into `phux-core` would give the domain crate
//! a libghostty dependency, so it stays triplicated and this file feeds one VT
//! corpus through all three and compares cell for cell.
//!
//! `RenderState::update` consumes the terminal's dirty bits, so each
//! projection gets its own private `Terminal` fed the same bytes; never point
//! two render states at one terminal here.
//!
//! Two asymmetries are structural and asserted: the server projection is
//! sparse (no default cells, no wide-glyph tail) while the other two are dense
//! ([`dense_as_screen_state`] bridges them), and only the server carries
//! OSC-133 semantics.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use libghostty_vt::Terminal as GhosttyTerminal;
use phux_core::screen::{
    CellColor, CellInfo, CellStyle, RenderedFrame, SCHEMA_VERSION, ScreenState, SemanticContent,
};
use phux_record::Replayer;
use phux_terminal::grid::SnapshotSynthesizer;
use phux_tui::attach::render::{ReplicaWalk, TerminalRenderer};

mod corpus;
use corpus::{CORPUS, Case, MAX_SCROLLBACK};

/// A fresh terminal with `case`'s bytes written in; every caller gets its own
/// (see the dirty-bit rule in the module docs).
fn fed_terminal(case: &Case) -> GhosttyTerminal<'static, 'static> {
    let mut term = {
        let mut terminal =
            GhosttyTerminal::new(case.cols, case.rows).expect("terminal construction");
        terminal
            .set_scrollback_max_lines(Some(MAX_SCROLLBACK))
            .expect("terminal construction");
        terminal
    };
    term.vt_write(case.bytes.as_bytes());
    term
}

/// Projection 1: the client's dense `RenderedFrame` (`snapshot --rendered`),
/// single-pane at origin; multi-pane offsets are the compositor's concern.
fn client_frame(case: &Case) -> RenderedFrame {
    let term = fed_terminal(case);
    let mut renderer = TerminalRenderer::new().expect("terminal renderer");
    let mut frame = RenderedFrame::blank(case.cols, case.rows);
    // With one pane the cursor election is the identity, and the walk-identity
    // token is a fixed constant (one terminal, walked once).
    frame.cursor = renderer
        .render_at_cells(
            ReplicaWalk::for_test(&term),
            &mut frame,
            (0, 0),
            (case.cols, case.rows),
        )
        .expect("render_at_cells");
    frame
}

/// Projection 2: the server's sparse `ScreenState` (`snapshot --cells`).
fn server_state(case: &Case) -> ScreenState {
    let term = fed_terminal(case);
    let mut synth = SnapshotSynthesizer::new().expect("snapshot synthesizer");
    synth
        .screen_state_with_scrollback(&term, 0, None, true)
        .expect("screen_state_with_scrollback")
}

/// Projection 3: the recorder's dense `RenderedFrame` (`phux rec`).
fn replay_frame(case: &Case) -> RenderedFrame {
    let mut replayer = Replayer::new(case.cols, case.rows).expect("replayer");
    replayer.feed(case.bytes.as_bytes());
    replayer
        .sample()
        .expect("sample")
        // The first sample always yields a frame (`settled_empty` guards it).
        .expect("the first sample always yields a frame")
        .frame
}

/// Bridge the dense shape to the sparse one: the `ScreenState` the server
/// must produce for `frame`. Lines are graphemes concatenated and
/// right-trimmed (tails `""`, blanks `" "`); cells are emitted only for a
/// non-default style and never for a tail, with `semantic` always `None` (see
/// [`styled_cells_only`]); columns already coincide with the server's
/// `col_index`.
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

/// Reduce the server's cells to what a dense frame can express: styled cells,
/// semantic mark stripped. OSC-133 emits cells for plain marked glyphs that
/// the glass cannot show; any styled server cell still must match exactly.
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

/// The two dense projections must be identical, whole struct and all (the
/// strongest comparison; read it first on a failure).
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

/// The recorder satisfies the server bridge too, so the triangle stays
/// explicit even if the client comparison weakens.
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

/// A 256-palette color stays a palette index in all three: re-themed
/// terminals and per-theme exports depend on it, and flattening to RGB would
/// still look plausible.
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

/// A wide glyph's tail is `""` in dense projections (preserving
/// `cells[row * cols + col]` indexing) and absent from the sparse one.
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

/// OSC-133 semantic marks are the server's alone: a rendered frame shows
/// what the glass shows, and a mark shows nothing.
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

    // The marked `ls -l` input glyphs are plain text, present in the sparse
    // projection only because of their mark.
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

    // The dense frame draws the same glyphs: the asymmetry is in annotation,
    // not content.
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
