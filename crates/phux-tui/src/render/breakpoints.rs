//! The responsive-chrome breakpoints (`docs/consumers/tui.md` §4.5).
//!
//! One `Copy` value built per attach from `[chrome]` and threaded to every
//! layout site, so the bar, sidebar, and overlays agree on "compact".

use phux_config::ChromeCfg;

/// Column and row thresholds the whole chrome shares. [`Default`] is the
/// shipped values, for sites with no config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromeBreakpoints {
    /// Width at or below which the chrome is column-starved. 64: a 60% box
    /// there leaves 34 columns of picker text, under a legible
    /// `session/window` plus branch.
    pub compact_cols: u16,

    /// Height at or below which the chrome is row-starved. 18: a 60% box
    /// there keeps four list rows after the shared modal chrome.
    pub compact_rows: u16,

    /// The narrowest pane area worth tiling into (40, half a classic
    /// terminal); no sidebar is reserved below `sidebar width + this`.
    pub min_pane_cols: u16,
}

impl ChromeBreakpoints {
    /// The shipped thresholds as a `const` (a test pins them to `[chrome]`'s
    /// serde defaults).
    pub const DEFAULT: Self = Self {
        compact_cols: 64,
        compact_rows: 18,
        min_pane_cols: 40,
    };

    /// Snapshot `[chrome]`; absent fields take their serde defaults.
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

    /// The serde defaults and [`ChromeBreakpoints::default`] name the same
    /// numbers.
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
