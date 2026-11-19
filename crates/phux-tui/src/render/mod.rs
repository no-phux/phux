//! Layered render: ratatui chrome composited over libghostty pane interiors
//! (`attach::render`). Chrome carves skip cells for pane rects, so the
//! layers never interleave (ADR-0020).

pub mod breakpoints;
pub mod chrome;
pub mod kit;
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
pub use chrome::status_bar::widgets::ELLIPSIS;

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

/// `path` as the shell shows it: `~/rest` under `home` (at a `/` boundary
/// only), `~` for an empty path.
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
/// in the last cell. Cells, not chars: a wide glyph straddling the budget is
/// dropped whole, and controls and bidi overrides are dropped.
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
pub use chrome::status_bar::widgets::{cell_width, display_width, is_bidi_control};

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

    /// Exact clips, never over the budget in cells. A fitting string is
    /// untouched; a clip lands exactly on the budget with an ellipsis; a
    /// zero budget yields nothing. Multi-byte text counts characters, not
    /// bytes, and a double-width character that would straddle the last
    /// cell is dropped whole. Zero-width marks ride with their base. Bidi
    /// overrides and control characters are dropped (chrome text reaches an
    /// emitter that writes escape sequences), but Hebrew letters are content.
    #[test]
    fn clips_are_exact_and_within_the_budget() {
        #[rustfmt::skip] // a case table, one row per case
        let cases = [
            ("fits exactly", "branch", 6, "branch"),
            ("fits with room", "branch", 99, "branch"),
            ("clipped to the budget", "wave2/sidebar", 6, "wave2\u{2026}"),
            ("budget of one", "wave2/sidebar", 1, "\u{2026}"),
            ("zero budget", "anything", 0, ""),
            ("multibyte", "échantillon", 4, "éch\u{2026}"),
            ("wide straddle", "日本語", 4, "日\u{2026}"),
            ("combining mark", "cafe\u{301}/src", 8, "cafe\u{301}/src"),
            ("emoji presentation", "\u{2705}\u{fe0f} build", 20, "\u{2705}\u{fe0f} build"),
            ("rlo override", "run\u{202e}gpj.exe", 40, "rungpj.exe"),
            ("isolates", "\u{2066}a\u{2069}", 40, "a"),
            ("rlm mark", "x\u{200f}y", 40, "xy"),
            ("hebrew", "\u{5e9}\u{5dc}\u{5d5}\u{5dd}", 40, "\u{5e9}\u{5dc}\u{5d5}\u{5dd}"),
            ("csi", "a\u{1b}[31mb", 40, "a[31mb"),
            ("bel", "a\u{7}b", 40, "ab"),
        ];
        for (case, text, max, want) in cases {
            let out = clip_text(text, max);
            assert_eq!(out, want, "{case}");
            assert!(display_width(&out) <= max, "{case}: over budget");
        }
        // Overrides cost no budget, so they cannot shift a cut.
        assert_eq!(clip_text("abcdef", 4), clip_text("abc\u{202e}def", 4));
        assert!(!clip_text("x\u{1b}]0;evil\u{7}y", 3).contains('\u{1b}'));
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
}
