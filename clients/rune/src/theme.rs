//! Built-in themes: a terminal palette for the server-owned grids and the
//! window's chrome drawn from the same colours.
//!
//! "System" installs nothing: programs keep the engine's defaults and the
//! chrome follows the OS. A program's own OSC colours beat any theme.

use phux_client_runtime::engine::TerminalTheme;
use phux_client_runtime::publication::Rgb;
use rune::Color;

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
    /// Marks with no theme installed: Catppuccin's, as the GPUI desktop uses.
    pub fn system_marks(dark: bool) -> Marks {
        THEMES[if dark { MOCHA } else { LATTE }].roles().marks
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

/// Where the Catppuccin pair sits in `THEMES`.
const MOCHA: usize = 0;
const LATTE: usize = 6;

pub const THEMES: &[Theme] = &[
    Theme {
        id: "catppuccin-mocha",
        name: "Catppuccin Mocha",
        dark: true,
        ansi: [
            0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de,
            0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
        ],
        foreground: 0xcdd6f4,
        background: 0x1e1e2e,
        cursor: 0xf5e0dc,
        accent: 0xcba6f7,
    },
    Theme {
        id: "tokyo-night",
        name: "Tokyo Night",
        dark: true,
        ansi: [
            0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6,
            0x414868, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
        ],
        foreground: 0xc0caf5,
        background: 0x1a1b26,
        cursor: 0xc0caf5,
        accent: 0x7aa2f7,
    },
    Theme {
        id: "rose-pine",
        name: "Rosé Pine",
        dark: true,
        ansi: [
            0x26233a, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
            0x6e6a86, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
        ],
        foreground: 0xe0def4,
        background: 0x191724,
        cursor: 0xe0def4,
        accent: 0xc4a7e7,
    },
    Theme {
        id: "dracula",
        name: "Dracula",
        dark: true,
        ansi: [
            0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2,
            0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
        foreground: 0xf8f8f2,
        background: 0x282a36,
        cursor: 0xf8f8f2,
        accent: 0xbd93f9,
    },
    Theme {
        id: "gruvbox-dark",
        name: "Gruvbox Dark",
        dark: true,
        ansi: [
            0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984,
            0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ],
        foreground: 0xebdbb2,
        background: 0x282828,
        cursor: 0xebdbb2,
        accent: 0xfe8019,
    },
    Theme {
        id: "nord",
        name: "Nord",
        dark: true,
        ansi: [
            0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0,
            0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ],
        foreground: 0xd8dee9,
        background: 0x2e3440,
        cursor: 0xd8dee9,
        accent: 0x88c0d0,
    },
    Theme {
        id: "catppuccin-latte",
        name: "Catppuccin Latte",
        dark: false,
        ansi: [
            0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be,
            0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
        ],
        foreground: 0x4c4f69,
        background: 0xeff1f5,
        cursor: 0xdc8a78,
        accent: 0x8839ef,
    },
    Theme {
        id: "solarized-light",
        name: "Solarized Light",
        dark: false,
        ansi: [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5,
            0x002b36, 0xcb4b16, 0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
        ],
        foreground: 0x657b83,
        background: 0xfdf6e3,
        cursor: 0x586e75,
        accent: 0x268bd2,
    },
];

/// A remembered id; `None` is System, and so is one no longer built in.
pub fn find(id: &str) -> Option<&'static Theme> {
    THEMES.iter().find(|t| t.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

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
