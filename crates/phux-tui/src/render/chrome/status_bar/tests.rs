//! Status-bar painter tests: rows, caching, widgets, insets, and themes.

use std::time::Duration;

use phux_config::widget::CellStyle;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

#[allow(clippy::wildcard_imports)]
use super::cells::*;
use super::*;
use phux_config::widget::WidgetRegistry;
use phux_config::{StatusCfg, Widget, WidgetSpec};
use std::time::UNIX_EPOCH;

fn ctx_default(session: &str) -> StatusBarContext<'_> {
    make_context(session, UNIX_EPOCH)
}

/// The columns an emitted row advances the terminal, walked the way
/// `write_buffer` does (by each symbol's display width).
fn emitted_columns(buffer: &Buffer, cols: u16) -> u16 {
    let mut x = 0u16;
    let mut columns = 0u16;
    while x < cols {
        let sym = buffer[(x, 0)].symbol();
        let w = u16::try_from(crate::render::display_width(sym))
            .unwrap_or(1)
            .max(1);
        columns = columns.saturating_add(w);
        x = x.saturating_add(w);
    }
    columns
}

fn spec(kind: &str, opts: &[(&str, toml::Value)]) -> Widget {
    Widget::Spec(WidgetSpec {
        kind: kind.to_owned(),
        opts: opts
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    })
}

fn build_bar(cfg: &StatusCfg) -> StatusBar {
    StatusBar::build(cfg, &WidgetRegistry::with_builtins()).unwrap()
}

/// A painter with just the `session-name` widget on the left.
fn session_bar(position: Position) -> StatusBarPainter {
    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".into())],
        ..Default::default()
    };
    StatusBarPainter::new(build_bar(&cfg), position)
}

fn windows_bar() -> StatusBarPainter {
    let cfg = StatusCfg {
        left: vec![spec("windows", &[])],
        ..StatusCfg::default()
    };
    StatusBarPainter::new(build_bar(&cfg), Position::Bottom)
}

fn wins(names: &[(&str, bool)]) -> Vec<WindowInfo> {
    names
        .iter()
        .map(|(name, active)| WindowInfo {
            name: (*name).to_owned(),
            active: *active,
            ..WindowInfo::default()
        })
        .collect()
}

/// One paint as raw VT.
fn paint(p: &mut StatusBarPainter, inset: BarInset, cols: u16, rows: u16, session: &str) -> String {
    let mut buf = Vec::new();
    p.paint(&mut buf, inset, cols, rows, &ctx_default(session))
        .unwrap();
    String::from_utf8(buf).unwrap()
}

/// Strip CSI escapes (styled cells interleave SGR between glyphs).
fn strip_csi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for n in chars.by_ref() {
                if ('@'..='~').contains(&n) {
                    break;
                }
            }
        } else if c != '\x1b' {
            out.push(c);
        }
    }
    out
}

fn visible(p: &mut StatusBarPainter, cols: u16, rows: u16, session: &str) -> String {
    strip_csi(&paint(p, BarInset::NONE, cols, rows, session))
}

#[test]
fn position_picks_the_bar_row() {
    assert_eq!(
        Position::from(phux_config::StatusPosition::Bottom),
        Position::Bottom
    );
    assert_eq!(
        Position::from(phux_config::StatusPosition::Top),
        Position::Top
    );
    for (position, cup) in [
        (Position::Bottom, "\x1b[24;1H"),
        (Position::Top, "\x1b[1;1H"),
    ] {
        let mut p = session_bar(position);
        assert_eq!(p.position(), position);
        let s = paint(&mut p, BarInset::NONE, 10, 24, "hi");
        assert!(s.contains(cup) && s.contains("hi"), "{s:?}");
    }
}

/// Unchanged inputs paint nothing; a viewport, context, or invalidate
/// repaints; zero dims and an empty bar never paint.
#[test]
fn the_row_cache_repaints_only_on_change() {
    let mut p = session_bar(Position::Bottom);
    assert!(!paint(&mut p, BarInset::NONE, 10, 24, "x").is_empty());
    assert!(paint(&mut p, BarInset::NONE, 10, 24, "x").is_empty());
    assert!(!paint(&mut p, BarInset::NONE, 20, 24, "x").is_empty());
    assert!(!paint(&mut p, BarInset::NONE, 20, 24, "y").is_empty());
    p.invalidate();
    assert!(!paint(&mut p, BarInset::NONE, 20, 24, "y").is_empty());
    assert!(paint(&mut p, BarInset::NONE, 0, 24, "x").is_empty());
    assert!(paint(&mut p, BarInset::NONE, 80, 0, "x").is_empty());
    assert_eq!(p.min_poll_interval(), Some(Duration::from_secs(1)));

    let mut empty = StatusBarPainter::new(build_bar(&StatusCfg::default()), Position::Bottom);
    assert!(empty.is_empty());
    assert_eq!(empty.min_poll_interval(), None);
    assert!(paint(&mut empty, BarInset::NONE, 80, 24, "").is_empty());
}

#[test]
fn time_and_session_both_appear_when_configured() {
    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".into())],
        right: vec![spec(
            "time",
            &[("format", toml::Value::String("LITERAL".into()))],
        )],
        ..Default::default()
    };
    let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
    let s = paint(&mut p, BarInset::NONE, 30, 24, "main");
    assert!(s.contains("main") && s.contains("LITERAL"), "{s:?}");
}

/// The config-error painter reserves the row and keeps repainting it
/// (polling, repainting after invalidate), with every column inert.
#[test]
fn error_line_painter_holds_the_bar_row() {
    let mut p = StatusBarPainter::error_line("config error: boom (run: phux config check)");
    assert!(!p.is_empty());
    assert_eq!(p.min_poll_interval(), Some(Duration::from_secs(1)));
    let s = paint(&mut p, BarInset::NONE, 80, 24, "");
    assert!(s.contains("\x1b[1;1H"), "{s:?}");
    let printable = strip_csi(&s);
    assert!(printable.contains("config error") && printable.contains("phux config check"));
    assert!(paint(&mut p, BarInset::NONE, 80, 24, "").is_empty());
    p.invalidate();
    assert!(!paint(&mut p, BarInset::NONE, 80, 24, "").is_empty());
    assert!((0..80).all(|x| p.window_hit_at(x).is_none()));
}

/// Painter-owned focused-pane state feeds the `cwd` and `exit` widgets;
/// exec feed output lands on the next paint.
#[test]
fn painter_state_feeds_cwd_exit_and_exec_widgets() {
    let cfg = StatusCfg {
        left: vec![
            spec("cwd", &[]),
            spec(
                "exec",
                &[("command", toml::Value::String("battery.sh".into()))],
            ),
        ],
        right: vec![spec(
            "exit",
            &[("format", toml::Value::String("rc={code}".into()))],
        )],
        ..Default::default()
    };
    let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
    let feeds = p.exec_feeds();
    assert_eq!(feeds.len(), 1);
    let v = visible(&mut p, 60, 24, "");
    assert!(
        !v.contains("/tmp") && !v.contains("rc=") && !v.contains("BAT"),
        "{v:?}"
    );

    assert!(p.set_focused_cwd(Some("/tmp/project".to_owned())));
    assert!(!p.set_focused_cwd(Some("/tmp/project".to_owned())));
    assert!(p.set_last_exit(Some(127)));
    feeds[0].apply_output("BAT 87%\n");
    let v = visible(&mut p, 60, 24, "");
    assert!(
        v.contains("/tmp/project") && v.contains("rc=127") && v.contains("BAT 87%"),
        "{v:?}"
    );

    assert!(p.set_last_exit(None));
    assert!(!visible(&mut p, 60, 24, "").contains("rc="));
}

fn tab_underlined(buf: &Buffer, start: u16, end: u16) -> bool {
    (start..=end).all(|x| buf[(x, 0)].modifier.contains(Modifier::UNDERLINED))
}

/// "0:bash 1:vim" puts window 0 on columns 0..=5 and window 1 on
/// 7..=11; separator and padding are inert. The map follows repaints and
/// a live drag marker never steals hits.
#[test]
fn window_hit_at_maps_painted_tab_columns() {
    let mut p = windows_bar();
    p.set_windows(wins(&[("bash", true), ("vim", false)]));
    assert_eq!(p.window_hit_at(0), None, "nothing painted yet");
    paint(&mut p, BarInset::NONE, 40, 10, "");
    let expect = |x: u16| match x {
        0..=5 => Some(0),
        7..=11 => Some(1),
        _ => None,
    };
    for x in 0..=40 {
        assert_eq!(p.window_hit_at(x), expect(x), "col {x}");
    }

    let ctx = ctx_default("");
    let compose =
        |p: &mut StatusBarPainter| p.compose_buffer(BarInset::NONE, 40, 10, &ctx).unwrap().0;
    assert!(!tab_underlined(&compose(&mut p), 7, 11));
    assert!(p.set_drop_index(Some(1)));
    assert!(!p.set_drop_index(Some(1)));
    let marked = compose(&mut p);
    assert!(tab_underlined(&marked, 7, 11) && !tab_underlined(&marked, 0, 5));
    paint(&mut p, BarInset::NONE, 40, 10, "");
    assert!((7..=11).all(|x| p.window_hit_at(x) == Some(1)));
    assert!(p.set_drop_index(None));
    assert!(!tab_underlined(&compose(&mut p), 7, 11));

    p.set_windows(wins(&[("a", false), ("b", true)]));
    paint(&mut p, BarInset::NONE, 40, 10, "");
    assert_eq!(p.window_hit_at(4), Some(1), "the fresh paint's tabs");
}

/// A left sidebar shifts the bar beside the strip (hits map through the
/// shifted origin); a right one narrows it in place, badge included.
#[test]
fn sidebar_insets_shift_or_narrow_the_bar() {
    let mut p = windows_bar();
    p.set_windows(wins(&[("bash", true), ("vim", false)]));
    let s = paint(&mut p, BarInset { left: 20, right: 0 }, 40, 10, "");
    assert!(
        s.contains("\x1b[10;21H") && !s.contains("\x1b[10;1H"),
        "{s:?}"
    );
    assert_eq!(
        p.last_row.as_ref().map(|(x, w, _)| (*x, *w)),
        Some((20, 20))
    );
    for x in 0..40 {
        let expect = match x {
            20..=25 => Some(0),
            27..=31 => Some(1),
            _ => None,
        };
        assert_eq!(p.window_hit_at(x), expect, "col {x}");
    }

    let mut p = windows_bar();
    p.set_windows(wins(&[("bash", true)]));
    p.set_supervisory(Some("[F]".to_owned()));
    let s = paint(&mut p, BarInset { left: 0, right: 20 }, 40, 10, "");
    assert!(
        s.contains("\x1b[10;1H") && s.contains("\x1b[10;18H\x1b[7;1m\x1b[38;2;190;242;100m[F]"),
        "{s:?}"
    );
    assert_eq!(p.last_row.as_ref().map(|(x, w, _)| (*x, *w)), Some((0, 20)));
}

#[test]
fn painter_threads_configured_prefix_to_help_hints_widget() {
    let cfg = StatusCfg {
        center: vec![spec("help-hints", &[])],
        ..Default::default()
    };
    let mut painter = StatusBarPainter::new(build_bar(&cfg), Position::Bottom);
    painter.set_prefix("C-b");
    let v = visible(&mut painter, 80, 24, "");
    assert!(v.contains("C-b  s Sessions") && !v.contains("C-a"), "{v:?}");
}

/// Cells a widget leaves without a background take the bar fill; a tab
/// with its own keeps it.
#[test]
fn the_bar_takes_its_fill_where_widgets_leave_no_background() {
    let bed = toml::Value::Table(toml::value::Table::from_iter([(
        "bg".to_owned(),
        toml::Value::String("#293628".to_owned()),
    )]));
    let cfg = StatusCfg {
        left: vec![spec("windows", &[("active", bed)])],
        right: vec![spec("session-name", &[])],
        ..StatusCfg::default()
    };
    let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Top);
    p.set_windows(wins(&[("zsh", true)]));
    let fill = Color::Rgb(0x17, 0x1b, 0x23);
    p.set_theme(&Theme {
        surface: fill,
        ..Theme::default()
    });
    let (buf, _, _) = p
        .compose_buffer(BarInset::NONE, 40, 10, &ctx_default("main"))
        .expect("composes");
    assert_eq!(buf[(0, 0)].bg, Color::Rgb(0x29, 0x36, 0x28));
    assert!((10..40).all(|x| buf[(x, 0)].bg == fill));
}

/// A `theme.<slot>` widget colour resolves against the painter's theme;
/// an unknown slot degrades to the terminal default.
#[test]
fn theme_slot_colors_resolve_against_the_theme() {
    let theme = Theme {
        accent: Color::Rgb(1, 2, 3),
        selection_bg: Color::Rgb(4, 5, 6),
        ..Theme::default()
    };
    let style = CellStyle {
        fg: Some("theme.accent".to_owned()),
        bg: Some("theme.selection_bg".to_owned()),
        ..CellStyle::default()
    };
    let resolved = to_ratatui_style(&style, &theme);
    assert_eq!(resolved.fg, Some(Color::Rgb(1, 2, 3)));
    assert_eq!(resolved.bg, Some(Color::Rgb(4, 5, 6)));

    let unknown = CellStyle {
        fg: Some("theme.nope".to_owned()),
        ..CellStyle::default()
    };
    assert_eq!(to_ratatui_style(&unknown, &theme).fg, None);
}

/// A window tab styled `theme.accent` follows `set_theme`, in the composed
/// buffer.
#[test]
fn a_themed_tab_follows_set_theme() {
    let active = toml::Value::Table(toml::value::Table::from_iter([(
        "fg".to_owned(),
        toml::Value::String("theme.accent".to_owned()),
    )]));
    let cfg = StatusCfg {
        left: vec![spec("windows", &[("active", active)])],
        ..StatusCfg::default()
    };
    let mut p = StatusBarPainter::new(build_bar(&cfg), Position::Top);
    p.set_windows(wins(&[("zsh", true)]));
    p.set_theme(&Theme {
        accent: Color::Rgb(9, 8, 7),
        ..Theme::default()
    });
    let (buf, _, _) = p
        .compose_buffer(BarInset::NONE, 40, 10, &ctx_default("main"))
        .expect("composes");
    assert_eq!(buf[(1, 0)].fg, Color::Rgb(9, 8, 7));
}

/// An Info notice composes in the selection pair, with no reverse video;
/// a Warn notice rides the error slot; the live paint agrees.
#[test]
fn notices_compose_in_theme_colors() {
    let theme = Theme::default();
    let mut p = windows_bar();
    p.set_windows(wins(&[("a", true)]));
    p.set_theme(&theme);
    let now = std::time::Instant::now();
    assert!(p.set_notice(Notice::info("hi"), now));
    let (buf, _, _) = p
        .compose_buffer(BarInset::NONE, 40, 10, &ctx_default("main"))
        .expect("composes");
    let cell = &buf[(39, 0)];
    assert_eq!(cell.fg, theme.selection_fg);
    assert_eq!(cell.bg, theme.selection_bg);
    assert!(!cell.modifier.contains(Modifier::REVERSED));
    let live = paint(&mut p, BarInset::NONE, 40, 10, "main");
    assert!(
        live.contains("\x1b[38;2;244;247;251;48;2;41;54;40m hi "),
        "{live:?}"
    );

    assert!(p.set_notice(Notice::warn("hi"), now));
    let (buf, _, _) = p
        .compose_buffer(BarInset::NONE, 40, 10, &ctx_default("main"))
        .expect("composes");
    assert_eq!(buf[(39, 0)].bg, theme.error);
    assert!(buf[(39, 0)].modifier.contains(Modifier::BOLD));
}

/// The config-error line is selection ink on the error slot.
#[test]
fn the_error_line_rides_the_error_slot() {
    let theme = Theme::default();
    let mut p = StatusBarPainter::error_line("bad config");
    p.set_theme(&theme);
    let (buf, _, _) = p
        .compose_buffer(BarInset::NONE, 40, 10, &ctx_default("main"))
        .expect("composes");
    assert_eq!(buf[(0, 0)].bg, theme.error);
    assert_eq!(buf[(0, 0)].fg, theme.selection_fg);
}

mod chips;
