//! Find-in-scrollback: a thin literal search over the server's existing
//! scrollback mirror.
//!
//! # Scope (`phux-3sy`)
//!
//! This is the *only* copy-mode-adjacent primitive phux owns. Per
//! CONTRIBUTING ("Things we will not accept" / "A homegrown selection
//! engine"), word/line/output **selection** and text **extraction**
//! delegate to the host terminal and to libghostty-vt's Selection +
//! Formatter APIs. phux builds nothing there. What phux *does* own is
//! locating a literal needle in the rows it already mirrors for
//! [`ScreenState`](phux_core::screen::ScreenState) — libghostty exposes no
//! search or regex, so this layer is genuinely ours to build.
//!
//! The search runs over the same text rows
//! [`SnapshotSynthesizer::screen_state_with_scrollback`] projects: the
//! history rows above the viewport (read side-effect-free via
//! `Point::History`) followed by the live viewport rows. It returns match
//! coordinates — a [`Region`] discriminant plus row/column spans in that
//! region's own coordinate space. Those coordinates are what a consumer
//! would hand to a libghostty `Selection` + `Formatter` to pull the matched
//! text, VT, or HTML back out. This module owns *finding* only; it does not
//! highlight, maintain a cursor, or extract — extraction is
//! [`crate::extract`]'s job; see the note below.
//!
//! ## Selection + extraction: the delegation boundary, and the unblocked bridge
//!
//! Per CONTRIBUTING ("A homegrown selection engine"), word/line/output
//! *boundary* logic and text *extraction* belong to the host terminal and to
//! libghostty. phux owns exactly one copy-mode-adjacent primitive: the
//! literal find-in-scrollback below. The remaining piece is a thin,
//! fully-safe extraction bridge — turn a [`Match`]'s coordinate span into a
//! [`libghostty_vt::selection::Selection`] and format just that range to text.
//!
//! The *types* for that bridge are all public at pin `8e1b0f7`: `Selection`
//! is built from two [`GridRef`](libghostty_vt::screen::GridRef) endpoints via
//! [`Selection::new`](libghostty_vt::selection::Selection::new),
//! `Terminal::grid_ref` yields them, and the one-shot
//! `Terminal::format_selection_alloc` (from libghostty)
//! formats a borrowed selection to bytes. The OSC-133-aware boundary helpers
//! (`select_word` / `select_line` / `select_output`) now have safe wrappers on
//! `Terminal`, but those are the boundary engine we deliberately refuse to
//! reimplement, so phux leaving them to the host terminal is by choice.
//!
//! Use the one-shot API, not `Formatter { selection: Some(_) }`. At pin
//! `acc4b87` the only selection-aware entry was `Formatter::new` with
//! `FormatterOptions::selection: Some(_)`, and `Formatter::new_inner` stored
//! `&raw const s` — the address of a match-arm-local copy of the
//! (`Copy`) `ffi::Selection` — which dropped before
//! `ghostty_formatter_terminal_new` read through it, so libghostty returned
//! `InvalidValue`. Pin `8e1b0f7` *adds* the sound one-shot selection API
//! (`Terminal::format_selection_alloc` / `format_selection_buf` /
//! `FormatOptions`), which holds the `Selection` *by borrow* across the FFI
//! call. That path round-trips; the extraction bridge uses it.
//! `safe_api_selection_formatter_bridge_round_trips_at_pin` proves it.
//!
//! The `Formatter::new` selection path is still unsound at `8e1b0f7` (it now
//! stores `&s.inner` where `s` is moved out of the by-value `Option` in the
//! match arm, so the borrow still dangles); phux must not use it. Fixing it
//! upstream — or porting phux to the one-shot API throughout — is tracked as a
//! follow-up.
//!
//! The remaining piece — *coordinate translation* from a [`Match`]'s
//! `char`-offset `col`/`len` (into the right-trimmed projected text row) onto
//! the terminal's grid columns within a
//! [`Point`](libghostty_vt::terminal::Point) space — now lives in
//! [`crate::extract`]. Wide glyphs make the `char` offset and the grid column
//! diverge (3sy's caveat); `extract::char_col_to_grid_x` re-walks the row's
//! cells to invert that mapping, and `extract::extract_match` builds the
//! [`Selection`](libghostty_vt::selection::Selection) and formats it. The
//! viewport/active case is complete; the
//! scrollback (history) point-space mapping is reported there as
//! unimplemented rather than shipped with a guessed `y`.

use crate::grid::{SnapshotSynthesizer, SynthesisError};
use libghostty_vt::Terminal as GhosttyTerminal;
// `Selection`, `Formatter`, `Point`, and friends are imported only inside the
// test module that proves the selection -> Formatter extraction path is sound;
// see `safe_api_selection_formatter_bridge_round_trips_at_pin`.

/// Which region of the mirrored screen a [`Match`] falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// A history row above the active viewport. `row` is the index into the
    /// projected scrollback rows, oldest-first (`row = 0` is the oldest
    /// retained row), matching
    /// [`ScreenState::scrollback`](phux_core::screen::ScreenState::scrollback).
    Scrollback,
    /// A live viewport row. `row` is the zero-based viewport row, top-first,
    /// matching [`ScreenState::lines`](phux_core::screen::ScreenState::lines).
    Viewport,
}

/// One literal-needle hit in the mirrored screen.
///
/// Coordinates are in the [`Region`]'s own row space. `col` and `len` are
/// measured in Unicode scalar values (`char`s) within the right-trimmed row
/// text, **not grid columns** or bytes — they index the same `String` rows
/// [`SnapshotSynthesizer::screen_state_with_scrollback`] projects, so a
/// consumer can slice the row directly.
///
/// # `col`/`len` are char offsets, not grid columns (`phux-4xi`)
///
/// A `char` offset and a grid column diverge whenever a row holds a wide glyph
/// (CJK / emoji, which occupies two grid columns) or a multi-`char` grapheme
/// cluster *before* the match: each preceding wide glyph shifts the grid column
/// one past the `char` offset. A consumer that builds a libghostty
/// [`Selection`](libghostty_vt::selection::Selection) — which is **grid-column**
/// based — must therefore re-walk the row's cells to invert the mapping rather
/// than passing `col` straight through as a grid `x`.
///
/// That inversion is the *consumer's* job, and the in-tree consumer already
/// does it: [`crate::extract`] (`char_col_to_grid_x` /
/// [`extract_match`](crate::extract::extract_match)) re-walks the same cells in
/// the same grid order the projection used, skipping wide-glyph spacer tails and
/// advancing by display width, so a hit on a row with a wide glyph before it
/// extracts the correct text. `Match` deliberately does **not** carry a
/// redundant grid column: the only consumer already remaps at the extraction
/// boundary (and proves it — see `extract`'s
/// `wide_glyph_before_match_shifts_grid_column` and
/// `scrollback_match_with_wide_glyph_round_trips`), so a second field would be
/// dead weight to keep in sync. Add one only if a consumer appears that needs
/// the raw grid column without the cell walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Region the match falls in.
    pub region: Region,
    /// Row index within `region`.
    pub row: usize,
    /// Start column of the match, in `char` offsets into the right-trimmed row
    /// text — **not** a grid column. Remap via the row's cell walk (see the
    /// type-level note and [`crate::extract`]) before using it as a libghostty
    /// grid `x`.
    pub col: usize,
    /// Length of the match, in `char`s (not grid columns).
    pub len: usize,
}

/// How a [`search`] needle is matched against row text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOptions {
    /// Match without regard to ASCII case. Non-ASCII characters are compared
    /// verbatim (a full Unicode case fold is out of scope for a literal
    /// find).
    pub case_insensitive: bool,
    /// Include the live viewport rows in the search. When `false`, only the
    /// scrollback history above the viewport is searched.
    pub include_viewport: bool,
}

/// How much scrollback history a [`search`] covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Search every retained history row (plus the viewport, per
    /// [`SearchOptions::include_viewport`]).
    AllHistory,
    /// Search the most-recent `n` history rows (those nearest the viewport),
    /// plus the viewport per [`SearchOptions::include_viewport`].
    RecentHistory(u32),
}

/// Find every occurrence of `needle` in `terminal`'s mirrored scrollback
/// (and optionally its viewport), oldest match first.
///
/// This is a literal substring search — libghostty exposes no regex, and we
/// add none here. An empty `needle` yields no matches. Overlapping matches
/// on a single row are not reported: after a hit the scan resumes past the
/// match, mirroring `str::match_indices` semantics.
///
/// The read is side-effect-free: it reuses
/// [`SnapshotSynthesizer::screen_state_with_scrollback`], which neither
/// scrolls the viewport nor mutates `terminal`, so it is safe to poll
/// against a live pane.
///
/// To extract the content under a returned [`Match`], hand it to
/// [`crate::extract`] — [`extract_match_in_scope`](crate::extract::extract_match_in_scope)
/// for plain text, or the format-generic
/// [`extract`](crate::extract::extract) for text/VT/HTML bytes.
///
/// The synthesizer and terminal share the libghostty allocator lifetime
/// `'alloc`: the pooled render iterators inside `synth` borrow from the same
/// arena the `terminal` was built in (mirroring
/// [`SnapshotSynthesizer::screen_state_with_scrollback`]).
pub fn search<'alloc>(
    synth: &SnapshotSynthesizer<'alloc>,
    terminal: &GhosttyTerminal<'alloc, '_>,
    needle: &str,
    scope: Scope,
    opts: SearchOptions,
) -> Result<Vec<Match>, SynthesisError> {
    if needle.is_empty() {
        return Ok(Vec::new());
    }

    let scrollback = match scope {
        Scope::AllHistory => Some(crate::grid::SCROLLBACK_ALL),
        Scope::RecentHistory(n) => Some(n),
    };

    // Reuse the existing structured projection so the wide-cell, history,
    // and right-trim handling stays in one place. `cells = false` keeps the
    // per-cell projection unallocated; we only want the text rows.
    let screen = synth.screen_state_with_scrollback(terminal, 0, scrollback, false)?;

    let mut matches = Vec::new();
    for (row, line) in screen.scrollback.iter().enumerate() {
        find_in_row(line, needle, Region::Scrollback, row, opts, &mut matches);
    }
    if opts.include_viewport {
        for (row, line) in screen.lines.iter().enumerate() {
            find_in_row(line, needle, Region::Viewport, row, opts, &mut matches);
        }
    }
    Ok(matches)
}

/// Convenience: drive [`search`] from a one-shot synthesizer. Per-pane hot
/// paths should reuse a [`SnapshotSynthesizer`] and call [`search`] directly.
pub fn search_oneshot(
    terminal: &GhosttyTerminal<'_, '_>,
    needle: &str,
    scope: Scope,
    opts: SearchOptions,
) -> Result<Vec<Match>, SynthesisError> {
    let synth = SnapshotSynthesizer::new()?;
    search(&synth, terminal, needle, scope, opts)
}

/// Append every (non-overlapping) hit of `needle` in `line` to `out`.
///
/// Columns and lengths are reported in `char` offsets into `line`. The scan
/// is byte-based against the chosen casing, then the byte offset is mapped to
/// a `char` offset so the reported coordinates index the row's scalar values
/// regardless of multi-byte UTF-8.
fn find_in_row(
    line: &str,
    needle: &str,
    region: Region,
    row: usize,
    opts: SearchOptions,
    out: &mut Vec<Match>,
) {
    // `char` length of the needle is stable across casing for ASCII-only
    // folding (the only folding we do), so compute it once.
    let needle_chars = needle.chars().count();

    if opts.case_insensitive {
        let hay = line.to_ascii_lowercase();
        let pat = needle.to_ascii_lowercase();
        // `hay` is a fresh String with the same char boundaries as `line`
        // (ASCII lowercasing never changes byte length), so byte offsets map
        // back onto `line` unchanged.
        push_byte_hits(&hay, &pat, line, region, row, needle_chars, out);
    } else {
        push_byte_hits(line, needle, line, region, row, needle_chars, out);
    }
}

/// Scan `hay` for `pat`, translating each byte offset into a `char` offset
/// in `origin` (the original, un-cased row) before recording the [`Match`].
fn push_byte_hits(
    hay: &str,
    pat: &str,
    origin: &str,
    region: Region,
    row: usize,
    needle_chars: usize,
    out: &mut Vec<Match>,
) {
    let mut from = 0usize;
    while let Some(rel) = hay[from..].find(pat) {
        let byte = from + rel;
        let col = origin[..byte].chars().count();
        out.push(Match {
            region,
            row,
            col,
            len: needle_chars,
        });
        // Advance past this match so hits don't overlap. `pat` is non-empty
        // (callers reject an empty needle), so `from` strictly increases.
        from = byte + pat.len();
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use libghostty_vt::{
        fmt::{Format, Formatter, FormatterOptions},
        selection::{FormatOptions, Selection},
        terminal::{Point, PointCoordinate},
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

    fn vp() -> SearchOptions {
        SearchOptions {
            case_insensitive: false,
            include_viewport: true,
        }
    }

    #[test]
    fn empty_needle_yields_no_matches() {
        let mut t = fresh(20, 3);
        t.vt_write(b"hello world");
        let hits = search_oneshot(&t, "", Scope::AllHistory, vp()).expect("search");
        assert!(hits.is_empty(), "an empty needle must match nothing");
    }

    #[test]
    fn finds_needle_in_viewport() {
        let mut t = fresh(40, 3);
        t.vt_write(b"the quick brown fox");
        let hits = search_oneshot(&t, "brown", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        let m = hits[0];
        assert_eq!(m.region, Region::Viewport);
        assert_eq!(m.row, 0);
        assert_eq!(m.col, 10, "'brown' starts at char 10 of the row");
        assert_eq!(m.len, 5);
    }

    #[test]
    fn finds_needle_in_scrollback_history() {
        // 5 lines, 2-row viewport -> 3 history rows (line1..line3).
        let mut t = fresh(20, 2);
        t.vt_write(b"alpha\r\nbravo\r\ncharlie\r\ndelta\r\necho");
        assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 3);

        let hits = search_oneshot(&t, "bravo", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        let m = hits[0];
        assert_eq!(m.region, Region::Scrollback);
        // History rows are oldest-first: alpha(0), bravo(1), charlie(2).
        assert_eq!(m.row, 1);
        assert_eq!((m.col, m.len), (0, 5));
    }

    #[test]
    fn viewport_excluded_when_not_requested() {
        let mut t = fresh(20, 2);
        t.vt_write(b"hit-1\r\nhit-2\r\nhit-3\r\nhit-4");
        // 4 lines, 2-row viewport -> hit-1, hit-2 in scrollback; hit-3, hit-4
        // in the viewport.
        let opts = SearchOptions {
            case_insensitive: false,
            include_viewport: false,
        };
        let hits = search_oneshot(&t, "hit", Scope::AllHistory, opts).expect("search");
        assert!(
            hits.iter().all(|m| m.region == Region::Scrollback),
            "include_viewport = false must not surface viewport rows, got {hits:?}",
        );
        assert_eq!(hits.len(), 2, "two history rows carry the needle");
    }

    #[test]
    fn recent_history_bounds_the_scan() {
        let mut t = fresh(20, 2);
        // 5 lines, 2-row viewport -> 3 history rows (mark1..mark3). Bound to
        // the most-recent 1 history row (mark3) and exclude the viewport.
        t.vt_write(b"mark1\r\nmark2\r\nmark3\r\nplain4\r\nplain5");
        assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 3);

        let opts = SearchOptions {
            case_insensitive: false,
            include_viewport: false,
        };
        let hits = search_oneshot(&t, "mark", Scope::RecentHistory(1), opts).expect("search");
        // Only the most-recent history row (mark3) is in scope.
        assert_eq!(hits.len(), 1, "only the last history row is searched");
        assert_eq!(hits[0].region, Region::Scrollback);
    }

    #[test]
    fn case_insensitive_matches_mixed_case() {
        let mut t = fresh(40, 3);
        t.vt_write(b"ERROR: Disk Full");
        let sensitive = search_oneshot(&t, "error", Scope::AllHistory, vp()).expect("search");
        assert!(
            sensitive.is_empty(),
            "case-sensitive 'error' must not hit 'ERROR'"
        );

        let opts = SearchOptions {
            case_insensitive: true,
            include_viewport: true,
        };
        let insensitive = search_oneshot(&t, "error", Scope::AllHistory, opts).expect("search");
        assert_eq!(insensitive.len(), 1);
        assert_eq!((insensitive[0].col, insensitive[0].len), (0, 5));
    }

    #[test]
    fn reports_multiple_non_overlapping_hits_per_row() {
        let mut t = fresh(40, 2);
        t.vt_write(b"aabaab");
        let hits = search_oneshot(&t, "aa", Scope::AllHistory, vp()).expect("search");
        // "aabaab": matches at char 0 and char 3; the scan resumes past each
        // hit, so the overlapping 'a' at index 1 is not double-counted.
        let cols: Vec<usize> = hits.iter().map(|m| m.col).collect();
        assert_eq!(cols, vec![0, 3], "non-overlapping hits, got {hits:?}");
    }

    #[test]
    fn column_offsets_count_scalars_past_multibyte() {
        // A multibyte glyph before the needle must not inflate the reported
        // column: coordinates are char offsets, not byte offsets.
        let mut t = fresh(40, 2);
        // "é" is two UTF-8 bytes but one char; "TODO" then starts at char 2.
        t.vt_write("éxTODO".as_bytes());
        let hits = search_oneshot(&t, "TODO", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].col, 2, "char offset, not byte offset, got {hits:?}");
    }

    #[test]
    fn no_history_searches_viewport_only() {
        let mut t = fresh(40, 5);
        t.vt_write(b"single line, no scrollback");
        assert_eq!(t.scrollback_rows().expect("scrollback_rows"), 0);
        let hits = search_oneshot(&t, "scrollback", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].region, Region::Viewport);
    }

    #[test]
    fn wide_glyph_before_scrollback_match_keeps_char_offset_and_extracts_correctly() {
        // Regression for phux-4xi: pins the search-layer contract that
        // `Match.col` is a CHAR offset into the right-trimmed projected row,
        // not a grid column, AND proves that contract holds end-to-end through
        // the extraction consumer that remaps char-col -> grid-col.
        //
        // Row "你好xNEEDLE" has two wide glyphs (你, 好) before the needle.
        // Its projected text is "你好xNEEDLE", so "NEEDLE" sits at CHAR offset
        // 3 (你=0, 好=1, x=2). But its GRID column is 5 (你 cols 0-1, 好 cols
        // 2-3, x col 4). If search reported a grid column it would say 5; it
        // must report the char offset 3. Pushing two more lines drives the
        // wide row into scrollback so this exercises the history path.
        let mut t = fresh(20, 2);
        t.vt_write("你好xNEEDLE\r\n".as_bytes());
        t.vt_write(b"filler-a\r\nfiller-b");

        let hits = search_oneshot(&t, "NEEDLE", Scope::AllHistory, vp()).expect("search");
        assert_eq!(hits.len(), 1);
        let m = hits[0];
        assert_eq!(m.region, Region::Scrollback, "the wide row is in history");
        assert_eq!(
            m.col, 3,
            "Match.col is the CHAR offset (你,好,x precede), not the grid column 5",
        );
        assert_eq!(m.len, 6, "len counts chars in NEEDLE");

        // End-to-end: the extraction consumer remaps char-col 3 -> grid-col 5
        // (wide-glyph aware) and pulls back the correct text. This proves the
        // documented char->grid mapping is the consumer's job and that it
        // holds through search + extract even with a wide glyph before the
        // match.
        let text = crate::extract::extract_match_in_scope(&t, m, Scope::AllHistory)
            .expect("extract scrollback match");
        assert_eq!(
            text, "NEEDLE",
            "char-offset Match.col round-trips correct text through the \
             grid-column remap in extract.rs, despite preceding wide glyphs",
        );
    }

    #[test]
    fn safe_api_selection_formatter_bridge_round_trips_at_pin() {
        // Confirms the sound, selection-restricted text-extraction path that
        // unblocks the extraction wrapper (phux-3sy / phux-97w): build a
        // `selection::Selection` from two `Terminal::grid_ref` endpoints and
        // format just that sub-range back to text.
        //
        // Pin history. At acc4b87 the only selection-aware formatter entry was
        // `Formatter::new` with `FormatterOptions::selection: Some(_)`, and
        // `Formatter::new_inner` took the address of a match-arm-local copy of
        // the `ffi::Selection` (`&raw const s`) that dropped before the FFI
        // call read through it — a dangling pointer, so libghostty returned
        // `InvalidValue`. Pin 8e1b0f7 ships the dedicated one-shot selection
        // API (`Terminal::format_selection_alloc` + `FormatOptions`), which
        // holds the `Selection` *by borrow* across the FFI call and is sound.
        // That is the path phux's extraction bridge will use, and the one this
        // test pins.
        //
        // CAVEAT (still-broken sibling path). `Formatter::new` with
        // `selection: Some(_)` is still unsound at 8e1b0f7: `new_inner` now
        // writes `&s.inner` where `s` is the `Selection` moved out of the
        // by-value `Option` in the match arm, so the borrow still dangles
        // before `opts.into()` runs. phux therefore extracts via
        // `format_selection_alloc`, never via `Formatter { selection: Some }`.
        // See the upstream follow-up note in the structured report.
        let mut t = fresh(40, 3);
        t.vt_write(b"the quick brown fox");

        // Whole-screen format (selection: None): works — this path never
        // builds a selection pointer.
        let mut whole = Formatter::new(
            &t,
            FormatterOptions::new()
                .with_format(Format::Plain)
                .with_trim(true)
                .with_unwrap(true),
        )
        .expect("whole-screen formatter constructs");
        let bytes = whole.format_alloc(None).expect("whole-screen format");
        assert!(
            String::from_utf8_lossy(&bytes).contains("brown"),
            "the no-selection formatter path is functional",
        );

        // Selection-restricted format via the sound one-shot API. Build a
        // linear selection over "brown" (cols 10..=14 of the only row) and
        // extract just that sub-range.
        let selection = Selection::new(
            t.grid_ref(Point::Active(PointCoordinate { x: 10, y: 0 }))
                .expect("start grid_ref"),
            t.grid_ref(Point::Active(PointCoordinate { x: 14, y: 0 }))
                .expect("end grid_ref"),
            false,
        );
        let extracted = t
            .format_selection_alloc(
                None,
                FormatOptions::new()
                    .with_emit_format(Format::Plain)
                    .with_trim(true)
                    .with_unwrap(true)
                    .with_selection(&selection),
            )
            .expect("selection-restricted formatting succeeds at pin 8e1b0f7")
            .expect("a non-empty selection yields Some(bytes)");
        assert_eq!(
            String::from_utf8_lossy(&extracted).trim(),
            "brown",
            "the selection-restricted extraction path round-trips the \
             sub-range text (cols 10..=14 -> \"brown\")",
        );
    }
}
