//! The responsive-chrome breakpoints, as one value threaded from the driver.
//!
//! The chrome adapts to small terminals around a handful of thresholds
//! (`docs/consumers/tui.md` §4.5). They used to be `const`s read at the use
//! sites, which made them correct-by-default and untunable: a user on an
//! unusual geometry — a 100-column terminal who wants full-bleed pickers, a
//! 55-column one who wants to keep the sidebar — had no knob (phux-huhi).
//!
//! They are now a plain `Copy` value built once per attach from `[chrome]`
//! and threaded to every layout site, exactly like the sidebar reservation
//! and the [`Theme`]: one snapshot, taken where the config is loaded, so the
//! status bar, the sidebar, and every overlay cannot disagree about what
//! "compact" means on the same frame.
//!
//! [`Theme`]: crate::render::Theme

use phux_config::ChromeCfg;

/// Column and row thresholds the whole chrome shares.
///
/// [`Default`] reproduces the shipped constants exactly, so every
/// construction site that has no config (tests, the pre-config bootstrap
/// frame) keeps the historical behaviour byte-for-byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromeBreakpoints {
    /// Viewport width at or below which the chrome is *column-starved*: a
    /// floating box's margins stop reading as composition and start reading
    /// as columns you cannot use.
    ///
    /// The shipped 64 is chosen from the content, not from a round number:
    /// a 60% box in 64 columns is 38 wide, and once its two border columns
    /// and the two-column indent of a nested picker row are taken out, 34
    /// remain — under the width at which a `session/window` pair plus its
    /// branch stays legible. Wider than this and the margins are affordable.
    pub compact_cols: u16,

    /// Viewport height at or below which the chrome is *row-starved*.
    ///
    /// The shipped 18: a 60% box in 18 rows is 10 tall, and the shared modal
    /// chrome (border, query line and its blank, footer and its blank)
    /// spends 6 of them, so four rows of an actual list survive. Below that
    /// a picker shows less than a page and scrolling replaces reading.
    pub compact_rows: u16,

    /// The narrowest pane area worth tiling into.
    ///
    /// The shipped 40 is half a classic 80-column terminal, and about where
    /// an editor, a diff, or an agent's output stops being readable rather
    /// than merely cramped. The sidebar strip is not reserved at all below
    /// `sidebar width + this`.
    pub min_pane_cols: u16,
}

impl ChromeBreakpoints {
    /// The shipped thresholds, as a `const` so `const fn` constructors
    /// (notably [`OverlayState::new`]) can start from them.
    ///
    /// These are the numbers `[chrome]`'s serde defaults also carry; a unit
    /// test below pins the two copies together.
    ///
    /// [`OverlayState::new`]: crate::render::overlay::OverlayState::new
    pub const DEFAULT: Self = Self {
        compact_cols: 64,
        compact_rows: 18,
        min_pane_cols: 40,
    };

    /// Snapshot `[chrome]` into the value the chrome threads around.
    ///
    /// Total: every field of [`ChromeCfg`] has a serde default, so an absent
    /// section, a partial one, and no config file at all all land on
    /// [`Self::default`].
    #[must_use]
    pub const fn from_cfg(cfg: &ChromeCfg) -> Self {
        Self {
            compact_cols: cfg.compact_cols,
            compact_rows: cfg.compact_rows,
            min_pane_cols: cfg.min_pane_cols,
        }
    }

    /// Whether a viewport `cols` wide is column-starved.
    #[must_use]
    pub const fn is_col_starved(self, cols: u16) -> bool {
        cols <= self.compact_cols
    }

    /// Whether a viewport `rows` tall is row-starved.
    #[must_use]
    pub const fn is_row_starved(self, rows: u16) -> bool {
        rows <= self.compact_rows
    }
}

impl Default for ChromeBreakpoints {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[cfg(test)]
mod tests {
    use super::ChromeBreakpoints;
    use phux_config::ChromeCfg;

    /// The historical constants are the shipped defaults, and the schema
    /// agrees with the render-side fallback. Two places name these numbers
    /// (the serde defaults and [`ChromeBreakpoints::default`]); this is the
    /// test that keeps them the same numbers.
    #[test]
    fn the_defaults_are_the_historical_constants() {
        let bp = ChromeBreakpoints::default();
        assert_eq!(bp.compact_cols, 64);
        assert_eq!(bp.compact_rows, 18);
        assert_eq!(bp.min_pane_cols, 40);
        assert_eq!(bp, ChromeBreakpoints::from_cfg(&ChromeCfg::default()));
    }

    /// The thresholds are inclusive: a viewport *at* the breakpoint is
    /// already starved, matching `outer.width <= COMPACT_COLS`.
    #[test]
    fn the_thresholds_are_inclusive() {
        let bp = ChromeBreakpoints::default();
        assert!(bp.is_col_starved(64));
        assert!(!bp.is_col_starved(65));
        assert!(bp.is_row_starved(18));
        assert!(!bp.is_row_starved(19));
    }

    /// `0` disables a threshold rather than meaning "always": no viewport
    /// has fewer than zero columns, so nothing is ever starved.
    #[test]
    fn a_zero_threshold_never_fires_on_a_real_viewport() {
        let bp = ChromeBreakpoints {
            compact_cols: 0,
            compact_rows: 0,
            min_pane_cols: 0,
        };
        assert!(!bp.is_col_starved(1));
        assert!(!bp.is_row_starved(1));
    }
}
