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

pub mod chrome;
pub mod overlay;
mod sgr;
pub mod theme;

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

/// Clip `s` to at most `max` display cells, marking the cut with
/// [`ELLIPSIS`].
///
/// The ellipsis *replaces* the last surviving character rather than being
/// appended, so the result is exactly `min(len, max)` cells wide and
/// callers can do width arithmetic on it. `max == 0` yields the empty
/// string; a string that already fits is returned untouched.
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
    if s.chars().count() <= max {
        return s.to_owned();
    }
    s.chars()
        .take(max - 1)
        .chain(std::iter::once(ELLIPSIS))
        .collect()
}

#[cfg(test)]
mod clip_text_tests {
    use super::clip_text;

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
}
