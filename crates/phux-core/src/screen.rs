//! Structured screen projection — the agent surface's read shape
//! (ADR-0022 §2, `phux-oki`).
//!
//! A [`ScreenState`] is a point-in-time projection of one pane's grid as
//! plain data: dims, cursor, and the viewport rows as text. It is the
//! stable JSON contract the CLI emits (`phux snapshot --json`) and the
//! payload the server returns from the side-effect-free `GET_SCREEN`
//! control command.
//!
//! This type lives in `phux-core` (not the server or client) precisely so
//! both ends share one definition: the server *produces* it by walking its
//! own libghostty `Terminal`; the CLI *consumes* it by deserializing the
//! `COMMAND_RESULT` JSON. Keeping it pure data here — no libghostty, no
//! I/O — is what lets the walk run server-side without dragging emulator
//! types across the crate boundary.

use serde::{Deserialize, Serialize};

/// Stable JSON contract version (ADR-0022 §2). Bump on any breaking change
/// to the [`ScreenState`] shape so consumers can pin or branch.
///
/// `2` adds the additive [`ScreenState::scrollback`] field (`phux-o1v`).
/// `3` adds the additive [`ScreenState::cells`] field (`phux-8yl`). Both
/// fields carry `#[serde(default)]`, so an older-shaped JSON (missing the
/// `scrollback` or `cells` key) still deserializes; the bump is the signal
/// for consumers that want to *produce* or *require* the newer fields.
///
/// It stays at `3` across the ADR-0077 additions ([`ScreenState::soft_wrap`],
/// [`ScreenState::truncated`], [`ScreenState::truncated_reason`],
/// [`ScreenState::title`]). `docs/consumers/agents.md` §4.1 is the governing
/// contract and it moves the version only when a key is **removed, renamed,
/// or retyped**; every one of those four is a new optional key that an older
/// consumer ignores and an older payload omits. Consumers that need to know
/// whether a *server* reports wrap information read
/// [`ScreenState::has_soft_wrap_info`] rather than the version — the
/// `Option` is the capability signal, which is strictly more precise than a
/// version number the old server would not have carried either.
pub const SCHEMA_VERSION: u32 = 3;

/// "Every row" sentinel for a row-count window, matching the `--scrollback`
/// tri-state convention: `None` ⇒ off, `Some(0)` ⇒ all, `Some(n)` ⇒ the most
/// recent `n` (`phux-o1v`, ADR-0077 §3).
pub const ROW_WINDOW_ALL: u32 = 0;

/// Row count for a bare `--tail` — one comfortable screenful of recent
/// output.
///
/// Deliberately the same number herdr defaults its `recent` sources to (80):
/// an agent asking for "what just happened" without a count wants a bounded
/// answer, and a bound that matches the neighbouring tool's is one less
/// surprise. `--tail 0` still means "all rendered rows" (subject to
/// [`ROW_WINDOW_MAX`]), so nothing is unreachable.
pub const ROW_WINDOW_DEFAULT: u32 = 80;

/// Hard ceiling on any row window, applied even to [`ROW_WINDOW_ALL`].
///
/// herdr clamps to 1000 because its window rides an HTTP/JSON API. phux's
/// snapshot rides a local UDS and is asked for by an agent a few times a
/// second, so the tight bound buys nothing: 10 000 rows at a typical 200
/// columns is ~2 MB worst case, comfortably inside the payload budget, and
/// it keeps `--tail 0` from turning an unbounded history into an unbounded
/// allocation. Crossing it sets [`ScreenState::truncated`], so the clamp is
/// never silent.
pub const ROW_WINDOW_MAX: u32 = 10_000;

/// [`ScreenState::truncated_reason`] value for a row window that dropped
/// older rows — the only reason this crate produces today.
///
/// The field is a plain string rather than an enum on purpose: the
/// vocabulary grows server-side (ADR-0078's alternate-screen harvest mints
/// its own key and its own reasons), and a closed enum would turn a newer
/// server's reason into a hard deserialize failure on an older consumer.
/// Consumers MUST tolerate an unknown value.
pub const TRUNCATED_ROW_WINDOW: &str = "row_window";

/// A color drawn from a libghostty style attribute, projected to plain data
/// (`phux-8yl`).
///
/// Mirrors libghostty's `style::StyleColor`: a cell's foreground or
/// background is either unset (the terminal default), a palette index
/// (`0..=255`, the 16 ANSI names plus the 256-color cube), or a direct
/// 24-bit RGB triple. Kept as a tagged enum so the JSON distinguishes
/// "default" from "explicitly black", which a flattened RGB cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CellColor {
    /// The terminal default (no explicit color set on the cell).
    #[default]
    Default,
    /// A palette index: `0..=15` are the ANSI names, `16..=255` the
    /// 256-color cube/greyscale ramp.
    Palette {
        /// The palette slot.
        index: u8,
    },
    /// A direct 24-bit truecolor value.
    Rgb {
        /// Red channel.
        r: u8,
        /// Green channel.
        g: u8,
        /// Blue channel.
        b: u8,
    },
}

/// OSC-133 semantic content classification of a cell (`phux-8yl`).
///
/// Set by shell integration via OSC-133 prompt-mark sequences; mirrors the
/// meaningful subset of libghostty's `screen::CellSemanticContent`. Lets an
/// agent tell shell prompt text apart from typed input without re-parsing
/// the screen heuristically.
///
/// The server collapses libghostty's `Output` (which is the *default* for
/// every cell, marked or not) to absence — [`CellInfo::semantic`] is `None`
/// for output and unmarked cells, and `Some` only for [`Self::Input`] /
/// [`Self::Prompt`]. [`Self::Output`] is retained in the enum for
/// forward-compatibility and explicit consumer matching, but the current
/// server never emits it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticContent {
    /// Command output. Never emitted by the current server (collapsed to
    /// `None`); see the type-level note.
    Output,
    /// User-typed input on a command line.
    Input,
    /// Shell prompt text.
    Prompt,
}

/// Per-cell text-style attributes, projected to plain data (`phux-8yl`).
///
/// Mirrors the boolean attribute set of libghostty's `style::Style` plus
/// the resolved foreground/background colors. The SGR `underline` *style*
/// (single/double/curly/…) is intentionally collapsed to a single
/// [`Self::underline`] bool for now — the agent surface cares that a cell
/// is underlined, not which of six dash patterns; the richer enum can land
/// additively later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "SGR attributes are an inherent bitset of independent flags, \
              mirroring libghostty's own `style::Style`; folding them into \
              two-variant enums would obscure the 1:1 mapping to SGR codes \
              and the JSON shape without buying anything"
)]
pub struct CellStyle {
    /// Bold (SGR 1).
    pub bold: bool,
    /// Faint / dim (SGR 2).
    pub faint: bool,
    /// Italic (SGR 3).
    pub italic: bool,
    /// Underlined (any SGR 4 variant).
    pub underline: bool,
    /// Blink (SGR 5).
    pub blink: bool,
    /// Inverse / reverse video (SGR 7).
    pub inverse: bool,
    /// Invisible / concealed (SGR 8).
    pub invisible: bool,
    /// Strikethrough (SGR 9).
    pub strikethrough: bool,
    /// Overline (SGR 53).
    pub overline: bool,
    /// Foreground color.
    pub fg: CellColor,
    /// Background color.
    pub bg: CellColor,
}

/// One cell's semantic + style projection (`phux-8yl`).
///
/// Cells are emitted in row-major order, skipping the right half of
/// double-width glyphs (libghostty's `SpacerTail`) — so a given `(row,
/// col)` appears at most once, and the base glyph carries the `(row, col)`
/// of its left edge. Blank cells are *not* emitted: the [`CellInfo`] vec is
/// sparse, carrying only cells with a non-default style or a semantic mark,
/// which keeps the JSON small for a mostly-empty grid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellInfo {
    /// Zero-based column, viewport-relative.
    pub col: u16,
    /// Zero-based row, viewport-relative.
    pub row: u16,
    /// OSC-133 semantic content, when the shell marked it; `None`
    /// otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic: Option<SemanticContent>,
    /// Text-style attributes for the cell.
    pub style: CellStyle,
}

/// Which rows of a [`ScreenState`] continue onto the row below them
/// (ADR-0077 §2).
///
/// libghostty tracks a soft wrap per row: a row whose text ran past the
/// right margin is flagged, and the row under it holds the continuation.
/// The server is the only side that can see that bit — by the time a row is
/// a right-trimmed `String` the wrap is indistinguishable from a hard
/// newline — so it travels, and *joining* stays consumer-side
/// ([`ScreenState::unwrapped_rows`]).
///
/// This is load-bearing rather than cosmetic: a substring match against
/// rows-as-painted silently fails whenever the match straddles a wrap, which
/// is exactly the `phux wait --until TEXT` bug ADR-0077 §2 exists to close.
///
/// Both vectors hold **row indices into their own array**, ascending. A
/// trailing wrapped [`Self::scrollback`] index (the last history row)
/// continues into `lines[0]`: history and viewport are one stream for
/// wrapping purposes, and the join walks them as one — see
/// [`ScreenState::unwrapped_split`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SoftWrap {
    /// Indices into [`ScreenState::lines`] whose row continues onto the
    /// next viewport row.
    #[serde(default)]
    pub lines: Vec<u32>,
    /// Indices into [`ScreenState::scrollback`] whose row continues onto
    /// the next history row — or, for the last index, into `lines[0]`.
    #[serde(default)]
    pub scrollback: Vec<u32>,
}

/// `skip_serializing_if` helper for [`ScreenState::truncated`]: a `false`
/// truncation flag is the pre-ADR-0077 shape, so it emits no key at all.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if hands the field by reference"
)]
const fn is_not_truncated(truncated: &bool) -> bool {
    !*truncated
}

/// Cursor position + visibility, viewport-relative, zero-based.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorState {
    /// Column, zero-based, viewport-relative.
    pub x: u16,
    /// Row, zero-based, viewport-relative.
    pub y: u16,
    /// Whether the cursor is currently visible (DECTCEM).
    pub visible: bool,
}

/// A point-in-time projection of one pane's grid as structured data.
///
/// The default shape is plain text lines + cursor + dims — what most
/// agents want. Per-cell styles and OSC-133 semantic marks ride the
/// additive [`Self::cells`] field (`--cells`, `phux-8yl`), not a new
/// struct (ADR-0022 §2); scrollback is the additive [`Self::scrollback`]
/// field (`--scrollback`, `phux-o1v`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenState {
    /// Contract version; see [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Wire-local terminal id of the captured pane.
    pub pane: u32,
    /// Grid width in cells.
    pub cols: u16,
    /// Grid height in cells.
    pub rows: u16,
    /// Cursor state, or `None` when the emulator can't resolve a
    /// viewport-resident cursor (e.g. it is in scrollback or hidden).
    pub cursor: Option<CursorState>,
    /// Viewport rows, top to bottom, right-trimmed.
    pub lines: Vec<String>,
    /// Scrollback history rows above the viewport, oldest first,
    /// right-trimmed. Populated only when the caller requests it
    /// (`phux snapshot --scrollback[=N]`); empty otherwise (`phux-o1v`).
    ///
    /// `#[serde(default)]` keeps the contract back-compatible: a v1-shaped
    /// JSON without this key deserializes to an empty `Vec`, and a v1
    /// consumer reading a v2 payload simply ignores the extra key.
    #[serde(default)]
    pub scrollback: Vec<String>,
    /// Per-cell semantic marks + styles for the viewport, or `None` when
    /// the caller did not request them (`phux snapshot --cells`,
    /// `phux-8yl`). When `Some`, the vec is sparse: only cells carrying a
    /// non-default style or an OSC-133 semantic mark are emitted, in
    /// row-major order. See [`CellInfo`].
    ///
    /// `#[serde(default)]` plus `skip_serializing_if` keeps the contract
    /// back-compatible: a JSON without this key deserializes to `None`,
    /// and the common `cells = None` snapshot serializes to exactly the
    /// pre-`phux-8yl` shape (no `cells` key at all).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<CellInfo>>,
    /// Which returned rows continue onto the row below them, from
    /// libghostty's per-row soft-wrap bit (ADR-0077 §2).
    ///
    /// `Some` — including `Some` of two empty vectors — means the producer
    /// **reported** wrap information and no row wraps. `None` means the
    /// producer said nothing, which today identifies a server predating this
    /// field. The distinction matters: a consumer that unwraps by default
    /// (every match path does) must be able to tell "nothing to join" from
    /// "cannot know", and a version number cannot express it because the
    /// older server does not send one that moved. See
    /// [`Self::has_soft_wrap_info`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_wrap: Option<SoftWrap>,
    /// True when the **requested window dropped older rows** (ADR-0077 §3).
    ///
    /// Scoped precisely: it reports clipping of the window the caller asked
    /// for — `--scrollback N` with more retained history than `N`, or a
    /// `--tail` window narrower than the rendered stream. It says nothing
    /// about rows the emulator itself evicted from its history ring long
    /// ago (unknowable), and it is not the marker for a refused
    /// alternate-screen harvest — ADR-0078 owns its own key for that, so
    /// that a consumer can never confuse "your transcript is clipped" with
    /// "you got no transcript".
    ///
    /// `#[serde(default)]` plus `skip_serializing_if` keeps an untruncated
    /// read byte-identical to the pre-ADR-0077 shape: absent means `false`,
    /// which is also the fail-safe reading for an older producer.
    #[serde(default, skip_serializing_if = "is_not_truncated")]
    pub truncated: bool,
    /// Why [`Self::truncated`] is true; `None` when it is false.
    ///
    /// Today the only value this crate produces is
    /// [`TRUNCATED_ROW_WINDOW`]. Consumers MUST tolerate an unknown value —
    /// see that constant for why it is a string and not an enum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_reason: Option<String>,
    /// The pane's OSC 0/2 title at capture time, when it has one
    /// (ADR-0077 §3).
    ///
    /// It is the ADR-0046 detector's highest-ranked evidence and no other
    /// read surface exposes it, so an offline `agent explain --file`
    /// capture loses it today. `None` means the pane set no title *or* the
    /// producer predates the field; both are "no title to reason about",
    /// which is the same fail-safe answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl Default for ScreenState {
    /// An empty 0x0 screen at the current [`SCHEMA_VERSION`].
    ///
    /// Exists so a caller that builds a `ScreenState` literal can spell the
    /// additive ADR-0077 keys as `..ScreenState::default()` and stay
    /// source-compatible across later additions.
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            pane: 0,
            cols: 0,
            rows: 0,
            cursor: None,
            lines: Vec::new(),
            scrollback: Vec::new(),
            cells: None,
            soft_wrap: None,
            truncated: false,
            truncated_reason: None,
            title: None,
        }
    }
}

impl ScreenState {
    /// Whether the producer reported soft-wrap information at all.
    ///
    /// `false` means an older server that cannot describe wraps, so an
    /// unwrapping consumer is reading rows as painted and a match that
    /// straddles a wrap can still be missed. That is the honest degradation
    /// and it is detectable — which is the whole point of
    /// [`Self::soft_wrap`] being an `Option`.
    #[must_use]
    pub const fn has_soft_wrap_info(&self) -> bool {
        self.soft_wrap.is_some()
    }

    /// Every returned row as painted: history above the viewport first,
    /// then the viewport, oldest first.
    ///
    /// This is the "rendered rows" stream a row window
    /// ([`row_window`]) and the unwrapper both operate over.
    #[must_use]
    pub fn rendered_rows(&self) -> Vec<&str> {
        self.scrollback
            .iter()
            .chain(self.lines.iter())
            .map(String::as_str)
            .collect()
    }

    /// The rows as **written** rather than as painted: each run of
    /// soft-wrapped rows joined into one logical line, history and viewport
    /// walked as a single stream so a run straddling the seam joins too.
    ///
    /// This is the function every match path should call — `wait --until`,
    /// `run`'s completion probe, any output-substring subscription. Matching
    /// raw viewport rows silently fails whenever the wanted text falls
    /// across a wrap, and a wrap is invisible in the joined-and-trimmed
    /// text, so the failure is silent by construction.
    ///
    /// When [`Self::has_soft_wrap_info`] is `false` the rows come back
    /// verbatim: with no wrap bits there is nothing to join, and inventing a
    /// heuristic ("the row is exactly `cols` wide, so it probably wrapped")
    /// would guess wrong on any full-width box-drawn line.
    ///
    /// Joining concatenates the **right-trimmed** rows the projection
    /// carries, so a wrap that fell inside a run of spaces loses them. That
    /// is inherited from the row projection, not introduced here.
    #[must_use]
    pub fn unwrapped_rows(&self) -> Vec<String> {
        let (mut rows, viewport) = self.unwrapped_split();
        rows.extend(viewport);
        rows
    }

    /// [`Self::unwrapped_rows`], kept split as `(scrollback, lines)`.
    ///
    /// A logical line is attributed to the array in which it **ends**, so a
    /// run that starts in history and finishes in the viewport lands in the
    /// viewport half. That keeps the two halves a partition (no row is
    /// duplicated or dropped) and puts a straddling run where a consumer
    /// reading "the live screen" expects it.
    ///
    /// Note that after unwrapping, `lines` no longer indexes the grid:
    /// `lines.len()` need not equal `rows`, and `cursor` / `cells`
    /// coordinates are grid coordinates that do not survive the join.
    #[must_use]
    pub fn unwrapped_split(&self) -> (Vec<String>, Vec<String>) {
        let split = self.scrollback.len();
        let Some(wrap) = self.soft_wrap.as_ref() else {
            return (self.scrollback.clone(), self.lines.clone());
        };

        // Flatten both index vectors into one stream-indexed predicate:
        // "row i continues onto row i + 1". Out-of-range indices from a
        // malformed payload are ignored rather than trusted.
        let mut continues = vec![false; split.saturating_add(self.lines.len())];
        for index in &wrap.scrollback {
            if let Some(slot) = usize::try_from(*index)
                .ok()
                .and_then(|i| continues.get_mut(i))
            {
                *slot = true;
            }
        }
        for index in &wrap.lines {
            if let Some(slot) = usize::try_from(*index)
                .ok()
                .and_then(|i| i.checked_add(split))
                .and_then(|i| continues.get_mut(i))
            {
                *slot = true;
            }
        }

        let last = continues.len().saturating_sub(1);
        let mut history: Vec<String> = Vec::new();
        let mut viewport: Vec<String> = Vec::new();
        let mut buf = String::new();
        for (i, row) in self.scrollback.iter().chain(self.lines.iter()).enumerate() {
            buf.push_str(row);
            // A wrapped final row has nothing left to join to, so the run
            // closes there rather than leaking past the end of the stream.
            if i < last && continues.get(i).copied().unwrap_or(false) {
                continue;
            }
            if i < split {
                history.push(std::mem::take(&mut buf));
            } else {
                viewport.push(std::mem::take(&mut buf));
            }
        }
        (history, viewport)
    }
}

/// Clamp a row stream to its most recent `want` rows, reporting whether
/// older rows were dropped (ADR-0077 §3).
///
/// `want` follows the `--scrollback` tri-state convention:
/// [`ROW_WINDOW_ALL`] (`0`) means every row, any other value the most recent
/// `want`. Either way the result is capped at [`ROW_WINDOW_MAX`].
///
/// The returned flag is what [`ScreenState::truncated`] carries: `true`
/// exactly when at least one older row was removed to satisfy the window.
#[must_use]
pub fn row_window(mut rows: Vec<String>, want: u32) -> (Vec<String>, bool) {
    let keep = if want == ROW_WINDOW_ALL {
        ROW_WINDOW_MAX
    } else {
        want.min(ROW_WINDOW_MAX)
    };
    let keep = usize::try_from(keep).unwrap_or(usize::MAX);
    if rows.len() <= keep {
        return (rows, false);
    }
    let dropped = rows.len() - keep;
    let tail = rows.split_off(dropped);
    (tail, true)
}

/// Stable JSON contract version for [`RenderedFrame`] (`phux-l5xa`).
///
/// Independent of [`SCHEMA_VERSION`] (the per-pane [`ScreenState`] contract):
/// the composited-frame projection is a different shape with its own
/// evolution. Bump on any breaking change to [`RenderedFrame`].
pub const RENDERED_SCHEMA_VERSION: u32 = 1;

/// One cell of the client's composited frame (`phux-l5xa`).
///
/// Unlike [`CellInfo`] (sparse, per-pane, carries only non-default cells)
/// this is a *dense* cell: every column of the assembled frame has exactly
/// one, so a consumer can index `cells[row * cols + col]` and read the glyph
/// and style the human's glass actually shows — pane content, dividers, and
/// status bar alike, already composited.
///
/// `grapheme` is the cell's grapheme cluster:
/// * a normal glyph (`"a"`, `"世"`, a ZWJ emoji sequence) for a drawn cell;
/// * a single space (`" "`) for a blank cell;
/// * the empty string (`""`) for the right half of a double-width glyph
///   (libghostty's `SpacerTail`) — the preceding cell's wide glyph already
///   occupies this column, so emitting no glyph here preserves exact widths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedCell {
    /// The cell's grapheme cluster; `" "` when blank, `""` for a wide-glyph
    /// tail. See the type-level note.
    pub grapheme: String,
    /// Resolved text-style attributes for the cell.
    pub style: CellStyle,
}

/// The client's composited multi-pane view, as structured dense cells
/// (`phux snapshot --rendered`, `phux-l5xa`).
///
/// Where [`ScreenState`] projects a single server-side pane grid, this
/// projects the **assembled frame** the client renders: layout tiling,
/// dividers, and the status bar, composited exactly as painted to the
/// terminal — but returned as cells (grapheme + style + cursor) rather than
/// VT bytes, so an agent, a test, or an assistant debugging a render bug can
/// ask "what does the screen look like right now" and get an answer with no
/// external emulator in the loop (closing the symmetric-blindspot gap that
/// forced pyte before).
///
/// Cells are dense and row-major: `cells.len() == cols as usize * rows as
/// usize`, and the cell at `(row, col)` is `cells[row * cols + col]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedFrame {
    /// Contract version; see [`RENDERED_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Composited frame width in cells.
    pub cols: u16,
    /// Composited frame height in cells.
    pub rows: u16,
    /// The composited cursor (whichever pane's cursor the end-of-frame
    /// policy elects), or `None` when no pane contributes a visible
    /// viewport cursor.
    pub cursor: Option<CursorState>,
    /// Dense, row-major cells of the assembled frame. Length is exactly
    /// `cols * rows`; index `(row, col)` as `cells[row * cols + col]`.
    pub cells: Vec<RenderedCell>,
}

impl RenderedFrame {
    /// Build a blank frame of `cols * rows` space cells with the default
    /// style and no cursor — the canvas the compositor fills.
    #[must_use]
    pub fn blank(cols: u16, rows: u16) -> Self {
        let len = usize::from(cols) * usize::from(rows);
        Self {
            schema_version: RENDERED_SCHEMA_VERSION,
            cols,
            rows,
            cursor: None,
            cells: vec![
                RenderedCell {
                    grapheme: " ".to_owned(),
                    style: CellStyle::default(),
                };
                len
            ],
        }
    }

    /// Mutable access to the cell at `(row, col)`, or `None` when the
    /// coordinate is outside the frame.
    pub fn cell_mut(&mut self, row: u16, col: u16) -> Option<&mut RenderedCell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let idx = usize::from(row) * usize::from(self.cols) + usize::from(col);
        self.cells.get_mut(idx)
    }

    /// The cell at `(row, col)`, or `None` when out of range.
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<&RenderedCell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        let idx = usize::from(row) * usize::from(self.cols) + usize::from(col);
        self.cells.get(idx)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    /// A v1-shaped JSON (no `scrollback`/`cells` keys, `schema_version =
    /// 1`) must still deserialize — both additive fields are
    /// `#[serde(default)]`, so older producers stay readable (`phux-o1v` /
    /// `phux-8yl` back-compat).
    #[test]
    fn deserializes_v1_json_without_scrollback_or_cells() {
        let v1 = r#"{
            "schema_version": 1,
            "pane": 3,
            "cols": 80,
            "rows": 2,
            "cursor": null,
            "lines": ["hello", "world"]
        }"#;
        let screen: ScreenState =
            serde_json::from_str(v1).expect("v1 JSON must deserialize into the current struct");
        assert_eq!(screen.schema_version, 1);
        assert_eq!(screen.lines, vec!["hello".to_owned(), "world".to_owned()]);
        assert!(
            screen.scrollback.is_empty(),
            "missing scrollback key defaults to empty",
        );
        assert!(screen.cells.is_none(), "missing cells key defaults to None",);
        assert!(
            !screen.has_soft_wrap_info(),
            "an older payload reports no wrap information, and that is detectable",
        );
        assert!(!screen.truncated, "missing truncated key defaults to false");
        assert!(screen.truncated_reason.is_none());
        assert!(screen.title.is_none());
    }

    /// Build a screen with explicit wrap bits. `wrapped_lines` /
    /// `wrapped_scrollback` are indices into their own array.
    fn wrapped_screen(
        scrollback: &[&str],
        lines: &[&str],
        wrapped_scrollback: &[u32],
        wrapped_lines: &[u32],
    ) -> ScreenState {
        ScreenState {
            schema_version: SCHEMA_VERSION,
            pane: 1,
            cols: 8,
            rows: u16::try_from(lines.len()).unwrap_or(0),
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            scrollback: scrollback.iter().map(|s| (*s).to_owned()).collect(),
            soft_wrap: Some(SoftWrap {
                lines: wrapped_lines.to_vec(),
                scrollback: wrapped_scrollback.to_vec(),
            }),
            ..ScreenState::default()
        }
    }

    /// The headline case: a logical line that soft-wrapped across two
    /// viewport rows joins back into one, so a substring straddling the
    /// wrap matches (`wait --until` today does not — ADR-0077 §2).
    #[test]
    fn unwraps_a_soft_wrapped_line_into_one_logical_row() {
        let screen = wrapped_screen(&[], &["the quick", "brown fox", "next line"], &[], &[0]);
        assert_eq!(
            screen.unwrapped_rows(),
            vec!["the quickbrown fox".to_owned(), "next line".to_owned()],
        );
        assert!(
            screen
                .unwrapped_rows()
                .iter()
                .any(|l| l.contains("quickbrown")),
            "the straddling substring must be findable after unwrapping",
        );
        assert!(
            !screen.lines.iter().any(|l| l.contains("quickbrown")),
            "and must NOT be findable in the rows as painted — that is the bug",
        );
    }

    /// Three rows in one run collapse to a single logical line, and the run
    /// after it stays separate.
    #[test]
    fn unwraps_a_run_of_more_than_two_rows() {
        let screen = wrapped_screen(&[], &["aaa", "bbb", "ccc", "ddd"], &[], &[0, 1]);
        assert_eq!(
            screen.unwrapped_rows(),
            vec!["aaabbbccc".to_owned(), "ddd".to_owned()],
        );
    }

    /// A run that starts in history and ends in the viewport joins across
    /// the seam, and lands in the viewport half of
    /// [`ScreenState::unwrapped_split`] — the array where it ends.
    #[test]
    fn unwraps_across_the_scrollback_viewport_seam() {
        let screen = wrapped_screen(&["old", "hist"], &["ory", "live"], &[1], &[]);
        let (history, viewport) = screen.unwrapped_split();
        assert_eq!(history, vec!["old".to_owned()]);
        assert_eq!(viewport, vec!["history".to_owned(), "live".to_owned()]);
        assert_eq!(
            screen.unwrapped_rows(),
            vec!["old".to_owned(), "history".to_owned(), "live".to_owned()],
            "the split is a partition: no row duplicated, none dropped",
        );
    }

    /// A wrap flag on the last row has nothing to join to; the run closes at
    /// the end of the stream rather than dropping the row.
    #[test]
    fn a_wrapped_final_row_still_emits() {
        let screen = wrapped_screen(&[], &["only"], &[], &[0]);
        assert_eq!(screen.unwrapped_rows(), vec!["only".to_owned()]);
    }

    /// With no wrap information at all (an older server), rows come back
    /// verbatim rather than heuristically joined — and the caller can tell.
    #[test]
    fn absent_soft_wrap_info_returns_rows_verbatim() {
        let screen = ScreenState {
            lines: vec!["the quick".to_owned(), "brown fox".to_owned()],
            ..ScreenState::default()
        };
        assert!(!screen.has_soft_wrap_info());
        assert_eq!(
            screen.unwrapped_rows(),
            vec!["the quick".to_owned(), "brown fox".to_owned()],
        );
    }

    /// `Some` of two empty vectors is "reported, nothing wraps" — a
    /// different answer from `None`, and that difference is the whole reason
    /// the field is an `Option` while `SCHEMA_VERSION` stays 3.
    #[test]
    fn empty_soft_wrap_is_reported_not_absent() {
        let screen = wrapped_screen(&[], &["a", "b"], &[], &[]);
        assert!(screen.has_soft_wrap_info());
        assert_eq!(
            screen.unwrapped_rows(),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    /// Out-of-range wrap indices in a malformed payload are ignored, not
    /// trusted into a panic.
    #[test]
    fn out_of_range_wrap_indices_are_ignored() {
        let screen = wrapped_screen(&[], &["a", "b"], &[9], &[7, 0]);
        assert_eq!(screen.unwrapped_rows(), vec!["ab".to_owned()]);
    }

    /// `rendered_rows` is history-then-viewport, oldest first.
    #[test]
    fn rendered_rows_concatenates_history_then_viewport() {
        let screen = wrapped_screen(&["h1", "h2"], &["v1"], &[], &[]);
        assert_eq!(screen.rendered_rows(), vec!["h1", "h2", "v1"]);
    }

    /// The row window keeps the most recent rows and reports the drop.
    #[test]
    fn row_window_keeps_the_tail_and_reports_truncation() {
        let rows: Vec<String> = (0..10).map(|i| format!("row{i}")).collect();
        let (window, truncated) = row_window(rows.clone(), 3);
        assert_eq!(
            window,
            vec!["row7".to_owned(), "row8".to_owned(), "row9".to_owned()],
        );
        assert!(truncated, "older rows were dropped to satisfy the window");

        let (window, truncated) = row_window(rows.clone(), 10);
        assert_eq!(window.len(), 10);
        assert!(
            !truncated,
            "a window at least as large as the stream drops nothing"
        );

        let (window, truncated) = row_window(rows, ROW_WINDOW_ALL);
        assert_eq!(window.len(), 10);
        assert!(!truncated, "ROW_WINDOW_ALL under the ceiling drops nothing");
    }

    /// Even `ROW_WINDOW_ALL` is capped at [`ROW_WINDOW_MAX`], and the cap
    /// sets the flag rather than silently clipping.
    #[test]
    fn row_window_clamps_all_to_the_ceiling() {
        let over = usize::try_from(ROW_WINDOW_MAX).unwrap_or(usize::MAX) + 5;
        let rows: Vec<String> = (0..over).map(|i| format!("row{i}")).collect();
        let (window, truncated) = row_window(rows, ROW_WINDOW_ALL);
        assert_eq!(
            u32::try_from(window.len()).unwrap_or(u32::MAX),
            ROW_WINDOW_MAX,
        );
        assert!(truncated, "the ceiling is never silent");
    }

    /// A truncated screen serializes both keys; an untruncated one emits
    /// neither, keeping the pre-ADR-0077 payload byte-identical.
    #[test]
    fn truncated_keys_appear_only_when_true() {
        let clean = ScreenState {
            lines: vec!["hi".to_owned()],
            ..ScreenState::default()
        };
        let json = serde_json::to_string(&clean).expect("serialize");
        assert!(
            !json.contains("\"truncated\""),
            "an untruncated read must not grow a key, got: {json}",
        );
        assert!(!json.contains("\"truncated_reason\""));

        let clipped = ScreenState {
            lines: vec!["hi".to_owned()],
            truncated: true,
            truncated_reason: Some(TRUNCATED_ROW_WINDOW.to_owned()),
            ..ScreenState::default()
        };
        let json = serde_json::to_string(&clipped).expect("serialize");
        assert!(json.contains("\"truncated\":true"), "got: {json}");
        assert!(
            json.contains("\"truncated_reason\":\"row_window\""),
            "got: {json}"
        );
        let back: ScreenState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, clipped);
    }

    /// The ADR-0077 keys survive a JSON round trip.
    #[test]
    fn round_trips_soft_wrap_and_title() {
        let original = ScreenState {
            pane: 4,
            cols: 8,
            rows: 2,
            lines: vec!["ab".to_owned(), "cd".to_owned()],
            scrollback: vec!["old".to_owned()],
            soft_wrap: Some(SoftWrap {
                lines: vec![0],
                scrollback: vec![0],
            }),
            title: Some("claude — phux".to_owned()),
            ..ScreenState::default()
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: ScreenState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, original);
    }

    /// A round-trip carries scrollback through serialize/deserialize.
    #[test]
    fn round_trips_scrollback_field() {
        let original = ScreenState {
            schema_version: SCHEMA_VERSION,
            pane: 1,
            cols: 10,
            rows: 1,
            cursor: None,
            lines: vec!["live".to_owned()],
            scrollback: vec!["old1".to_owned(), "old2".to_owned()],
            cells: None,
            ..ScreenState::default()
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: ScreenState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, original);
    }

    /// A `cells = None` snapshot must serialize to exactly the pre-cells
    /// shape: no `cells` key at all (`skip_serializing_if`), so a consumer
    /// pinned to the older schema sees no surprise field (`phux-8yl`).
    #[test]
    fn omits_cells_key_when_none() {
        let screen = ScreenState {
            schema_version: SCHEMA_VERSION,
            pane: 1,
            cols: 2,
            rows: 1,
            cursor: None,
            lines: vec!["hi".to_owned()],
            scrollback: Vec::new(),
            cells: None,
            ..ScreenState::default()
        };
        let json = serde_json::to_string(&screen).expect("serialize");
        assert!(
            !json.contains("\"cells\""),
            "None cells must not emit a key, got: {json}",
        );
    }

    /// A populated `cells` field round-trips, including the semantic mark
    /// and the tagged color enum (`phux-8yl`).
    #[test]
    fn round_trips_cells_field() {
        let original = ScreenState {
            schema_version: SCHEMA_VERSION,
            pane: 2,
            cols: 4,
            rows: 1,
            cursor: None,
            lines: vec!["$ ls".to_owned()],
            scrollback: Vec::new(),
            cells: Some(vec![
                CellInfo {
                    col: 0,
                    row: 0,
                    semantic: Some(SemanticContent::Prompt),
                    style: CellStyle {
                        bold: true,
                        faint: false,
                        italic: false,
                        underline: false,
                        blink: false,
                        inverse: false,
                        invisible: false,
                        strikethrough: false,
                        overline: false,
                        fg: CellColor::Rgb { r: 1, g: 2, b: 3 },
                        bg: CellColor::Default,
                    },
                },
                CellInfo {
                    col: 2,
                    row: 0,
                    semantic: Some(SemanticContent::Input),
                    style: CellStyle {
                        bold: false,
                        faint: false,
                        italic: false,
                        underline: false,
                        blink: false,
                        inverse: false,
                        invisible: false,
                        strikethrough: false,
                        overline: false,
                        fg: CellColor::Palette { index: 7 },
                        bg: CellColor::Default,
                    },
                },
            ]),
            ..ScreenState::default()
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let decoded: ScreenState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, original);
    }

    /// `RenderedFrame::blank` allocates `cols * rows` dense space cells with
    /// the default style; `cell`/`cell_mut` index row-major and bound-check
    /// (`phux-l5xa`).
    #[test]
    fn rendered_frame_blank_is_dense_and_indexes_row_major() {
        let mut f = RenderedFrame::blank(3, 2);
        assert_eq!(f.schema_version, RENDERED_SCHEMA_VERSION);
        assert_eq!(f.cells.len(), 6);
        assert_eq!(f.cell(1, 2).expect("in range").grapheme, " ");
        assert_eq!(f.cell(1, 2).expect("in range").style, CellStyle::default());
        assert!(f.cell(2, 0).is_none(), "row past the frame is None");
        assert!(f.cell(0, 3).is_none(), "col past the frame is None");
        f.cell_mut(1, 2).expect("in range").grapheme = "X".to_owned();
        assert_eq!(f.cell(1, 2).expect("in range").grapheme, "X");
        // Row-major: (row 1, col 2) is index 1*3 + 2 = 5.
        assert_eq!(f.cells[5].grapheme, "X");
        assert!(f.cell_mut(2, 0).is_none(), "out-of-range mut is None");
    }

    /// A `RenderedFrame` survives a JSON round-trip, cursor and all
    /// (`phux-l5xa`).
    #[test]
    fn rendered_frame_json_round_trips() {
        let mut f = RenderedFrame::blank(2, 1);
        f.cell_mut(0, 1).expect("in range").grapheme = "Z".to_owned();
        f.cursor = Some(CursorState {
            x: 1,
            y: 0,
            visible: true,
        });
        let json = serde_json::to_string(&f).expect("serialize");
        let back: RenderedFrame = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(f, back);
    }
}
