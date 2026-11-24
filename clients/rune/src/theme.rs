//! Themes: terminal palettes the person picks, and the adapters that read
//! them (the shared catalog, a Ghostty config, Omarchy, the desktop's
//! light or dark). A theme is only the solver's input; the chrome's roles
//! come from `phux_design` through `ui::tokens`.
//!
//! "System" follows the OS: the house look in its appearance, and the
//! house terminal palette (`phux`, `phux-light`) on the grid. A program's
//! own OSC colours beat any theme. A Ghostty config on this machine adds
//! one more theme, read at launch, and Omarchy another, which follows
//! Omarchy's theme changes and is the default until the person picks one.

use phux_client_runtime::engine::TerminalTheme;
use phux_client_runtime::publication::Rgb;
use rune::Color;

mod builtin;
mod ghostty;
mod omarchy;
mod portal;
pub use builtin::themes;
pub use ghostty::ghostty;
pub use omarchy::omarchy;
pub use portal::system_dark;

/// Wake Rune when Omarchy's theme or the desktop's light or dark changes.
pub fn watch(waker: rune::Waker) {
    omarchy::watch(waker.clone());
    portal::watch(waker);
}

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
    /// The theme's own terminal selection fill, when it names one.
    pub selection: Option<u32>,
    /// Bold text in the default foreground, when the theme names a colour.
    pub bold: Option<u32>,
}

const fn bytes(hex: u32) -> [u8; 3] {
    [(hex >> 16) as u8, (hex >> 8) as u8, hex as u8]
}

/// `hex` as a Rune colour, decoded the way grid cells are.
pub fn color(hex: u32) -> Color {
    let [r, g, b] = bytes(hex);
    crate::grid::color(Rgb { r, g, b })
}

/// How far apart two colours read, 1 to 21 (WCAG's contrast ratio).
pub fn contrast(a: Color, b: Color) -> f32 {
    let (a, b) = (a.luminance(), b.luminance());
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// Black or white, whichever reads on `fill`. One always clears WCAG AA.
pub fn on_fill(fill: Color) -> Color {
    if contrast(Color::WHITE, fill) >= contrast(Color::BLACK, fill) {
        Color::WHITE
    } else {
        Color::BLACK
    }
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

    /// The house look's selection, before a window knows its appearance.
    pub fn system() -> Self {
        use crate::ui::tokens::Paint;
        let s = phux_design::Roles::house(phux_design::Mode::Dark).selection;
        Self {
            foreground: s.fg.paint(),
            background: s.bg.paint(),
        }
    }
}

impl Theme {
    /// The house terminal palette for the system appearance: the grid
    /// matches the chrome with no theme picked.
    pub fn system_terminal(dark: bool) -> Option<TerminalTheme> {
        Some(builtin::required(if dark { "phux" } else { "phux-light" }).terminal())
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
}

/// Every theme on offer, in picker order: Omarchy's, the Ghostty config's,
/// then the built-ins.
pub fn all() -> impl Iterator<Item = &'static Theme> {
    omarchy().into_iter().chain(ghostty()).chain(themes())
}

/// A remembered id; `None` is System, and so is one no longer on offer.
/// Old ids (`midnight`, `catppuccin`) still find their theme.
pub fn find(id: &str) -> Option<&'static Theme> {
    all().find(|t| t.id == id).or_else(|| builtin::named(id))
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
mod tests;
