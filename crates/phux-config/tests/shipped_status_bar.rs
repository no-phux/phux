//! What the shipped status bar actually looks like, at the widths people
//! actually run.
//!
//! Every other test in this crate exercises one widget or one rule. This
//! one renders the *default lineup* — the exact `[status]` block in
//! `default.toml` — across a ladder of terminal widths, and pins the
//! whole row. It is the only test that would notice a change that is
//! individually correct in every widget and collectively unreadable.

#![allow(clippy::expect_used, reason = "tests")]

use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use phux_config::widget::{
    CellHit, StatusBar, WidgetContext, WidgetRegistry, WindowInfo, row_to_string,
};

fn win(name: &str, active: bool) -> WindowInfo {
    WindowInfo {
        name: name.to_owned(),
        active,
        zoomed: false,
        attention: false,
        branch: None,
    }
}

/// The bar built from the shipped defaults, rendered at `width`.
fn shipped_row(
    width: u16,
    session: &str,
    windows: &[WindowInfo],
) -> Vec<phux_config::widget::Cell> {
    let cfg = phux_config::parse_with_defaults("", Path::new("/nonexistent/config.toml"))
        .expect("shipped defaults parse");
    let bar = StatusBar::build(&cfg.status, &WidgetRegistry::with_builtins())
        .expect("the shipped status lineup must build");
    // A fixed instant so the clock is deterministic; the `time` widget's
    // own formatting is covered elsewhere, here it only has to occupy a
    // stable number of columns.
    let ctx = WidgetContext::new(
        UNIX_EPOCH + Duration::from_secs(12_345),
        session,
        "C-a",
        windows,
    );
    bar.render(&ctx, width)
}

fn shipped_text(width: u16, session: &str, windows: &[WindowInfo]) -> String {
    row_to_string(&shipped_row(width, session, windows))
}

/// Whatever else changes, the row is exactly as wide as the terminal —
/// no short row (which leaves stale cells from the last paint) and no
/// long one (which wraps onto the pane above).
#[test]
fn the_shipped_bar_is_exactly_the_terminal_width_at_every_size() {
    let windows = [
        win("zsh", false),
        win("nvim", true),
        win("server", false),
        win("logs", false),
    ];
    for width in 1u16..=200 {
        let row = shipped_row(width, "phux", &windows);
        assert_eq!(row.len(), usize::from(width), "row length at width {width}");
        assert_eq!(
            row_to_string(&row).chars().count(),
            usize::from(width),
            "painted width at {width}"
        );
    }
}

/// A roomy terminal shows everything: all four padded tabs, the hints,
/// the session name and the clock.
#[test]
fn a_roomy_terminal_shows_the_whole_lineup() {
    let windows = [win("zsh", false), win("nvim", true), win("server", false)];
    let text = shipped_text(120, "phux", &windows);

    assert!(text.starts_with(" 0:zsh  1:nvim  2:server "), "{text:?}");
    assert!(text.contains("C-a  Space palette"), "{text:?}");
    assert!(text.contains("phux"), "{text:?}");
    // The `switch` chip is for narrow terminals only.
    assert!(!text.contains("switch"), "{text:?}");
}

/// A narrow terminal trades ambient context for an affordance: the clock
/// and session name step aside, the tab strip collapses around the active
/// tab, and a clickable `switch` chip appears.
#[test]
fn a_narrow_terminal_trades_context_for_an_affordance() {
    let windows = [
        win("zsh", false),
        win("nvim", true),
        win("server", false),
        win("logs", false),
    ];
    let text = shipped_text(46, "phux", &windows);

    assert!(text.contains(" switch "), "{text:?}");
    assert!(!text.contains("C-a"), "hints yield first: {text:?}");
    // The active tab is always visible, whole.
    assert!(text.contains("1:nvim"), "{text:?}");
    // And no tab is half-drawn: every window name present is complete.
    assert!(!text.contains('\u{2026}'), "no clipped tab: {text:?}");
}

/// The chip is a click target across its whole painted width, padding
/// included — and nothing else on the row claims to be one.
#[test]
fn the_switch_chip_is_clickable_across_its_padding() {
    let windows = [win("nvim", true)];
    let row = shipped_row(46, "phux", &windows);
    let switch_cols: Vec<usize> = row
        .iter()
        .enumerate()
        .filter(|(_, c)| c.hit == Some(CellHit::Switch))
        .map(|(i, _)| i)
        .collect();

    // " switch " is 8 cells, flush against the right edge.
    assert_eq!(switch_cols.len(), 8, "{switch_cols:?}");
    assert_eq!(*switch_cols.last().expect("non-empty"), 45);
    // Contiguous: no inert gap inside the target.
    for pair in switch_cols.windows(2) {
        assert_eq!(pair[1], pair[0] + 1, "{switch_cols:?}");
    }
}

/// The breakpoint is a clean switchover, not an overlap or a gap: at
/// every width one of the two shapes is in effect and never both.
#[test]
fn the_two_right_slot_shapes_never_overlap() {
    let windows = [win("nvim", true)];
    for width in 20u16..=120 {
        let text = shipped_text(width, "sess", &windows);
        let has_chip = text.contains(" switch ");
        let has_clock = text.contains(':') && text.contains("sess");
        assert!(
            !(has_chip && has_clock),
            "both shapes at width {width}: {text:?}"
        );
        if width <= 64 {
            assert!(has_chip, "no chip at width {width}: {text:?}");
        }
    }
}

/// Down to a genuinely tiny grid the bar still degrades rather than
/// breaking: no panic, no overrun, and the active window's index — the
/// thing you navigate by — is the last thing to go.
#[test]
fn a_tiny_grid_still_degrades_instead_of_breaking() {
    let windows = [
        win("zsh", false),
        win("a-very-long-window-name", true),
        win("logs", false),
    ];
    for width in 1u16..=20 {
        let text = shipped_text(width, "a-long-session-name", &windows);
        assert_eq!(text.chars().count(), usize::from(width));
    }
    // At 12 columns the active tab's index survives.
    assert!(shipped_text(12, "s", &windows).contains('1'));
}
