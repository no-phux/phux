//! Integration test for the phux-nz4.5 status-bar pipeline:
//! TOML string → `Config` → `StatusBar` → painter → assert widget
//! output landed on the bottom row.
//!
//! Tests the seams the user cares about: the two in-tree widgets
//! (`time`, `session-name`) must appear when included in `[status]`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::time::SystemTime;

use phux_config::widget::{StatusBar, WidgetRegistry};
use phux_tui::attach::status_bar::{BarInset, Position, StatusBarPainter, make_context};

const CONFIG_BOTH_WIDGETS: &str = r#"
[status]
left = [{ kind = "session-name", prefix = "[" }]
right = [{ kind = "time", format = "FAKE-CLOCK" }]
"#;

#[test]
fn both_in_tree_widgets_render_to_bottom_row_from_config() {
    let cfg =
        phux_config::parse_str(CONFIG_BOTH_WIDGETS, &PathBuf::from("test.toml")).expect("parse");
    let registry = WidgetRegistry::with_builtins();
    let bar = StatusBar::build(&cfg.status, &registry).expect("build");
    assert!(!bar.is_empty());

    let mut painter = StatusBarPainter::new(bar, Position::default());
    let mut buf: Vec<u8> = Vec::new();
    let cols: u16 = 40;
    let rows: u16 = 24;
    painter
        .paint(
            &mut buf,
            BarInset::NONE,
            cols,
            rows,
            &make_context("session-x", SystemTime::UNIX_EPOCH),
        )
        .expect("paint");

    let s = String::from_utf8(buf).expect("utf8");

    // CUP to last row (`rows`, 1-based).
    assert!(s.contains("\x1b[24;1H"), "no CUP to bottom row: {s:?}");
    // session-name widget output (with `[` prefix).
    assert!(s.contains("[session-x"), "session widget missing: {s:?}");
    // time widget output (literal because the format has no `%` escapes).
    assert!(s.contains("FAKE-CLOCK"), "time widget missing: {s:?}");
}

#[test]
fn default_placement_is_bottom() {
    // No config knob for top/bottom exists today; the painter's
    // Position::default() must be Bottom per docs/consumers/tui.md §8.5.
    let cfg =
        phux_config::parse_str(CONFIG_BOTH_WIDGETS, &PathBuf::from("test.toml")).expect("parse");
    let registry = WidgetRegistry::with_builtins();
    let bar = StatusBar::build(&cfg.status, &registry).expect("build");

    let mut painter = StatusBarPainter::new(bar, Position::default());
    let mut buf: Vec<u8> = Vec::new();
    painter
        .paint(
            &mut buf,
            BarInset::NONE,
            40,
            10,
            &make_context("s", SystemTime::UNIX_EPOCH),
        )
        .expect("paint");
    let s = String::from_utf8(buf).expect("utf8");
    // Row 10 = bottom of a 10-row viewport, 1-based.
    assert!(s.contains("\x1b[10;1H"), "default not bottom: {s:?}");
    assert!(!s.contains("\x1b[1;1H"), "must not target row 1: {s:?}");
}

#[test]
fn empty_status_section_yields_no_paint() {
    let cfg = phux_config::parse_str("", &PathBuf::from("empty.toml")).expect("parse");
    let registry = WidgetRegistry::with_builtins();
    let bar = StatusBar::build(&cfg.status, &registry).expect("build");
    assert!(bar.is_empty(), "default status should be empty");
    let mut painter = StatusBarPainter::new(bar, Position::default());
    let mut buf: Vec<u8> = Vec::new();
    painter
        .paint(
            &mut buf,
            BarInset::NONE,
            40,
            10,
            &make_context("anything", SystemTime::UNIX_EPOCH),
        )
        .expect("paint");
    assert!(
        buf.is_empty(),
        "empty bar must emit zero bytes (no chrome reservation)"
    );
}
