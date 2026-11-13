//! Built-in themes: a terminal palette for the server-owned grids and the
//! window's chrome drawn from the same colours.
//!
//! "System" follows the OS: the chrome, and the terminal too, which keeps the
//! engine's defaults when dark and takes Catppuccin Latte's palette when
//! light. A program's own OSC colours beat any theme. A
//! Ghostty config on this machine adds one more theme, read at launch, and
//! Omarchy another, which follows Omarchy's theme changes and is the
//! default until the person picks one.

use phux_client_runtime::engine::TerminalTheme;
use phux_client_runtime::publication::Rgb;
use rune::Color;

mod builtin;
mod ghostty;
mod omarchy;
pub use builtin::THEMES;
use builtin::{LATTE, MOCHA};
pub use ghostty::ghostty;
pub use omarchy::{omarchy, watch as watch_omarchy};

/// What a readable role must clear against the surfaces it sits on: WCAG AA
/// for body text.
pub const TEXT_BAR: f32 = 4.5;
/// The lower bar for `dim`: WCAG's 3:1 for marks and large text. Dim carries
/// hints and captions, never the only word on something that must be read.
pub const DIM_BAR: f32 = 3.0;
/// How far a raised surface steps toward the text colour.
const RAISED: f32 = 0.09;
/// How much accent the selected wash lays over a surface.
pub const SELECTED_WASH: f32 = 0.22;
/// How much accent a theme's terminal selection holds.
const SELECTION_WASH: f32 = 0.4;
/// The terminal selection (text, fill) with no theme installed.
const SYSTEM_SELECTION: (u32, u32) = (0xffffff, 0x3a5f9e);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub dark: bool,
    pub ansi: [u32; 16],
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
    pub accent: u32,
}

const fn bytes(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

/// `hex` as a Rune colour, decoded the way grid cells are.
pub fn color(hex: u32) -> Color {
    let [r, g, b] = bytes(hex);
    crate::grid::color(Rgb { r, g, b })
}

/// `from` moved `amount` of the way to `to`, per sRGB channel.
pub fn mix(from: u32, to: u32, amount: f32) -> u32 {
    let [fr, fg, fb] = bytes(from);
    let [tr, tg, tb] = bytes(to);
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount) as u32;
    (channel(fr, tr) << 16) | (channel(fg, tg) << 8) | channel(fb, tb)
}

/// How far apart two colours read, 1 to 21 (WCAG's contrast ratio).
pub fn contrast(a: Color, b: Color) -> f32 {
    let (a, b) = (a.luminance(), b.luminance());
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Black or white, whichever reads on `fill`. One always clears `TEXT_BAR`.
pub fn on_fill(fill: Color) -> Color {
    if contrast(Color::WHITE, fill) >= contrast(Color::BLACK, fill) {
        Color::WHITE
    } else {
        Color::BLACK
    }
}

/// `hex` moved just far enough away from `over` to clear `bar`.
fn legible(hex: u32, over: u32, bar: f32) -> u32 {
    const STEPS: u16 = 50;
    let end = if on_fill(color(over)) == Color::WHITE {
        0xffffff
    } else {
        0x000000
    };
    (0..=STEPS)
        .map(|step| mix(hex, end, f32::from(step) / f32::from(STEPS)))
        .find(|candidate| contrast(color(*candidate), color(over)) >= bar)
        .unwrap_or(end)
}

/// The agent marks, which double as words in the inbox and sidebar.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Marks {
    pub alert: Color,
    pub done: Color,
    pub bell: Color,
    pub working: Color,
    pub good: Color,
}

/// The terminal's selection colours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    pub foreground: Color,
    pub background: Color,
}

impl Selection {
    /// These colours as a memo key: a new theme repaints the grid.
    pub const fn key(self) -> [u32; 8] {
        let (f, b) = (self.foreground, self.background);
        [
            f.r.to_bits(),
            f.g.to_bits(),
            f.b.to_bits(),
            f.a.to_bits(),
            b.r.to_bits(),
            b.g.to_bits(),
            b.b.to_bits(),
            b.a.to_bits(),
        ]
    }

    fn of((foreground, background): (u32, u32)) -> Self {
        Self {
            foreground: color(foreground),
            background: color(background),
        }
    }

    pub fn system() -> Self {
        Self::of(SYSTEM_SELECTION)
    }
}

/// What the chrome draws with, moved off a theme's raw colours only as far as
/// its own surfaces need. A palette written for terminals isn't always
/// readable on a raised sheet (Solarized Light's foreground isn't).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Roles {
    pub text: Color,
    pub dim: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub danger: Color,
    /// The rule between the window's regions, opaque so it reads the same
    /// over every surface.
    pub edge: Color,
    pub marks: Marks,
    pub selection: Selection,
}

impl Theme {
    /// Marks with no theme installed: Catppuccin's.
    pub fn system_marks(dark: bool) -> Marks {
        THEMES[if dark { MOCHA } else { LATTE }].roles().marks
    }

    /// The terminal palette with no theme picked: the engine's own (dark)
    /// defaults, or Latte's in a light appearance so the grid matches the
    /// chrome.
    pub fn system_terminal(dark: bool) -> Option<TerminalTheme> {
        (!dark).then(|| THEMES[LATTE].terminal())
    }

    /// The system appearance's failure colour, from the same palettes.
    pub fn system_danger(dark: bool) -> Color {
        THEMES[if dark { MOCHA } else { LATTE }].roles().danger
    }

    fn raised_hex(&self) -> u32 {
        mix(self.background, self.foreground, RAISED)
    }

    /// A raised surface: sheets, cards, and the hardest ground for text.
    pub fn raised(&self) -> Color {
        color(self.raised_hex())
    }

    pub fn roles(&self) -> Roles {
        let raised = self.raised_hex();
        // Text also sits on the selected wash, the closest ground to it.
        let washed = mix(raised, self.accent, SELECTED_WASH);
        let text = legible(self.foreground, washed, TEXT_BAR);
        let readable = |hex| color(legible(hex, raised, TEXT_BAR));
        let accent = readable(self.accent);
        let selection = mix(self.background, self.accent, SELECTION_WASH);
        Roles {
            text: color(text),
            dim: color(legible(mix(text, raised, 0.28), raised, DIM_BAR)),
            accent,
            on_accent: on_fill(accent),
            danger: readable(self.ansi[1]),
            edge: self.surface(0.16),
            marks: Marks {
                alert: readable(self.ansi[1]),
                done: readable(self.ansi[6]),
                bell: readable(mix(self.ansi[1], self.ansi[3], 0.5)),
                working: readable(self.ansi[3]),
                good: readable(self.ansi[2]),
            },
            selection: Selection::of((legible(self.foreground, selection, TEXT_BAR), selection)),
        }
    }

    /// The engine's defaults for every replica.
    pub fn terminal(&self) -> TerminalTheme {
        TerminalTheme {
            ansi16: self.ansi.map(bytes),
            foreground: bytes(self.foreground),
            background: bytes(self.background),
            cursor: bytes(self.cursor),
        }
    }

    /// The chrome: raised surfaces step toward the text colour.
    pub fn surface(&self, step: f32) -> Color {
        color(mix(self.background, self.foreground, step))
    }
}

/// Every theme on offer, in picker order: Omarchy's, the Ghostty config's,
/// then the built-ins.
pub fn all() -> impl Iterator<Item = &'static Theme> {
    omarchy().into_iter().chain(ghostty()).chain(THEMES)
}

/// A remembered id; `None` is System, and so is one no longer on offer.
pub fn find(id: &str) -> Option<&'static Theme> {
    all().find(|t| t.id == id)
}

/// The most a theme file may hold.
const MAX_CONFIG: u64 = 1 << 20;

/// A theme file's text. `None` unless it's a regular file of a sane size: a
/// FIFO would block its reader forever, and a device might never end.
fn read_config(path: &std::path::Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    (meta.is_file() && meta.len() <= MAX_CONFIG)
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_terminal_matches_the_appearance() {
        assert_eq!(
            Theme::system_terminal(true),
            None,
            "dark keeps the engine's defaults"
        );
        let light = Theme::system_terminal(false).expect("light installs a palette");
        assert!(light.background.iter().all(|&c| c > 0xc0), "a light ground");
    }

    #[test]
    fn ids_are_distinct_and_found() {
        for (at, theme) in THEMES.iter().enumerate() {
            assert_eq!(find(theme.id), Some(theme));
            assert!(THEMES[at + 1..].iter().all(|t| t.id != theme.id));
        }
        assert_eq!(find("gone"), None);
    }

    #[test]
    fn text_contrasts_with_its_background() {
        let luma = |hex: u32| {
            let [r, g, b] = bytes(hex);
            0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)
        };
        for theme in THEMES {
            let gap = luma(theme.foreground) - luma(theme.background);
            assert_eq!(gap > 0.0, theme.dark, "{}", theme.name);
            assert!(gap.abs() > 80.0, "{}", theme.name);
        }
    }

    #[test]
    fn every_readable_role_clears_its_bar_on_every_surface() {
        for theme in THEMES {
            let roles = theme.roles();
            let marks = roles.marks;
            let readable = [
                ("accent", roles.accent),
                ("danger", roles.danger),
                ("alert", marks.alert),
                ("done", marks.done),
                ("bell", marks.bell),
                ("working", marks.working),
                ("good", marks.good),
            ];
            let grounds = [theme.surface(0.0), theme.surface(0.045), theme.raised()];
            for ground in grounds {
                let name = theme.name;
                let text = contrast(roles.text, ground);
                assert!(text >= TEXT_BAR, "{name} text: {text}");
                let dim = contrast(roles.dim, ground);
                assert!(dim >= DIM_BAR, "{name} dim: {dim}");
                for (role, ink) in readable {
                    let got = contrast(ink, ground);
                    assert!(got >= TEXT_BAR, "{name} {role}: {got}");
                }
            }
        }
    }

    #[test]
    fn text_reads_on_the_selected_wash() {
        for theme in THEMES {
            let wash = color(mix(theme.raised_hex(), theme.accent, SELECTED_WASH));
            let got = contrast(theme.roles().text, wash);
            assert!(got >= TEXT_BAR, "{}: {got}", theme.name);
        }
    }

    #[test]
    fn on_accent_and_selection_read_on_their_fills() {
        for theme in THEMES {
            let roles = theme.roles();
            let selection = roles.selection;
            for (role, got) in [
                ("on accent", contrast(roles.on_accent, roles.accent)),
                (
                    "selection",
                    contrast(selection.foreground, selection.background),
                ),
            ] {
                assert!(got >= TEXT_BAR, "{} {role}: {got}", theme.name);
            }
        }
    }

    #[test]
    fn any_accent_gets_a_readable_ink() {
        for hex in [0xffffff, 0x000000, 0x808080, 0xffcc00, 0x0a84ff] {
            let fill = color(hex);
            assert!(contrast(on_fill(fill), fill) >= TEXT_BAR, "{hex:06x}");
        }
    }

    #[test]
    fn system_marks_come_from_catppuccin() {
        assert_eq!(THEMES[MOCHA].id, "catppuccin-mocha");
        assert_eq!(THEMES[LATTE].id, "catppuccin-latte");
        assert_ne!(Theme::system_marks(true), Theme::system_marks(false));
    }

    #[test]
    fn mixing_stays_between_its_ends() {
        assert_eq!(mix(0x000000, 0xffffff, 0.0), 0x000000);
        assert_eq!(mix(0x000000, 0xffffff, 1.0), 0xffffff);
        assert_eq!(mix(0x102030, 0x102030, 0.5), 0x102030);
    }

    #[test]
    fn a_theme_reaches_the_engine_as_bytes() {
        let t = find("nord").unwrap().terminal();
        assert_eq!(t.background, [0x2e, 0x34, 0x40]);
        assert_eq!(t.ansi16[1], [0xbf, 0x61, 0x6a]);
    }
}
