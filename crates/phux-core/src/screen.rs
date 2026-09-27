//! Structured screen projection: the `phux snapshot --json` read shape.
//!
//! ADR-0022 §2; shared by the server that produces it (`GET_SCREEN`) and
//! the CLI that consumes it. Pure data; every optional key is additive.

use serde::{Deserialize, Serialize};

/// Stable JSON contract version (ADR-0022 §2). Moves only when a key is
/// removed, renamed, or retyped (`docs/consumers/agents.md` §4.1); new
/// optional keys leave it alone.
pub const SCHEMA_VERSION: u32 = 3;

/// "Every row" sentinel for a row window (`--scrollback` tri-state:
/// `None` off, `Some(0)` all, `Some(n)` the most recent `n`).
pub const ROW_WINDOW_ALL: u32 = 0;

/// Row count for a bare `--tail`: one comfortable screenful.
pub const ROW_WINDOW_DEFAULT: u32 = 80;

/// Hard ceiling on any row window, even [`ROW_WINDOW_ALL`], so `--tail 0`
/// cannot become an unbounded allocation. Crossing it sets
/// [`ScreenState::truncated`].
pub const ROW_WINDOW_MAX: u32 = 10_000;

/// [`ScreenState::truncated_reason`] for a row window that dropped older
/// rows. The field is an open string vocabulary; consumers MUST tolerate
/// unknown values.
pub const TRUNCATED_ROW_WINDOW: &str = "row_window";

/// A cell color: unset (terminal default), a palette index, or 24-bit RGB.
/// Tagged so JSON distinguishes "default" from "explicitly black".
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

/// OSC-133 semantic content of a cell. The server collapses `Output` (the
/// default for every cell) to `None`, so only `Input` / `Prompt` are emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticContent {
    /// Command output (never emitted; see the type-level note).
    Output,
    /// User-typed input on a command line.
    Input,
    /// Shell prompt text.
    Prompt,
}

/// Per-cell SGR attributes plus resolved colors. Underline style variants
/// collapse to one bool.
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

/// One cell's semantic + style projection. The [`ScreenState::cells`] vec is
/// sparse (only styled or marked cells), row-major, and skips wide-glyph
/// tails.
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

/// Which rows continue onto the row below (ADR-0077 §2).
///
/// From libghostty's per-row soft-wrap bit. Only the server can see the bit, so it travels and
/// joining stays consumer-side ([`ScreenState::unwrapped_rows`]). Indices are
/// ascending into their own array; a wrapped last scrollback row continues
/// into `lines[0]`.
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

/// [`RenderedScreen::format`] tag for a libghostty-vt HTML rendering.
pub const RENDERED_FORMAT_HTML: &str = "html";

/// [`RenderedScreen::format`] tag for a libghostty-vt VT rendering.
pub const RENDERED_FORMAT_VT: &str = "vt";

/// A pane rendered through libghostty-vt's own Formatter.
///
/// `phux snapshot --format html|vt`. `data` is UTF-8 for HTML and standard base64 for VT
/// (which is not guaranteed UTF-8). `format` is an open string; consumers
/// MUST tolerate unknown values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedScreen {
    /// Which rendering this is: [`RENDERED_FORMAT_HTML`] or
    /// [`RENDERED_FORMAT_VT`]. Consumers MUST tolerate an unknown value.
    pub format: String,
    /// The rendered payload; see the type-level note for the encoding.
    pub data: String,
}

/// `skip_serializing_if` helper: an untruncated read emits no key.
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
/// Every field after `lines` is additive: `#[serde(default)]` keeps older
/// payloads readable and `skip_serializing_if` keeps the default shape
/// byte-identical to the pre-addition contract.
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
    /// History rows above the viewport, oldest first, right-trimmed; empty
    /// unless requested (`--scrollback[=N]`).
    #[serde(default)]
    pub scrollback: Vec<String>,
    /// Sparse per-cell marks and styles (`--cells`); `None` unless requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cells: Option<Vec<CellInfo>>,
    /// Soft-wrap bits (ADR-0077 §2). `Some` (even empty) means the producer
    /// reported wraps; `None` means it cannot (an older server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_wrap: Option<SoftWrap>,
    /// True when the requested row window dropped older rows (ADR-0077 §3).
    /// Says nothing about rows the emulator itself evicted.
    #[serde(default, skip_serializing_if = "is_not_truncated")]
    pub truncated: bool,
    /// Why [`Self::truncated`] is true; see [`TRUNCATED_ROW_WINDOW`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated_reason: Option<String>,
    /// The pane's OSC 0/2 title at capture time, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Formatter rendering of the same capture, when requested via
    /// `GET_SCREEN`'s `format` byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<RenderedScreen>,
    /// Why [`Self::rendered`] is absent despite a requested format, when the
    /// server's render failed (non-fatal). `None` can also mean an older peer
    /// never saw the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_error: Option<String>,
}

impl Default for ScreenState {
    /// An empty 0x0 screen at the current [`SCHEMA_VERSION`].
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
            rendered: None,
            rendered_error: None,
        }
    }
}

impl ScreenState {
    /// Whether the producer reported soft-wrap information at all.
    #[must_use]
    pub const fn has_soft_wrap_info(&self) -> bool {
        self.soft_wrap.is_some()
    }

    /// Every returned row as painted: history, then viewport.
    #[must_use]
    pub fn rendered_rows(&self) -> Vec<&str> {
        self.scrollback
            .iter()
            .chain(self.lines.iter())
            .map(String::as_str)
            .collect()
    }

    /// The rows as written rather than as painted: soft-wrapped runs joined,
    /// across the history/viewport seam too. Every text-match path should
    /// use this; a match straddling a wrap is otherwise silently missed.
    /// Without wrap info the rows come back verbatim (no guessing).
    #[must_use]
    pub fn unwrapped_rows(&self) -> Vec<String> {
        let (mut rows, viewport) = self.unwrapped_split();
        rows.extend(viewport);
        rows
    }

    /// [`Self::unwrapped_rows`] split as `(scrollback, lines)`. A joined line
    /// belongs to the array it ends in. After unwrapping, `lines` no longer
    /// indexes the grid.
    #[must_use]
    pub fn unwrapped_split(&self) -> (Vec<String>, Vec<String>) {
        let split = self.scrollback.len();
        let Some(wrap) = self.soft_wrap.as_ref() else {
            return (self.scrollback.clone(), self.lines.clone());
        };

        // "row i continues onto row i + 1" over the joined stream; out-of-range
        // indices from a malformed payload are ignored.
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
            // A wrapped final row has nothing to join to; close the run.
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

/// Clamp a row stream to its most recent `want` rows ([`ROW_WINDOW_ALL`]
/// means all), capped at [`ROW_WINDOW_MAX`]. The flag reports whether any
/// older row was dropped.
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

/// Stable JSON contract version for [`RenderedFrame`], independent of
/// [`SCHEMA_VERSION`].
pub const RENDERED_SCHEMA_VERSION: u32 = 1;

/// One dense cell of the client's composited frame. `grapheme` is `" "` for
/// a blank cell and `""` for the right half of a wide glyph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedCell {
    /// The cell's grapheme cluster.
    pub grapheme: String,
    /// Resolved text-style attributes for the cell.
    pub style: CellStyle,
}

/// The client's composited multi-pane view (`phux snapshot --rendered`):
/// layout, dividers, and status bar as dense row-major cells, where
/// `(row, col)` is `cells[row * cols + col]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedFrame {
    /// Contract version; see [`RENDERED_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Composited frame width in cells.
    pub cols: u16,
    /// Composited frame height in cells.
    pub rows: u16,
    /// The composited cursor, or `None` when no pane contributes one.
    pub cursor: Option<CursorState>,
    /// Dense, row-major cells; length is exactly `cols * rows`.
    pub cells: Vec<RenderedCell>,
}

impl RenderedFrame {
    /// A blank frame of default-styled space cells and no cursor.
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

    fn owned(rows: &[&str]) -> Vec<String> {
        rows.iter().map(|s| (*s).to_owned()).collect()
    }

    /// An older payload without any additive key still deserializes, and
    /// reports no wrap information.
    #[test]
    fn deserializes_v1_json_without_additive_keys() {
        let v1 = r#"{"schema_version":1,"pane":3,"cols":80,"rows":2,"cursor":null,"lines":["hello","world"]}"#;
        let screen: ScreenState = serde_json::from_str(v1).expect("v1 JSON must deserialize");
        assert_eq!(screen.lines, owned(&["hello", "world"]));
        assert_eq!(
            screen,
            ScreenState {
                schema_version: 1,
                pane: 3,
                cols: 80,
                rows: 2,
                lines: owned(&["hello", "world"]),
                ..ScreenState::default()
            }
        );
        assert!(!screen.has_soft_wrap_info());
    }

    /// Soft-wrapped runs join into logical lines (ADR-0077 §2): within the
    /// viewport, across the scrollback seam, at the stream end, and with
    /// malformed indices ignored.
    #[test]
    fn unwraps_soft_wrapped_runs() {
        type Case<'a> = (
            &'a [&'a str],
            &'a [&'a str],
            &'a [u32],
            &'a [u32],
            &'a [&'a str],
        );
        let cases: &[Case<'_>] = &[
            (
                &[],
                &["the quick", "brown fox", "next"],
                &[],
                &[0],
                &["the quickbrown fox", "next"],
            ),
            (
                &[],
                &["aaa", "bbb", "ccc", "ddd"],
                &[],
                &[0, 1],
                &["aaabbbccc", "ddd"],
            ),
            (
                &["old", "hist"],
                &["ory", "live"],
                &[1],
                &[],
                &["old", "history", "live"],
            ),
            (&[], &["only"], &[], &[0], &["only"]),
            (&[], &["a", "b"], &[], &[], &["a", "b"]),
            (&[], &["a", "b"], &[9], &[7, 0], &["ab"]),
        ];
        for (scrollback, lines, wrapped_sb, wrapped_lines, want) in cases {
            let screen = ScreenState {
                lines: owned(lines),
                scrollback: owned(scrollback),
                soft_wrap: Some(SoftWrap {
                    lines: wrapped_lines.to_vec(),
                    scrollback: wrapped_sb.to_vec(),
                }),
                ..ScreenState::default()
            };
            assert!(screen.has_soft_wrap_info());
            assert_eq!(screen.unwrapped_rows(), owned(want), "case {lines:?}");
        }

        // A run straddling the seam lands in the half where it ends.
        let seam = ScreenState {
            lines: owned(&["ory", "live"]),
            scrollback: owned(&["old", "hist"]),
            soft_wrap: Some(SoftWrap {
                lines: Vec::new(),
                scrollback: vec![1],
            }),
            ..ScreenState::default()
        };
        assert_eq!(
            seam.unwrapped_split(),
            (owned(&["old"]), owned(&["history", "live"]))
        );
        assert_eq!(seam.rendered_rows(), vec!["old", "hist", "ory", "live"]);

        // No wrap info: rows verbatim, never heuristically joined.
        let old = ScreenState {
            lines: owned(&["the quick", "brown fox"]),
            ..ScreenState::default()
        };
        assert_eq!(old.unwrapped_rows(), owned(&["the quick", "brown fox"]));
    }

    #[test]
    fn row_window_keeps_the_tail_and_never_clips_silently() {
        let rows: Vec<String> = (0..10).map(|i| format!("row{i}")).collect();
        assert_eq!(
            row_window(rows.clone(), 3),
            (owned(&["row7", "row8", "row9"]), true)
        );
        assert_eq!(row_window(rows.clone(), 10), (rows.clone(), false));
        assert_eq!(row_window(rows.clone(), ROW_WINDOW_ALL), (rows, false));

        let over = usize::try_from(ROW_WINDOW_MAX).unwrap_or(usize::MAX) + 5;
        let rows: Vec<String> = (0..over).map(|i| format!("row{i}")).collect();
        let (window, truncated) = row_window(rows, ROW_WINDOW_ALL);
        assert_eq!(u32::try_from(window.len()).ok(), Some(ROW_WINDOW_MAX));
        assert!(truncated, "the ceiling is never silent");
    }

    /// Every additive key is absent from a default read (byte-identical to
    /// the original contract) and round-trips when populated.
    #[test]
    fn additive_keys_are_omitted_when_unset_and_round_trip_when_set() {
        let json = serde_json::to_string(&ScreenState::default()).expect("serialize");
        for key in [
            "cells",
            "soft_wrap",
            "truncated",
            "truncated_reason",
            "title",
            "rendered",
            "rendered_error",
        ] {
            assert!(
                !json.contains(&format!("\"{key}\"")),
                "{key} leaked: {json}"
            );
        }

        let full = ScreenState {
            pane: 4,
            cols: 8,
            rows: 1,
            cursor: Some(CursorState {
                x: 1,
                y: 0,
                visible: true,
            }),
            lines: owned(&["$ ls"]),
            scrollback: owned(&["old"]),
            cells: Some(vec![
                CellInfo {
                    col: 0,
                    row: 0,
                    semantic: Some(SemanticContent::Prompt),
                    style: CellStyle {
                        bold: true,
                        fg: CellColor::Rgb { r: 1, g: 2, b: 3 },
                        ..CellStyle::default()
                    },
                },
                CellInfo {
                    col: 2,
                    row: 0,
                    semantic: Some(SemanticContent::Input),
                    style: CellStyle {
                        fg: CellColor::Palette { index: 7 },
                        ..CellStyle::default()
                    },
                },
            ]),
            soft_wrap: Some(SoftWrap {
                lines: vec![0],
                scrollback: vec![0],
            }),
            truncated: true,
            truncated_reason: Some(TRUNCATED_ROW_WINDOW.to_owned()),
            title: Some("claude — phux".to_owned()),
            rendered: Some(RenderedScreen {
                format: RENDERED_FORMAT_HTML.to_owned(),
                data: "<span>hi</span>".to_owned(),
            }),
            rendered_error: Some("libghostty: out of memory".to_owned()),
            ..ScreenState::default()
        };
        let json = serde_json::to_string(&full).expect("serialize");
        assert!(json.contains("\"truncated\":true"), "got: {json}");
        assert!(
            json.contains("\"truncated_reason\":\"row_window\""),
            "got: {json}"
        );
        assert!(json.contains("\"format\":\"html\""), "got: {json}");
        let back: ScreenState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, full);
    }

    #[test]
    fn rendered_frame_is_dense_row_major_and_round_trips() {
        let mut f = RenderedFrame::blank(3, 2);
        assert_eq!(f.cells.len(), 6);
        assert_eq!(f.cell(1, 2).expect("in range").grapheme, " ");
        assert!(f.cell(2, 0).is_none() && f.cell(0, 3).is_none());
        assert!(f.cell_mut(2, 0).is_none());
        f.cell_mut(1, 2).expect("in range").grapheme = "X".to_owned();
        assert_eq!(f.cells[5].grapheme, "X");

        let json = serde_json::to_string(&f).expect("serialize");
        let back: RenderedFrame = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(f, back);
    }
}
