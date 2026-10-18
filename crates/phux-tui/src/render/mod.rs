//! Layered render: ratatui chrome ([`chrome`], overlays) composited over
//! libghostty pane interiors, which write VT straight to stdout (see
//! `attach::render`).
//!
//! Chrome carves skip cells for pane rects, so the layers
//! never interleave; cursor and SGR are handed off at the boundary. `ratatui`
//! lives only in this crate (ADR-0020).

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

/// The single-cell "there is more here than fits" mark, shared with the
/// status-bar composer so every surface cuts the same way.
pub const ELLIPSIS: char = phux_config::widget::ELLIPSIS;

/// Clip `text` from the left to at most `max` display cells, led by
/// [`ELLIPSIS`] when cut, so the tail stays: a path keeps its deepest
/// directory, a file its name.
#[must_use]
pub fn clip_left(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    let mut tail: Vec<char> = Vec::new();
    let mut used = 1;
    for ch in text.chars().rev() {
        let w = cell_width(ch).unwrap_or(0);
        if used + w > max {
            break;
        }
        used += w;
        tail.push(ch);
    }
    tail.push(ELLIPSIS);
    tail.into_iter().rev().collect()
}

/// `path` as the shell shows it: `home` becomes `~`; an empty path means home.
///
/// A path under home becomes `~/rest`. A sibling that merely shares the
/// prefix (`/Users/pat` under home `/Users/p`) is left alone, since the match
/// must end at a `/` boundary. `home` is a parameter so callers read `$HOME`
/// once and tests pass their own.
#[must_use]
pub fn home_to_tilde(path: &str, home: Option<&str>) -> String {
    if path.is_empty() {
        return "~".to_owned();
    }
    let Some(home) = home
        .map(|h| h.trim_end_matches('/'))
        .filter(|h| !h.is_empty())
    else {
        return path.to_owned();
    };
    match path.strip_prefix(home) {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

/// Clip `s` to at most `max` display cells, marking a cut with [`ELLIPSIS`]
/// (which replaces the last cell, so the result never exceeds `max`).
///
/// Cells, not chars: chrome text is arbitrary input, and a CJK label
/// measured by chars overruns its neighbours; a wide glyph straddling the
/// budget is dropped whole. Zero-width marks ride with their base for free;
/// controls and bidi overrides are dropped (see [`cell_width`]). Every chrome
/// surface shortens text here, so a cut is always marked.
#[must_use]
pub fn clip_text(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if display_width(s) <= max {
        // The same filter as the clip path: unprintable characters never
        // survive, marks do.
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

/// Chrome measures text with the status bar's rules: one definition of what
/// is printable and how wide it is.
pub use phux_config::widget::{cell_width, display_width, is_bidi_control};

#[cfg(test)]
mod home_to_tilde_tests {
    use super::home_to_tilde;

    #[test]
    fn home_becomes_tilde_at_a_path_boundary_only() {
        assert_eq!(home_to_tilde("/Users/p", Some("/Users/p")), "~");
        assert_eq!(home_to_tilde("/Users/p/src/x", Some("/Users/p")), "~/src/x");
        assert_eq!(home_to_tilde("/Users/p/src", Some("/Users/p/")), "~/src");
        assert_eq!(
            home_to_tilde("/Users/pat/x", Some("/Users/p")),
            "/Users/pat/x"
        );
        assert_eq!(home_to_tilde("/Users/p/x", None), "/Users/p/x");
        assert_eq!(home_to_tilde("", Some("/Users/p")), "~");
    }
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
    /// twice its budget wide and overruns the columns the sidebar strip or a
    /// pane title reserved.
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

    /// Bidi overrides never survive a clip either (they reorder later text).
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
