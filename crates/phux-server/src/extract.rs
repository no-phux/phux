//! Selection extraction: turn a [`Match`] coordinate span into the text under
//! it, via libghostty's sound one-shot selection-format API.
//!
//! # Why this exists (`phux-3sy` / `phux-97w`, the unblocked bridge)
//!
//! [`search`](crate::search) locates a literal needle in the rows phux
//! already mirrors and returns a [`Match`]: a [`Region`] plus a `row` and a
//! `col`/`len` span measured in `char` offsets into the *right-trimmed
//! projected text row* (the exact `String` rows
//! [`screen_state_with_scrollback`](crate::grid::SnapshotSynthesizer::screen_state_with_scrollback)
//! builds). That is the search layer's coordinate space, not libghostty's.
//!
//! This module is the copy/extract primitive the selection/copy-mode epic
//! (`phux-abi`) needs: it converts that `(Region, row, char-col, len)` span
//! into a [`libghostty_vt::selection::Selection`] over the terminal's grid
//! and formats just that range back out via the sound one-shot
//! `Terminal::format_selection_alloc` path. It is deliberately *only* the
//! find-coords -> selection -> extraction primitive: no copy-mode UI,
//! no cursor, no highlight.
//!
//! # Output formats
//!
//! The format-generic entry is [`extract`]: it takes a libghostty
//! [`Format`] (re-exported here) and returns raw bytes, because two of the
//! three formats are not plain text — [`Format::Plain`] strips styling,
//! [`Format::Vt`] preserves colors/styles/URLs as escape sequences, and
//! [`Format::Html`] emits inline-styled markup. The `String`-returning
//! helpers ([`extract_match`], [`extract_match_in_scope`],
//! [`extract_active_span`], [`extract_history_span`]) are thin plain-text
//! conveniences over the same span extraction.
//!
//! # The wide-glyph translation (the crux, 3sy's caveat)
//!
//! A `char` offset into the trimmed text row diverges from a grid column
//! whenever the row holds wide glyphs (CJK / emoji) or multi-`char` grapheme
//! clusters (a base codepoint plus combining marks). The projection (see
//! [`screen_state_with_scrollback`](crate::grid::SnapshotSynthesizer::screen_state_with_scrollback))
//! builds each row by
//! walking cells in grid order, skipping [`CellWide::SpacerTail`] cells (the
//! right half of a wide glyph emits no column), pushing each cell's whole
//! grapheme cluster as one-or-more `char`s, and advancing the grid column by
//! the cell's display width (2 for [`CellWide::Wide`], else 1). For example,
//! the row `你好xy` occupies grid columns 0 (wide `你`), 1 (its skipped
//! tail), 2 (wide `好`), 3 (its tail), 4 (`x`), 5 (`y`); its trimmed text is
//! the 4-`char` string `你好xy` whose char offsets 0,1,2,3 map to grid
//! columns 0,2,4,5.
//!
//! The private `char_col_to_grid_x` re-walks the same cells in the same order
//! to invert that mapping: given a target `char` offset it returns the grid column of
//! the cell that *contains* that offset. Mapping the cell's *left edge* is
//! correct for a wide glyph — a selection endpoint on the wide base covers
//! the whole glyph, and libghostty's `SpacerTail` is not independently
//! selectable.
//!
//! # Point space
//!
//! A [`Region::Viewport`] match maps to [`Point::Active`] coordinates (the
//! viewport walk projects active-area rows top-first, `y = row`).
//!
//! A [`Region::Scrollback`] match maps to [`Point::History`] coordinates. The
//! search layer reports the history `row` oldest-first into the *projected
//! window* it searched, which is the full retained history for
//! [`Scope::AllHistory`] and the most-recent slice for
//! [`Scope::RecentHistory`]. That window offset is one-to-one with an absolute
//! [`Point::History`] `y` once the [`Scope`] (and the live retained-row count)
//! are known, so [`extract_match_in_scope`] threads the scope and resolves it;
//! the scope-free [`extract_match`] rejects a scrollback match with
//! [`ExtractError::ScrollbackNeedsScope`] rather than guessing. The history and
//! viewport walks share one point-space-generic cell walk, so the
//! wide-glyph / combining-mark `char`-col-to-grid-col inversion is identical in
//! both.

use libghostty_vt::{
    Terminal as GhosttyTerminal,
    screen::CellWide,
    selection::{FormatOptions, Selection},
    terminal::{Point, PointCoordinate},
};

// Re-exported so consumers of the bridge can name the output format without
// reaching into libghostty_vt themselves.
pub use libghostty_vt::fmt::Format;

use crate::grid::{GRAPHEME_INLINE, SynthesisError};
use crate::search::{Match, Region, Scope};

/// Errors from translating a [`Match`] to a selection and extracting its text.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// Surfaced from the libghostty render/selection path.
    #[error("libghostty: {0}")]
    Synthesis(#[from] SynthesisError),
    /// Surfaced directly from a libghostty call (`grid_ref`, format).
    #[error("libghostty: {0}")]
    Ghostty(#[from] libghostty_vt::Error),
    /// The match's `char` column could not be resolved to a grid column —
    /// the row is shorter (in non-tail cells) than the reported `col`. This
    /// indicates the terminal mutated between [`search`](crate::search) and
    /// extraction, or a stale [`Match`].
    #[error("match column {col} is past the end of row {row} ({region:?})")]
    ColumnOutOfRange {
        /// The region the out-of-range match referenced.
        region: Region,
        /// The row index within that region.
        row: usize,
        /// The `char` column that fell past the row's last non-tail cell.
        col: usize,
    },
    /// A [`Region::Scrollback`] match was handed to the scope-free
    /// [`extract_match`], which cannot resolve the history row index to an
    /// absolute [`Point::History`] `y` without knowing the search
    /// [`Scope`] that produced it. Use
    /// [`extract_match_in_scope`] (which threads the scope) or resolve the
    /// absolute history `y` yourself and call [`extract_history_span`].
    #[error(
        "scrollback (history) extraction needs the search scope; use extract_match_in_scope or extract_history_span"
    )]
    ScrollbackNeedsScope,
    /// The history row the match referenced is no longer retained — the
    /// terminal's scrollback shrank (rows aged out, a resize/reflow, or an
    /// alt-screen clear) between [`search`](crate::search) and extraction, so
    /// the resolved absolute `y` is at or past the current
    /// `Terminal::scrollback_rows` count.
    #[error("history row {history_y} is past the current scrollback ({total} rows)")]
    HistoryRowOutOfRange {
        /// The absolute history `y` that fell outside the live scrollback.
        history_y: u32,
        /// The current retained-history row count.
        total: u32,
    },
}

/// Extract the content under a [`Match`] in the requested output [`Format`],
/// as raw bytes.
///
/// This is the format-generic entry of the find -> selection -> format bridge
/// (`phux-97w`): the match's `char`-offset span is translated to grid columns
/// (wide-glyph aware), a linear [`Selection`] is built from two
/// `Terminal::grid_ref` endpoints, and the range is formatted via the sound
/// one-shot `Terminal::format_selection_alloc` path in the chosen format:
///
/// - [`Format::Plain`]: the text with styling stripped (what
///   [`extract_match_in_scope`] returns as a `String`).
/// - [`Format::Vt`]: VT escape sequences preserving colors, styles, and URLs.
/// - [`Format::Html`]: markup with inline styles.
///
/// Bytes, not `String`, because the VT form deliberately contains escape
/// sequences and neither non-plain form is guaranteed to please a strict
/// UTF-8 round-trip through lossy conversion; callers that want plain text
/// should use the `String` helpers.
///
/// A [`Region::Scrollback`] match resolves its history row against `scope`
/// exactly as [`extract_match_in_scope`] does (and shares its
/// [`ExtractError::HistoryRowOutOfRange`] staleness check).
pub fn extract(
    terminal: &GhosttyTerminal<'_, '_>,
    m: Match,
    scope: Scope,
    format: Format,
) -> Result<Vec<u8>, ExtractError> {
    match m.region {
        Region::Viewport => {
            let y = u32::try_from(m.row).unwrap_or(u32::MAX);
            extract_span(terminal, PointSpace::Active, y, m.col, m.len, format)
                .map_err(|e| restamp_viewport(e, m.row))
        }
        Region::Scrollback => {
            let history_y = resolve_history_y(terminal, m.row, scope)?;
            extract_span(
                terminal,
                PointSpace::History,
                history_y,
                m.col,
                m.len,
                format,
            )
        }
    }
}

/// Extract the text under a [`Region::Viewport`] [`Match`] as a plain-text
/// `String`.
///
/// This is the scope-free entry. A [`Region::Viewport`] match maps its `row`
/// directly onto [`Point::Active`] `y`. A [`Region::Scrollback`] match returns
/// [`ExtractError::ScrollbackNeedsScope`]: the history row index is *relative*
/// to the search window, so resolving it to an absolute [`Point::History`] `y`
/// requires the [`Scope`] the search ran under — use [`extract_match_in_scope`].
///
/// The match's `char`-offset `col`/`len` are translated to grid columns via
/// the private `char_col_to_grid_x` (handling wide glyphs and multi-`char`
/// clusters), then a linear `Selection` is built over `[start_x, end_x]` of row `y`
/// and formatted with the sound one-shot
/// `Terminal::format_selection_alloc` API.
pub fn extract_match(terminal: &GhosttyTerminal<'_, '_>, m: Match) -> Result<String, ExtractError> {
    match m.region {
        Region::Viewport => {
            let y = u32::try_from(m.row).unwrap_or(u32::MAX);
            extract_active_span(terminal, y, m.col, m.len).map_err(|e| restamp_viewport(e, m.row))
        }
        Region::Scrollback => Err(ExtractError::ScrollbackNeedsScope),
    }
}

/// Extract the text under a [`Match`] as a plain-text `String`.
///
/// Unlike [`extract_match`], this resolves a [`Region::Scrollback`] match's
/// history row against the [`Scope`] the search ran under (`phux-3sy` /
/// `phux-97w`, the scrollback half of the bridge).
///
/// # The history-`y` resolution (why the scope is needed)
///
/// The search layer reports a scrollback `Match.row` as an index into the
/// *projected history window*, oldest-first — the exact rows
/// [`screen_state_with_scrollback`](crate::grid::SnapshotSynthesizer::screen_state_with_scrollback)
/// built. For [`Scope::AllHistory`] that window is every retained row
/// (`start = 0`), so `row` *is* the absolute [`Point::History`] `y`. For
/// [`Scope::RecentHistory`] with `n` the window is the most-recent `n` rows, the
/// slice `[total - min(n, total), total)`, so the absolute `y` is
/// `start + row`. Both starts mirror the
/// [`SnapshotSynthesizer`](crate::grid::SnapshotSynthesizer) scrollback walk
/// exactly, so the inversion is one-to-one given the scope and the live
/// `scrollback_rows()`.
///
/// `scrollback_rows()` is queried *now*, so a row that aged out of history
/// between search and extraction surfaces as
/// [`ExtractError::HistoryRowOutOfRange`] rather than a wrong selection.
pub fn extract_match_in_scope(
    terminal: &GhosttyTerminal<'_, '_>,
    m: Match,
    scope: Scope,
) -> Result<String, ExtractError> {
    extract(terminal, m, scope, Format::Plain).map(into_lossy_string)
}

/// Re-stamp a [`ExtractError::ColumnOutOfRange`] from active-space inputs back
/// onto the viewport match the caller passed, so the error names that match's
/// region/row rather than the raw active-area coordinates.
fn restamp_viewport(e: ExtractError, row: usize) -> ExtractError {
    match e {
        ExtractError::ColumnOutOfRange { col, .. } => ExtractError::ColumnOutOfRange {
            region: Region::Viewport,
            row,
            col,
        },
        other => other,
    }
}

/// Map a scrollback [`Match::row`] (an index into the search's projected
/// history window, oldest-first) onto an absolute [`Point::History`] `y`,
/// using the live retained-history count to anchor a bounded window.
///
/// See [`extract_match_in_scope`] for the window math. Returns
/// [`ExtractError::HistoryRowOutOfRange`] if the resolved `y` is past the
/// current scrollback (the row aged out since the search).
fn resolve_history_y(
    terminal: &GhosttyTerminal<'_, '_>,
    row: usize,
    scope: Scope,
) -> Result<u32, ExtractError> {
    let total = u32::try_from(terminal.scrollback_rows()?).unwrap_or(u32::MAX);
    let row = u32::try_from(row).unwrap_or(u32::MAX);
    // Window start mirrors `SnapshotSynthesizer::scrollback_lines`: all-history
    // starts at 0; a bounded request keeps the most-recent `n` rows.
    let start = match scope {
        Scope::AllHistory => 0,
        Scope::RecentHistory(n) => total.saturating_sub(n.min(total)),
    };
    let history_y = start.saturating_add(row);
    if history_y >= total {
        return Err(ExtractError::HistoryRowOutOfRange { history_y, total });
    }
    Ok(history_y)
}

/// Extract the text spanning `len` `char`s starting at `char` column
/// `char_col` of active-area row `y`, as plain text.
///
/// This is the point-space-explicit primitive [`extract_match`] delegates to
/// for the viewport case. `char_col`/`len` are `char` offsets into the same
/// right-trimmed projected text row the search layer reports; they are
/// translated to grid columns here.
///
/// A zero `len` extracts nothing and returns an empty string without touching
/// libghostty's selection path.
pub fn extract_active_span(
    terminal: &GhosttyTerminal<'_, '_>,
    y: u32,
    char_col: usize,
    len: usize,
) -> Result<String, ExtractError> {
    extract_span(
        terminal,
        PointSpace::Active,
        y,
        char_col,
        len,
        Format::Plain,
    )
    .map(into_lossy_string)
}

/// Extract the text spanning `len` `char`s starting at `char` column
/// `char_col` of *history* (scrollback) row `history_y`, as plain text.
///
/// This is the [`Point::History`] analog of [`extract_active_span`]. `history_y`
/// is an absolute index into the retained scrollback in libghostty's history
/// coordinate space (`y = 0` is the oldest retained row), the same space the
/// [`SnapshotSynthesizer`](crate::grid::SnapshotSynthesizer) scrollback walk
/// reads — see [`extract_match_in_scope`] for how a search's relative
/// scrollback `row` resolves to this absolute `y`. `char_col`/`len` are `char`
/// offsets into the right-trimmed projected history row text the search layer
/// reports.
///
/// A zero `len` extracts nothing and returns an empty string without touching
/// libghostty's selection path.
pub fn extract_history_span(
    terminal: &GhosttyTerminal<'_, '_>,
    history_y: u32,
    char_col: usize,
    len: usize,
) -> Result<String, ExtractError> {
    extract_span(
        terminal,
        PointSpace::History,
        history_y,
        char_col,
        len,
        Format::Plain,
    )
    .map(into_lossy_string)
}

/// Decode formatted bytes for the plain-text `String` helpers, consuming the
/// buffer without a copy in the (expected) valid-UTF-8 case.
///
/// Only ever applied to [`Format::Plain`] output, where non-UTF-8 can only
/// arise from grid corruption; the lossy replacement character is the honest
/// rendering of that, matching the projection layer's behavior.
fn into_lossy_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Which point space [`extract_span`] walks/selects in. Active is the live
/// viewport; History is the scrollback above it.
#[derive(Clone, Copy)]
enum PointSpace {
    Active,
    History,
}

impl PointSpace {
    /// Build the [`Point`] for `(x, y)` in this space.
    const fn point(self, x: u16, y: u32) -> Point {
        let coord = PointCoordinate { x, y };
        match self {
            Self::Active => Point::Active(coord),
            Self::History => Point::History(coord),
        }
    }

    /// The [`Region`] this space corresponds to, for error stamping.
    const fn region(self) -> Region {
        match self {
            Self::Active => Region::Viewport,
            Self::History => Region::Scrollback,
        }
    }
}

/// Shared span extraction over a [`PointSpace`]: translate `char` offsets to
/// grid columns, build a linear [`Selection`] over `[start_x, end_x]` of row
/// `y`, and format it in `format` with the sound one-shot
/// [`libghostty_vt::Terminal::format_selection_alloc`] API.
fn extract_span(
    terminal: &GhosttyTerminal<'_, '_>,
    space: PointSpace,
    y: u32,
    char_col: usize,
    len: usize,
    format: Format,
) -> Result<Vec<u8>, ExtractError> {
    if len == 0 {
        return Ok(Vec::new());
    }

    let out_of_range = |col: usize| ExtractError::ColumnOutOfRange {
        region: space.region(),
        row: usize::try_from(y).unwrap_or(usize::MAX),
        col,
    };

    // Translate the start and the (inclusive) last char of the span to grid
    // columns. The selection endpoints are inclusive (see `Selection::new`),
    // so the end maps the last char, `char_col + len - 1`.
    let start_x =
        char_col_to_grid_x(terminal, space, y, char_col)?.ok_or_else(|| out_of_range(char_col))?;
    let last_char = char_col + len - 1;
    let end_x = char_col_to_grid_x(terminal, space, y, last_char)?
        .ok_or_else(|| out_of_range(last_char))?;

    let start = terminal.grid_ref(space.point(start_x, y))?;
    let end = terminal.grid_ref(space.point(end_x, y))?;
    let selection = Selection::new(start, end, false);

    let bytes = terminal
        .format_selection_alloc(
            None,
            FormatOptions::new()
                .with_emit_format(format)
                .with_trim(true)
                .with_unwrap(true)
                .with_selection(&selection),
        )?
        // A non-empty selection over real cells yields Some(bytes); None
        // means libghostty saw nothing selectable in the range, which we
        // surface as empty output rather than an error.
        .map(|b| b.to_vec())
        .unwrap_or_default();
    Ok(bytes)
}

/// Map a `char` offset into row `y`'s right-trimmed projected text (in the
/// given [`PointSpace`] — viewport active area or scrollback history) to the
/// grid column of the cell that contains it.
///
/// Walks the row's cells in grid order exactly as the text projection does:
/// skip [`CellWide::SpacerTail`] cells (they emit no `char` and own no
/// column), count each visited cell's grapheme-cluster `char`s, and advance
/// the grid column by the cell's display width (2 for [`CellWide::Wide`],
/// else 1). The first cell whose accumulated `char` range covers `target` is
/// the answer, and its *left* grid column is returned (a wide glyph's base
/// column, not its skipped tail).
///
/// Returns `Ok(None)` when `target` lands at or past the row's last non-tail
/// cell — the row's projected text is shorter than `target + 1` `char`s.
///
/// This mirrors the projection's right-trim only implicitly: trailing blank
/// cells contribute a single space `char` each here, but a `target` inside
/// the *trimmed* text always falls on a real (non-trailing-blank) cell, so
/// the trim never changes the answer for an in-range `target`. An empty
/// (blank) cell emits one space `char` and one column, matching the
/// projection's `buf.push(' ')`.
fn char_col_to_grid_x(
    terminal: &GhosttyTerminal<'_, '_>,
    space: PointSpace,
    y: u32,
    target: usize,
) -> Result<Option<u16>, ExtractError> {
    let cols = terminal.cols()?;
    let mut char_acc = 0usize;
    let mut grid_x = 0u16;

    while grid_x < cols {
        let point = space.point(grid_x, y);
        let grid_ref = terminal.grid_ref(point)?;
        let wide = grid_ref.cell()?.wide()?;
        if matches!(wide, CellWide::SpacerTail) {
            // Right half of a wide glyph: emits no char and owns no column in
            // the projection. Advance one grid column without counting chars.
            grid_x += 1;
            continue;
        }

        // How many `char`s this cell contributes to the projected text. A
        // blank cell contributes exactly one (a space).
        let cluster_chars = grapheme_char_count(&grid_ref)?.max(1);
        if target < char_acc + cluster_chars {
            // `target` falls within this cell's grapheme cluster; the cell's
            // left grid column is the selection endpoint.
            return Ok(Some(grid_x));
        }
        char_acc += cluster_chars;

        grid_x = grid_x.saturating_add(if matches!(wide, CellWide::Wide) { 2 } else { 1 });
    }

    Ok(None)
}

/// Count the `char`s in the grapheme cluster at `grid_ref`, with a stack
/// buffer fast path and a heap retry for deep clusters — mirroring the
/// scrollback projection's read.
fn grapheme_char_count(
    grid_ref: &libghostty_vt::screen::GridRef<'_>,
) -> Result<usize, ExtractError> {
    let mut inline = [char::from(0u8); GRAPHEME_INLINE];
    match grid_ref.graphemes(&mut inline) {
        Ok(n) => Ok(n),
        Err(libghostty_vt::Error::OutOfSpace { required }) => {
            let mut heap = vec![char::from(0u8); required];
            Ok(grid_ref.graphemes(&mut heap)?)
        }
        Err(err) => Err(err.into()),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use crate::search::{Scope, SearchOptions, search_oneshot};
    use libghostty_vt::{Terminal as GhosttyTerminal, TerminalOptions};

    fn fresh(cols: u16, rows: u16) -> GhosttyTerminal<'static, 'static> {
        GhosttyTerminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: 100,
        })
        .expect("Terminal::new")
    }

    fn vp() -> SearchOptions {
        SearchOptions {
            case_insensitive: false,
            include_viewport: true,
        }
    }

    #[test]
    fn extracts_plain_ascii_match_text() {
        // (a) A plain-ASCII match round-trips its exact text: search finds
        // the needle, extraction pulls back the same bytes.
        let mut t = fresh(40, 3);
        t.vt_write(b"the quick brown fox");
        let hits = search_oneshot(&t, "brown", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        let text = extract_match(&t, hits[0]).expect("extract");
        assert_eq!(text, "brown", "the extracted text matches the needle");
    }

    #[test]
    fn extracts_whole_word_with_neighbours() {
        // A longer span (two words) round-trips, proving the inclusive
        // end-column math (char_col + len - 1) covers the full range.
        let mut t = fresh(40, 3);
        t.vt_write(b"alpha bravo charlie delta");
        let hits = search_oneshot(&t, "bravo charlie", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        let text = extract_match(&t, hits[0]).expect("extract");
        assert_eq!(text, "bravo charlie");
    }

    #[test]
    fn wide_glyph_before_match_shifts_grid_column() {
        // (b) A wide glyph BEFORE the match: the char offset and the grid
        // column diverge, and extraction must use the grid column.
        //
        // Row "你好xTODO": grid columns 0(你 wide),1(tail),2(好 wide),3(tail),
        // 4(x),5(T),6(O),7(D),8(O). Projected text is "你好xTODO" — char
        // offsets 0(你),1(好),2(x),3(T)... So "TODO" is at CHAR offset 3 but
        // GRID column 5. If extraction used the char offset (3) as the grid
        // x, the selection would start on 好's tail / x and the text would be
        // wrong. The grid mapping must put the start at grid column 5.
        let mut t = fresh(40, 3);
        t.vt_write("你好xTODO".as_bytes());

        let hits = search_oneshot(&t, "TODO", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        let m = hits[0];
        assert_eq!(m.col, 3, "char offset of TODO is 3 (你, 好, x precede it)");

        // Direct grid-column translation: char offset 3 -> grid column 5.
        let start_x = char_col_to_grid_x(&t, PointSpace::Active, 0, 3)
            .expect("translate")
            .expect("in range");
        assert_eq!(
            start_x, 5,
            "char offset 3 maps to grid column 5, not 3 (two wide glyphs precede it)"
        );

        let text = extract_match(&t, m).expect("extract");
        assert_eq!(
            text, "TODO",
            "the wide-glyph-aware grid mapping extracts the right text"
        );
    }

    #[test]
    fn extracts_match_that_includes_a_wide_glyph() {
        // The needle itself spans a wide glyph: "好x" is char offsets 1..=2
        // but grid columns 2 and 4 (好 is wide). The inclusive end must land
        // on grid column 4 (x), so the selection covers 好 (cols 2-3) and x.
        let mut t = fresh(40, 3);
        t.vt_write("你好x".as_bytes());
        let hits = search_oneshot(&t, "好x", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        let text = extract_match(&t, hits[0]).expect("extract");
        assert_eq!(text, "好x");
    }

    #[test]
    fn multi_row_region_extracts_each_row() {
        // (c) A multi-row selection: extract a span on a second viewport row,
        // proving the row->y mapping is per-row (y = match.row).
        let mut t = fresh(40, 4);
        t.vt_write(b"first line here\r\nsecond MARKER line");
        let hits = search_oneshot(&t, "MARKER", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        let m = hits[0];
        assert_eq!(m.region, Region::Viewport);
        assert_eq!(m.row, 1, "MARKER is on the second viewport row");
        let text = extract_match(&t, m).expect("extract");
        assert_eq!(text, "MARKER");
    }

    #[test]
    fn match_to_extract_round_trips_searched_content() {
        // The evolved 97w pin-test: a real Match -> extract round-trip over
        // content the search actually found, not a hardcoded sub-range. Every
        // hit's extracted text equals the searched needle.
        let mut t = fresh(40, 3);
        t.vt_write(b"needle once, needle twice, needle thrice");
        let hits = search_oneshot(&t, "needle", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 3, "three hits on the row");
        for m in hits {
            let text = extract_match(&t, m).expect("extract");
            assert_eq!(text, "needle", "each searched hit extracts its needle");
        }
    }

    #[test]
    fn scope_free_extract_rejects_scrollback() {
        // The scope-free entry cannot resolve a history row without the
        // search scope, so it returns the precise ScrollbackNeedsScope error
        // rather than guessing a y.
        let mut t = fresh(20, 2);
        t.vt_write(b"alpha\r\nbravo\r\ncharlie\r\ndelta\r\necho");
        let hits = search_oneshot(&t, "bravo", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].region, Region::Scrollback);
        let err = extract_match(&t, hits[0]).expect_err("scrollback needs scope");
        assert!(matches!(err, ExtractError::ScrollbackNeedsScope));
    }

    #[test]
    fn scrollback_match_extracts_all_history() {
        // The scrollback half of the bridge: an AllHistory search hit in
        // scrollback round-trips its text via extract_match_in_scope. "bravo"
        // is history row 1 (alpha=0, bravo=1, charlie=2 above a 2-row vp).
        let mut t = fresh(20, 2);
        t.vt_write(b"alpha\r\nbravo\r\ncharlie\r\ndelta\r\necho");
        let hits = search_oneshot(&t, "bravo", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].region, Region::Scrollback);
        let text =
            extract_match_in_scope(&t, hits[0], Scope::AllHistory).expect("scrollback extract");
        assert_eq!(text, "bravo", "history match round-trips its text");
    }

    #[test]
    fn scrollback_match_extracts_under_recent_window() {
        // A bounded RecentHistory search reports the row as an offset into the
        // most-recent window; the absolute history y must be reconstructed
        // from the window start (a naive `y = row` would read from the wrong,
        // older row). 3 history rows (mark1,mark2,mark3); bound to the last 2
        // so the window is {mark2 at window-row 0 -> abs y=1, mark3 at
        // window-row 1 -> abs y=2}. Searching for each distinct full token and
        // extracting it back proves the window offset resolves to the right
        // absolute history row (not to mark1 at abs y=0).
        let mut t = fresh(20, 2);
        t.vt_write(b"mark1\r\nmark2\r\nmark3\r\nplain4\r\nplain5");
        assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 3);
        let scope = Scope::RecentHistory(2);
        let opts = SearchOptions {
            case_insensitive: false,
            include_viewport: false,
        };

        // mark2 is at window-row 0 (abs y=1). The hit's char span is the full
        // "mark2", so extraction must return exactly that — and reading abs
        // y=1 (not y=0, where "mark1" lives) is what proves the offset.
        let hits2 = search_oneshot(&t, "mark2", scope, opts).expect("search mark2");
        assert_eq!(hits2.len(), 1, "mark2 is in the recent window");
        assert_eq!(hits2[0].region, Region::Scrollback);
        assert_eq!(hits2[0].row, 0, "mark2 is window-row 0 of the recent slice");
        assert_eq!(
            extract_match_in_scope(&t, hits2[0], scope).expect("extract mark2"),
            "mark2",
            "window-row 0 resolves to abs history y=1 (mark2), not y=0 (mark1)",
        );

        // mark3 is at window-row 1 (abs y=2).
        let hits3 = search_oneshot(&t, "mark3", scope, opts).expect("search mark3");
        assert_eq!(hits3.len(), 1, "mark3 is in the recent window");
        assert_eq!(hits3[0].row, 1, "mark3 is window-row 1 of the recent slice");
        assert_eq!(
            extract_match_in_scope(&t, hits3[0], scope).expect("extract mark3"),
            "mark3",
            "window-row 1 resolves to abs history y=2 (mark3)",
        );

        // mark1 is OUTSIDE the recent-2 window, so it is not found at all —
        // the bound really excludes the oldest row.
        let hits1 = search_oneshot(&t, "mark1", scope, opts).expect("search mark1");
        assert!(hits1.is_empty(), "mark1 is outside the recent-2 window");
    }

    #[test]
    fn scrollback_match_with_wide_glyph_round_trips() {
        // The history walk must apply the same wide-glyph char-col->grid-col
        // inversion as the viewport. Row "你好xKEEP" in scrollback: "KEEP" is
        // char offset 3 but grid column 5 (two wide glyphs precede). The
        // history extraction must use the grid column, not the char offset.
        let mut t = fresh(20, 2);
        // Push the wide-glyph row into scrollback with two more lines.
        t.vt_write("你好xKEEP\r\n".as_bytes());
        t.vt_write(b"filler-a\r\nfiller-b");
        let hits = search_oneshot(&t, "KEEP", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].region,
            Region::Scrollback,
            "the wide row is history"
        );
        assert_eq!(hits[0].col, 3, "KEEP is char offset 3 (你,好,x precede)");
        let text =
            extract_match_in_scope(&t, hits[0], Scope::AllHistory).expect("scrollback extract");
        assert_eq!(
            text, "KEEP",
            "wide-glyph-aware history mapping extracts the right text",
        );
    }

    #[test]
    fn scrollback_row_aged_out_errors() {
        // If history shrinks between search and extraction, an absolute y
        // past the live scrollback surfaces HistoryRowOutOfRange rather than
        // a wrong selection. row 100 in an AllHistory scope is far past the
        // 3 retained rows.
        let mut t = fresh(20, 2);
        t.vt_write(b"alpha\r\nbravo\r\ncharlie\r\ndelta\r\necho");
        let stale = Match {
            region: Region::Scrollback,
            row: 100,
            col: 0,
            len: 1,
        };
        let err =
            extract_match_in_scope(&t, stale, Scope::AllHistory).expect_err("aged-out history");
        assert!(matches!(
            err,
            ExtractError::HistoryRowOutOfRange { total: 3, .. }
        ));
    }

    #[test]
    fn extract_plain_format_matches_string_helper() {
        // The format-generic entry with Format::Plain yields the same bytes
        // the plain-text String helper decodes, on both region paths.
        let mut t = fresh(20, 2);
        t.vt_write(b"alpha\r\nbravo\r\ncharlie\r\ndelta\r\necho bravo");
        let hits = search_oneshot(&t, "bravo", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 2, "one history hit, one viewport hit");
        for m in hits {
            let bytes = extract(&t, m, Scope::AllHistory, Format::Plain).expect("extract bytes");
            let text = extract_match_in_scope(&t, m, Scope::AllHistory).expect("extract text");
            assert_eq!(bytes, text.as_bytes(), "plain bytes == String helper");
            assert_eq!(text, "bravo");
        }
    }

    #[test]
    fn vt_format_preserves_styling_plain_strips_it() {
        // A red-SGR needle: Plain extraction strips the styling to the bare
        // text; Vt extraction preserves it as escape sequences carrying the
        // same text.
        let mut t = fresh(40, 3);
        t.vt_write(b"ok \x1b[31mALERT\x1b[0m done");
        let hits = search_oneshot(&t, "ALERT", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1, "styling must not perturb the projected text");
        let m = hits[0];

        let plain = extract(&t, m, Scope::AllHistory, Format::Plain).expect("plain");
        assert_eq!(plain, b"ALERT", "plain output is the bare text");

        let vt = extract(&t, m, Scope::AllHistory, Format::Vt).expect("vt");
        let vt_text = String::from_utf8_lossy(&vt).into_owned();
        assert!(
            vt_text.contains("ALERT"),
            "VT output carries the text, got {vt_text:?}",
        );
        assert!(
            vt.contains(&0x1b),
            "VT output preserves escape sequences for the red SGR, got {vt_text:?}",
        );
        assert!(
            vt_text.contains("31"),
            "the red (SGR 31) styling survives VT extraction, got {vt_text:?}",
        );
    }

    #[test]
    fn html_format_emits_markup() {
        let mut t = fresh(40, 3);
        t.vt_write(b"pre \x1b[1mBOLDBIT\x1b[0m post");
        let hits = search_oneshot(&t, "BOLDBIT", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        let html = extract(&t, hits[0], Scope::AllHistory, Format::Html).expect("html");
        let html_text = String::from_utf8_lossy(&html).into_owned();
        assert!(
            html_text.contains("BOLDBIT"),
            "HTML output carries the text, got {html_text:?}",
        );
        assert!(
            html_text.contains('<'),
            "HTML output is markup, got {html_text:?}",
        );
    }

    #[test]
    fn vt_format_on_scrollback_match() {
        // The format dimension composes with the history-y resolution: a
        // styled needle pushed into scrollback extracts as VT (text + escape
        // bytes) through the same scope-threaded path as plain text.
        let mut t = fresh(20, 2);
        t.vt_write(b"\x1b[32mGREENLIT\x1b[0m\r\nfiller-a\r\nfiller-b");
        let hits = search_oneshot(&t, "GREENLIT", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].region,
            Region::Scrollback,
            "the styled row is history"
        );
        let vt = extract(&t, hits[0], Scope::AllHistory, Format::Vt).expect("vt");
        let vt_text = String::from_utf8_lossy(&vt).into_owned();
        assert!(vt_text.contains("GREENLIT"), "got {vt_text:?}");
        assert!(
            vt.contains(&0x1b),
            "history VT extraction preserves styling, got {vt_text:?}",
        );
    }

    #[test]
    fn format_entry_zero_len_is_empty() {
        // Zero-length spans short-circuit to empty output in every format
        // without touching the selection path.
        let t = fresh(20, 2);
        let m = Match {
            region: Region::Viewport,
            row: 0,
            col: 0,
            len: 0,
        };
        for format in [Format::Plain, Format::Vt, Format::Html] {
            let bytes = extract(&t, m, Scope::AllHistory, format).expect("extract");
            assert!(bytes.is_empty(), "{format:?} zero-len span is empty");
        }
    }

    #[test]
    fn out_of_range_column_errors() {
        // A col past the row's last non-tail cell surfaces ColumnOutOfRange
        // rather than panicking or silently truncating.
        let t = fresh(40, 3);
        // Empty viewport row 0: every cell is blank (one space char each), so
        // char offset 0 maps to grid column 0, but offset 100 (past the 40
        // columns) is out of range.
        let err = extract_active_span(&t, 0, 100, 1).expect_err("past row end");
        assert!(matches!(
            err,
            ExtractError::ColumnOutOfRange { col: 100, .. }
        ));
    }
}
