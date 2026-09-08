//! Layered render: ratatui-driven chrome composited over libghostty pane
//! interiors.
//!
//! phux-client uses two renderers with disjoint screen regions:
//!
//! - **Chrome** (this module, [`chrome`]) — ratatui paints the status bar,
//!   pane dividers, borders, and overlays. Layout math, widget composition,
//!   and modal stacking live here.
//! - **Pane interior** (outside this module) — libghostty drives VT bytes
//!   straight to stdout, preserving kitty graphics, sixel, OSC 8 hyperlinks,
//!   and the Kitty key protocol on the hot path. See `attach::render`.
//!
//! The two layers are composited, not interleaved: chrome carves skip-cell
//! rectangles for pane rects so libghostty owns those cells exclusively;
//! cursor and SGR state are explicitly handed off at the boundary.
//!
//! `ratatui` is confined to this crate (`phux-client`); the pane-interior
//! substrate lives in `phux-client-core`, which has no `ratatui`
//! dependency, so the boundary is compiler-enforced rather than grep-checked
//! (ADR-0020 replaced `scripts/check-ratatui-boundary.sh` with the crate
//! split in phux-0fv). See epic `phux-5ke` and `ADR-0020`.

pub mod breakpoints;
pub mod chrome;
pub mod overlay;
mod sgr;
pub mod theme;

pub use breakpoints::ChromeBreakpoints;

/// Color-preserving SGR emitter for chrome painted outside the ratatui-buffer
/// path (the driver's copy-mode status strip).
pub use sgr::write_sgr_color;
pub use theme::Theme;

/// The single-cell mark that says "there is more here than fits".
///
/// Shared with the status-bar composer's [`phux_config::widget::ELLIPSIS`]
/// so one glyph means one thing everywhere in the chrome: sidebar labels,
/// list rows, and status widgets all cut the same way.
pub const ELLIPSIS: char = phux_config::widget::ELLIPSIS;

/// Clip `s` to at most `max` display CELLS, marking the cut with
/// [`ELLIPSIS`].
///
/// The ellipsis *replaces* the last surviving cell rather than being
/// appended, so the result is at most `max` cells wide and callers can
/// do width arithmetic on it. `max == 0` yields the empty string; a
/// string that already fits is returned untouched.
///
/// Cells, not chars. Chrome text is arbitrary input — a window name, a
/// branch, a pane's OSC-2 title — so a CJK or emoji label measured by
/// `chars().count()` is clipped to roughly twice its budget and overruns
/// whatever was supposed to sit beside it. A double-width character that
/// would straddle the budget is dropped whole rather than half-drawn.
///
/// Zero-width characters (combining marks, variation selectors, ZWJ)
/// ride along with the character they belong to and never consume
/// budget; control characters are dropped, because chrome text reaches
/// an emitter that writes escape sequences.
///
/// Every chrome surface that shortens text goes through here. A row that
/// silently drops its tail is indistinguishable from a row whose content
/// really is that short, which is how a truncated branch name reads as a
/// different branch.
#[must_use]
pub fn clip_text(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if display_width(s) <= max {
        // One filter for both paths: a character that cannot reach the
        // wire cannot survive a clip either, whether or not the clip
        // shortened anything. `cell_width` is that filter — controls and
        // explicit bidi overrides return `None`, and a zero-width mark
        // returns `Some(0)`, so marks ride along and formatting does not.
        return s.chars().filter(|c| cell_width(*c).is_some()).collect();
    }
    // The cut costs one cell for the ellipsis.
    let budget = max - 1;
    let mut out = String::with_capacity(s.len().min(budget * 4 + 4));
    let mut used = 0usize;
    for ch in s.chars() {
        let Some(w) = cell_width(ch) else {
            continue;
        };
        if w == 0 {
            // A mark with no base yet would be a stray combining
            // character; one with a base belongs to it and is free.
            if !out.is_empty() {
                out.push(ch);
            }
            continue;
        }
        if used + w > budget {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push(ELLIPSIS);
    out
}

/// The advance width of `ch` in terminal cells, or `None` for a
/// character that must never reach a VT emitter.
///
/// Control characters are refused explicitly rather than left to
/// `unicode-width` happening to return `None` for them: chrome text is
/// untrusted (a pane names itself via OSC 2), and an `\x1b` reaching the
/// emitter would let that pane inject escape sequences into phux's own
/// chrome.
///
/// Explicit BIDI FORMATTING characters are refused for the same reason.
/// They are zero-width, so they cost no budget and no width check ever
/// notices them, but they reorder everything drawn after them: a pane
/// that titles itself `"\u{202e}..."` can make its own rail label — or its
/// tab, or its sidebar row — read as another pane's. Real
/// right-to-left text does not need them; the terminal derives direction
/// from the letters themselves, so dropping the overrides costs nothing
/// legitimate.
#[must_use]
pub fn cell_width(ch: char) -> Option<usize> {
    if ch.is_control() || is_bidi_control(ch) {
        return None;
    }
    unicode_width::UnicodeWidthChar::width(ch)
}

/// The explicit bidi formatting characters: the embedding/override set
/// (`U+202A`-`U+202E`), the isolates (`U+2066`-`U+2069`), and the
/// standalone marks (`U+061C`, `U+200E`, `U+200F`).
#[must_use]
pub const fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// The width of `s` in terminal cells, ignoring anything unprintable.
#[must_use]
pub fn display_width(s: &str) -> usize {
    s.chars().filter_map(cell_width).sum()
}

#[cfg(test)]
mod clip_text_tests {
    use super::{clip_text, display_width};

    #[test]
    fn a_string_that_fits_is_untouched() {
        assert_eq!(clip_text("branch", 6), "branch");
        assert_eq!(clip_text("branch", 99), "branch");
    }

    #[test]
    fn a_clip_lands_exactly_on_the_budget() {
        assert_eq!(clip_text("wave2/sidebar", 6), "wave2\u{2026}");
        assert_eq!(clip_text("wave2/sidebar", 6).chars().count(), 6);
        assert_eq!(clip_text("wave2/sidebar", 1), "\u{2026}");
    }

    #[test]
    fn a_zero_budget_yields_nothing() {
        assert_eq!(clip_text("anything", 0), "");
    }

    /// Char counts, not byte counts: a multi-byte label must not be cut
    /// mid-codepoint or over-budget.
    #[test]
    fn multibyte_text_is_counted_in_characters() {
        assert_eq!(clip_text("échantillon", 4), "éch\u{2026}");
        assert_eq!(clip_text("échantillon", 4).chars().count(), 4);
    }

    /// CELLS, not chars. A CJK label clipped by `chars().count()` is
    /// twice its budget wide, which is how the sidebar strip and a pane
    /// title used to overrun the columns reserved for them.
    #[test]
    fn wide_text_is_counted_in_cells() {
        // Never OVER the budget, which is the property callers rely on.
        for max in 1..12 {
            assert!(
                display_width(clip_text("日本語のテスト", max).as_str()) <= max,
                "budget {max} overrun"
            );
        }
        // A budget a whole number of glyphs fits is used exactly: 7 =
        // three double-width characters plus the ellipsis.
        assert_eq!(clip_text("日本語のテスト", 7), "日本語\u{2026}");
        // Exactly the budget: untouched, no ellipsis.
        assert_eq!(clip_text("日本語", 6), "日本語");
    }

    /// A double-width character that would straddle the last cell is
    /// dropped whole. Half a glyph is not a shorter glyph.
    #[test]
    fn a_wide_char_never_straddles_the_budget() {
        // Budget 4 = ellipsis + 3 cells: one CJK char (2) fits, the
        // second would need cells 3-4 and does not.
        let out = clip_text("日本語", 4);
        assert_eq!(out, "日\u{2026}");
        assert!(display_width(&out) <= 4);
    }

    /// Zero-width marks ride with their base and cost no budget, so a
    /// decomposed or emoji-qualified label is not truncated at the first
    /// one.
    #[test]
    fn zero_width_marks_ride_with_their_base() {
        assert_eq!(clip_text("cafe\u{301}/src", 8), "cafe\u{301}/src");
        assert_eq!(
            clip_text("\u{2705}\u{fe0f} build", 20),
            "\u{2705}\u{fe0f} build"
        );
    }

    /// Explicit bidi overrides never survive either. They are
    /// zero-width, so they cost no budget and no width check notices
    /// them, but they reorder everything drawn after them: a pane that
    /// names itself with an override can make its sidebar row or its tab
    /// read as another pane's. Real right-to-left text keeps working —
    /// the terminal derives direction from the letters themselves.
    #[test]
    fn bidi_overrides_are_dropped_but_rtl_letters_are_not() {
        assert_eq!(clip_text("run\u{202e}gpj.exe", 40), "rungpj.exe");
        assert_eq!(clip_text("\u{2066}a\u{2069}", 40), "a");
        assert_eq!(clip_text("x\u{200f}y", 40), "xy");
        // Hebrew letters are content, not formatting: untouched.
        assert_eq!(
            clip_text("\u{5e9}\u{5dc}\u{5d5}\u{5dd}", 40),
            "\u{5e9}\u{5dc}\u{5d5}\u{5dd}"
        );
        // And they cost no budget, so they cannot shift a cut.
        assert_eq!(clip_text("abcdef", 4), clip_text("abc\u{202e}def", 4));
    }

    /// Control characters never survive: chrome text reaches an emitter
    /// that writes escape sequences, and a pane names itself.
    #[test]
    fn control_characters_are_dropped() {
        assert_eq!(clip_text("a\u{1b}[31mb", 40), "a[31mb");
        assert_eq!(clip_text("a\u{7}b", 40), "ab");
        assert!(!clip_text("x\u{1b}]0;evil\u{7}y", 3).contains('\u{1b}'));
    }
}
