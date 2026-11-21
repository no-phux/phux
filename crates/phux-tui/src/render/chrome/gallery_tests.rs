//! The TUI design gallery: the sidebar, the status bar, a toast
//! overlay and the session picker, painted under each terminal the chrome
//! must survive and serialised cell by cell (symbol, fg, bg, attributes)
//! into `tests/fixtures/gallery/<fixture>.txt`, compared exactly.
//!
//! `TUI_GALLERY_BLESS=1` rewrites the fixtures after a deliberate
//! change; `TUI_GALLERY_SNAP=<dir>` also writes each fixture as an SVG
//! for a visual review.

use std::fmt::Write as _;
use std::path::PathBuf;

use phux_client::agent_meta::AgentMetaState;
use phux_config::{StatusCfg, Widget, WidgetSpec};
use phux_design::{Mode, Rgb8, Roles};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect as RataRect;
use ratatui::style::{Color, Modifier};

use super::sidebar::{AgentEntry, SessionRosterEntry, SidebarPainter, SidebarRule};
use super::status_bar::widgets::{StatusBar, WidgetRegistry, WindowInfo};
use super::status_bar::{BarInset, Position, StatusBarPainter, make_context};
use crate::layout::Rect;
use crate::render::overlay::RenderOverlay;
use crate::render::overlay::select_list::{SelectItem, SelectList};
use crate::render::overlay::toast::ToastOverlay;
use crate::render::theme::{Depth, Glyphs, Outer, Theme, xterm_rgb};

/// The terminals, plus the house look in both modes and 256.
fn fixtures() -> Vec<(&'static str, Theme)> {
    let solarized = phux_design::catalog()
        .find("solarized-dark")
        .expect("solarized-dark is in the catalog");
    let light_ground = Roles::house(Mode::Light).layer.ground.rgb();
    vec![
        ("house-dark", Theme::house(&Outer::default())),
        (
            "house-light",
            Theme::house(&Outer {
                background: Some(light_ground),
                ..Outer::default()
            }),
        ),
        (
            "xterm-256",
            Theme::house(&Outer {
                depth: Depth::Ansi256,
                ..Outer::default()
            }),
        ),
        (
            "solarized-dark-16",
            Theme::from_input(
                &solarized.input(),
                &Outer {
                    depth: Depth::Ansi16,
                    background: Some(solarized.bg),
                    ..Outer::default()
                },
            ),
        ),
        (
            "xterm-16-unknown",
            Theme::house(&Outer {
                depth: Depth::Ansi16,
                ..Outer::default()
            }),
        ),
        (
            "no-color",
            Theme::house(&Outer {
                depth: Depth::Mono,
                ..Outer::default()
            }),
        ),
        (
            "cjk-ascii",
            Theme::house(&Outer {
                glyphs: Glyphs::Ascii,
                ..Outer::default()
            }),
        ),
    ]
}

fn window(name: &str, active: bool) -> WindowInfo {
    WindowInfo {
        name: name.to_owned(),
        active,
        ..WindowInfo::default()
    }
}

fn agent(window: usize, name: &str, state: AgentMetaState, seen: bool) -> AgentEntry {
    AgentEntry {
        session: None,
        session_id: None,
        resource: None,
        window,
        window_name: format!("w{window}"),
        pane: None,
        name: name.to_owned(),
        state,
        attention: false,
        host: None,
        seen,
    }
}

fn roster(name: &str, active: bool, blocked: usize, done: usize) -> SessionRosterEntry {
    SessionRosterEntry {
        name: name.to_owned(),
        host: "mini".to_owned(),
        active,
        selectable: true,
        blocked,
        done_unvisited: done,
        settled: 1,
        ..SessionRosterEntry::default()
    }
}

/// Every rung of the ladder on one strip: agents in each state, an asking
/// window, a failed and an ended retained pane, and a roster histogram.
fn sidebar(theme: &Theme) -> Buffer {
    let mut p = SidebarPainter::new(*theme);
    p.set_windows(vec![
        window("editor", true),
        WindowInfo {
            attention: true,
            ..window("review", false)
        },
        WindowInfo {
            exited: Some("3".to_owned()),
            ..window("build", false)
        },
        WindowInfo {
            exited: Some("0".to_owned()),
            ..window("lint", false)
        },
    ]);
    p.set_agents(vec![
        agent(1, "claude", AgentMetaState::Blocked, false),
        agent(0, "codex", AgentMetaState::Working, false),
        agent(2, "gemini", AgentMetaState::Done, false),
        agent(3, "merge", AgentMetaState::Done, true),
        agent(0, "watch", AgentMetaState::Idle, false),
        agent(0, "probe", AgentMetaState::Unknown, false),
    ]);
    p.set_roster(vec![
        roster("development", true, 1, 1),
        roster("ops", false, 2, 0),
        roster("notes", false, 0, 3),
    ]);
    p.compose_buffer(
        Rect {
            x: 0,
            y: 0,
            w: 30,
            h: 26,
        },
        SidebarRule::Trailing,
        &[],
    )
}

/// The window tabs and the needs-you chip.
fn status_bar(theme: &Theme) -> Buffer {
    let cfg = StatusCfg {
        left: vec![Widget::Spec(WidgetSpec {
            kind: "windows".to_owned(),
            opts: std::collections::BTreeMap::new(),
        })],
        right: vec![Widget::Bare("session-name".into())],
        ..StatusCfg::default()
    };
    let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar builds");
    let mut p = StatusBarPainter::new(bar, Position::Bottom);
    p.set_theme(theme);
    p.set_windows(vec![window("editor", true), window("review", false)]);
    p.set_needs_you(2);
    p.compose_buffer(
        BarInset::NONE,
        60,
        1,
        &make_context("main", std::time::UNIX_EPOCH),
    )
    .map(|(buf, _, _)| buf)
    .expect("composes")
}

fn toast(theme: &Theme) -> Buffer {
    let area = RataRect::new(0, 0, 44, 7);
    let mut buf = Buffer::empty(area);
    ToastOverlay::new(
        "Config reloaded",
        vec!["theme: phux".to_owned(), "2 panes need you".to_owned()],
        theme,
    )
    .render(area, &mut buf);
    buf
}

fn picker(theme: &Theme) -> Buffer {
    let action = |name: &str| crate::attach::resolved(name, std::collections::BTreeMap::new());
    let one = phux_design::plural("count.needs_you", 1).expect("plural");
    let many = phux_design::plural("count.needs_you", 3).expect("plural");
    let list = SelectList::new(
        "Sessions",
        vec![
            SelectItem::header("Local"),
            SelectItem::new("development", action("switch-session"))
                .indented()
                .secondary(one)
                .attention(),
            SelectItem::new("ops", action("switch-session"))
                .indented()
                .secondary(many)
                .attention(),
            SelectItem::new("notes", action("switch-session")).indented(),
        ],
        theme,
    );
    let area = RataRect::new(0, 0, 48, 10);
    let mut buf = Buffer::empty(area);
    list.render(area, &mut buf);
    buf
}

fn color(c: Color) -> String {
    match c {
        Color::Reset => "-".to_owned(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => format!("i{i}"),
        named => format!("{named:?}").to_lowercase(),
    }
}

fn attrs(m: Modifier) -> String {
    [
        (Modifier::BOLD, "b"),
        (Modifier::DIM, "d"),
        (Modifier::ITALIC, "i"),
        (Modifier::UNDERLINED, "u"),
        (Modifier::REVERSED, "r"),
    ]
    .iter()
    .filter(|(bit, _)| m.contains(*bit))
    .map(|(_, code)| *code)
    .collect()
}

/// One buffer as text rows, each followed by its style runs.
fn serialise(name: &str, buf: &Buffer, out: &mut String) {
    let area = buf.area;
    let _ = writeln!(out, "== {name} {}x{}", area.width, area.height);
    for y in 0..area.height {
        let text: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
        let _ = writeln!(out, "{y:02}|{text}|");
        let mut runs = Vec::new();
        let mut x = 0;
        while x < area.width {
            let cell = &buf[(x, y)];
            let key = (cell.fg, cell.bg, cell.modifier);
            let start = x;
            while x < area.width && {
                let c = &buf[(x, y)];
                (c.fg, c.bg, c.modifier) == key
            } {
                x += 1;
            }
            if key != (Color::Reset, Color::Reset, Modifier::empty()) {
                let a = attrs(key.2);
                let a = if a.is_empty() {
                    String::new()
                } else {
                    format!(" +{a}")
                };
                runs.push(format!(
                    "{start}-{} {}/{}{a}",
                    x - 1,
                    color(key.0),
                    color(key.1)
                ));
            }
        }
        if !runs.is_empty() {
            let _ = writeln!(out, "  {}", runs.join(", "));
        }
    }
}

fn render_fixture(theme: &Theme) -> (String, Vec<(&'static str, Buffer)>) {
    let parts = vec![
        ("sidebar", sidebar(theme)),
        ("status-bar", status_bar(theme)),
        ("toast", toast(theme)),
        ("picker", picker(theme)),
    ];
    let mut out = String::new();
    for (name, buf) in &parts {
        serialise(name, buf, &mut out);
    }
    (out, parts)
}

fn fixture_dir() -> PathBuf {
    // Read at run time: a compiled-in path ties the build cache to one
    // checkout (scripts/check-cache-portable.sh).
    PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo test sets CARGO_MANIFEST_DIR"),
    )
    .join("tests/fixtures/gallery")
}

/// The 16-colour xterm defaults, to preview what a named slot looks like.
fn preview_rgb(c: Color, fallback: Rgb8) -> Rgb8 {
    let named = [
        (Color::Black, 0x00_0000),
        (Color::Red, 0xcd_0000),
        (Color::Green, 0x00_cd00),
        (Color::Yellow, 0xcd_cd00),
        (Color::Blue, 0x00_00ee),
        (Color::Magenta, 0xcd_00cd),
        (Color::Cyan, 0x00_cdcd),
        (Color::Gray, 0xe5_e5e5),
        (Color::DarkGray, 0x7f_7f7f),
        (Color::LightRed, 0xff_0000),
        (Color::LightGreen, 0x00_ff00),
        (Color::LightYellow, 0xff_ff00),
        (Color::LightBlue, 0x5c_5cff),
        (Color::LightMagenta, 0xff_00ff),
        (Color::LightCyan, 0x00_ffff),
        (Color::White, 0xff_ffff),
    ];
    match c {
        Color::Rgb(r, g, b) => Rgb8 { r, g, b },
        Color::Indexed(i) if i >= 16 => xterm_rgb(i),
        Color::Reset => fallback,
        other => named
            .iter()
            .find(|(n, _)| *n == other)
            .map_or(fallback, |(_, hex)| Rgb8::hex(*hex)),
    }
}

/// An SVG of every part, on a ground that stands in for the terminal's.
fn svg(name: &str, ground: Rgb8, ink: Rgb8, parts: &[(&'static str, Buffer)]) -> String {
    const CW: u32 = 9;
    const CH: u32 = 18;
    let hex = |c: Rgb8| format!("#{:06x}", c.to_u32());
    let width = parts
        .iter()
        .map(|(_, b)| u32::from(b.area.width))
        .max()
        .unwrap_or(0)
        * CW
        + 32;
    let height: u32 = parts
        .iter()
        .map(|(_, b)| u32::from(b.area.height) * CH + 28)
        .sum::<u32>()
        + 40;
    let mut out = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='{width}' height='{height}' \
         font-family='Menlo, monospace' font-size='14'>\
         <rect width='100%' height='100%' fill='{}'/>\
         <text x='16' y='24' fill='{}' font-size='12'>{name}</text>",
        hex(ground),
        hex(ink)
    );
    let mut top = 40;
    for (part, buf) in parts {
        let _ = write!(
            out,
            "<text x='16' y='{}' fill='{}' font-size='11' opacity='0.6'>{part}</text>",
            top + 10,
            hex(ink)
        );
        top += 18;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                let (mut fg, mut bg) = (preview_rgb(cell.fg, ink), preview_rgb(cell.bg, ground));
                if cell.modifier.contains(Modifier::REVERSED) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let px = 16 + u32::from(x) * CW;
                let py = top + u32::from(y) * CH;
                if bg != ground {
                    let _ = write!(
                        out,
                        "<rect x='{px}' y='{py}' width='{CW}' height='{CH}' fill='{}'/>",
                        hex(bg)
                    );
                }
                let sym = cell.symbol();
                if sym.trim().is_empty() {
                    continue;
                }
                let sym = sym
                    .replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                let weight = if cell.modifier.contains(Modifier::BOLD) {
                    " font-weight='bold'"
                } else {
                    ""
                };
                let _ = write!(
                    out,
                    "<text x='{px}' y='{}' fill='{}'{weight}>{sym}</text>",
                    py + 14,
                    hex(fg)
                );
            }
        }
        top += u32::from(buf.area.height) * CH + 10;
    }
    out.push_str("</svg>");
    out
}

/// The terminal ground a fixture stands in for, for the SVG preview.
fn preview_ground(name: &str) -> (Rgb8, Rgb8) {
    match name {
        "house-light" => {
            let r = Roles::house(Mode::Light);
            (r.layer.ground.rgb(), r.text.text.rgb())
        }
        "solarized-dark-16" => (Rgb8::hex(0x00_2b36), Rgb8::hex(0x83_9496)),
        "xterm-16-unknown" | "no-color" => (Rgb8::hex(0x00_0000), Rgb8::hex(0xe5_e5e5)),
        _ => {
            let r = Roles::house(Mode::Dark);
            (r.layer.ground.rgb(), r.text.text.rgb())
        }
    }
}

#[test]
fn gallery_fixtures_match() {
    let bless = std::env::var_os("TUI_GALLERY_BLESS").is_some();
    let svg_dir = std::env::var_os("TUI_GALLERY_SNAP").map(PathBuf::from);
    let dir = fixture_dir();
    let mut stale = Vec::new();
    for (name, theme) in fixtures() {
        let (text, parts) = render_fixture(&theme);
        let path = dir.join(format!("{name}.txt"));
        if bless {
            std::fs::create_dir_all(&dir).expect("fixture dir");
            std::fs::write(&path, &text).expect("write fixture");
        } else if std::fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
            stale.push(name);
        }
        if let Some(svg_dir) = &svg_dir {
            std::fs::create_dir_all(svg_dir).expect("svg dir");
            let (ground, ink) = preview_ground(name);
            std::fs::write(
                svg_dir.join(format!("{name}.svg")),
                svg(name, ground, ink, &parts),
            )
            .expect("write svg");
        }
    }
    assert!(
        stale.is_empty(),
        "gallery fixtures differ: {stale:?}; review, then rerun with TUI_GALLERY_BLESS=1"
    );
}

/// What each fixture exists to prove, asserted on the buffers themselves
/// so a bless cannot quietly drop it.
#[test]
fn every_fixture_carries_its_contract() {
    for (name, theme) in fixtures() {
        let (text, parts) = render_fixture(&theme);
        let ascii = theme.term.glyphs == Glyphs::Ascii;
        let (needs, failed, ended, done, working, unknown) = if ascii {
            ("*", "x3", "-", "+", "~", ".")
        } else {
            ("●", "✕3", "–", "◆", "◐", "·")
        };
        for mark in [needs, failed, ended, done, working, unknown] {
            assert!(text.contains(mark), "{name}: the ladder shows {mark}");
        }
        if ascii {
            // `·` stays as a chip separator; only the ladder switches.
            for glyph in ["●", "✕", "◆", "◐", "○", "–"] {
                assert!(!text.contains(glyph), "{name}: no mixed ladder ({glyph})");
            }
        }
        assert!(
            text.contains(&format!(" {needs}2 ")),
            "{name}: the bar chip"
        );
        assert!(text.contains("1 needs you") && text.contains("3 need you"));
        assert!(
            !text.contains("blocked") && !text.contains(" ask "),
            "{name}"
        );
        check_cells(name, &theme, &parts);
    }
}

/// The per-cell half of the contract: depth, `NO_COLOR` and no SGR dim.
fn check_cells(name: &str, theme: &Theme, parts: &[(&'static str, Buffer)]) {
    {
        for (part, buf) in parts {
            for cell in buf.content() {
                if theme.term.depth == Depth::Mono {
                    assert_eq!(
                        (cell.fg, cell.bg),
                        (Color::Reset, Color::Reset),
                        "{name}/{part}: NO_COLOR paints no colour"
                    );
                }
                assert!(
                    !cell.modifier.contains(Modifier::DIM),
                    "{name}/{part}: faint is a colour, never SGR dim"
                );
                if matches!(theme.term.depth, Depth::Ansi16 | Depth::Ansi256) {
                    assert!(
                        !matches!(cell.fg, Color::Rgb(..)) && !matches!(cell.bg, Color::Rgb(..)),
                        "{name}/{part}: 24-bit colour on a {:?} terminal",
                        theme.term.depth
                    );
                }
                if theme.term.depth == Depth::Ansi256 {
                    for c in [cell.fg, cell.bg] {
                        if let Color::Indexed(i) = c {
                            assert!(i >= 16, "{name}/{part}: user-remapped index {i}");
                        }
                    }
                }
            }
        }
    }
}
