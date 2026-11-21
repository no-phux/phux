//! What the outer terminal can show: its colour depth, the
//! background chrome text lands on, and whether the status ladder must fall
//! back to ASCII.
//!
//! The attach handshake probes OSC 10/11 once on the cooked terminal
//! (`attach::terminal_probe`, never when stdout is not a TTY) and records the
//! reply here with [`record_reported`]; [`Outer::detect`] reads it with the
//! environment when the theme is built.

use std::sync::Mutex;

use phux_design::{Mode, Rgb8};

/// How many colours the outer terminal renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Depth {
    /// 24-bit (`COLORTERM=truecolor|24bit`): roles paint as `Color::Rgb`.
    TrueColor,
    /// The xterm 256 palette: roles snap to entries 16–255, never 0–15,
    /// which the user remaps.
    Ansi256,
    /// The 16 ANSI colours: a semantic map chosen per background.
    Ansi16,
    /// `NO_COLOR` (or `TERM=dumb`): glyph plus word, bold only.
    Mono,
}

/// Which column of the status ladder the chrome draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Glyphs {
    /// `● ✕ ◆ ◐ ○ · – •`.
    Unicode,
    /// `* x + ~ o . - !`: a CJK or non-UTF-8 locale, where the ladder's
    /// East-Asian-Ambiguous marks may draw two cells wide. The whole ladder
    /// switches; marks are never mixed.
    Ascii,
}

/// What a built [`super::Theme`] knows about the terminal it paints on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Term {
    /// The colour depth the slots were fitted to.
    pub depth: Depth,
    /// The status ladder column.
    pub glyphs: Glyphs,
    /// The roles were solved against the terminal's real background, so
    /// chrome text may sit on `Reset`. When `false`, chrome drawn on the
    /// terminal's own ground (rules, pane titles) fills with `surface`.
    pub ground_known: bool,
}

impl Default for Term {
    fn default() -> Self {
        Self {
            depth: Depth::TrueColor,
            glyphs: Glyphs::Unicode,
            ground_known: false,
        }
    }
}

impl Term {
    /// Whether colour is off entirely.
    #[must_use]
    pub const fn is_mono(self) -> bool {
        matches!(self.depth, Depth::Mono)
    }
}

/// Everything detected about the outer terminal before the roles are solved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Outer {
    /// How many colours the terminal renders.
    pub depth: Depth,
    /// The status ladder column the locale allows.
    pub glyphs: Glyphs,
    /// The OSC 11 reply, when the terminal answered.
    pub background: Option<Rgb8>,
    /// The OSC 10 reply, when the terminal answered.
    pub foreground: Option<Rgb8>,
    /// Light or dark from `COLORFGBG`, for when OSC 11 did not answer.
    pub hinted_mode: Option<Mode>,
}

impl Default for Outer {
    /// A truecolor UTF-8 terminal that answered nothing: the deterministic
    /// shape tests and `Theme::default` paint against.
    fn default() -> Self {
        Self {
            depth: Depth::TrueColor,
            glyphs: Glyphs::Unicode,
            background: None,
            foreground: None,
            hinted_mode: None,
        }
    }
}

/// The OSC 10/11 reply the attach probe recorded, `(foreground, background)`.
static REPORTED: Mutex<Option<(Rgb8, Rgb8)>> = Mutex::new(None);

/// Record the outer terminal's default colours; true when they changed.
///
/// The next theme build solves against the background the chrome really
/// sits on, from the attach probe or a later answer after a theme switch.
pub fn record_reported(colors: phux_protocol::caps::TerminalDefaultColors) -> bool {
    let rgb = |c: phux_protocol::caps::TerminalColor| Rgb8 {
        r: c.r,
        g: c.g,
        b: c.b,
    };
    let new = Some((rgb(colors.foreground), rgb(colors.background)));
    REPORTED
        .lock()
        .is_ok_and(|mut slot| std::mem::replace(&mut *slot, new) != new)
}

fn reported() -> Option<(Rgb8, Rgb8)> {
    REPORTED.lock().ok().and_then(|slot| *slot)
}

impl Outer {
    /// The process's terminal: its environment plus the recorded probe.
    /// Unit tests always get [`Outer::default`], so no developer's `TERM`
    /// or `NO_COLOR` leaks into an assertion.
    #[must_use]
    pub fn detect() -> Self {
        if cfg!(test) {
            return Self::default();
        }
        Self::from_env(|key| std::env::var(key).ok(), reported())
    }

    /// Detection over an explicit environment and probe reply.
    #[must_use]
    pub fn from_env(var: impl Fn(&str) -> Option<String>, reported: Option<(Rgb8, Rgb8)>) -> Self {
        let set = |key: &str| var(key).filter(|v| !v.is_empty());
        Self {
            depth: depth_from(set("NO_COLOR").is_some(), set("TERM"), set("COLORTERM")),
            glyphs: glyphs_from(
                set("LC_ALL")
                    .or_else(|| set("LC_CTYPE"))
                    .or_else(|| set("LANG")),
            ),
            foreground: reported.map(|(fg, _)| fg),
            background: reported.map(|(_, bg)| bg),
            hinted_mode: set("COLORFGBG").as_deref().and_then(colorfgbg_mode),
        }
    }
}

fn depth_from(no_color: bool, term: Option<String>, colorterm: Option<String>) -> Depth {
    let term = term.unwrap_or_default().to_ascii_lowercase();
    if no_color || term == "dumb" {
        return Depth::Mono;
    }
    let colorterm = colorterm.unwrap_or_default().to_ascii_lowercase();
    if matches!(colorterm.as_str(), "truecolor" | "24bit") || term.ends_with("-direct") {
        return Depth::TrueColor;
    }
    if term.contains("256") {
        return Depth::Ansi256;
    }
    // No `TERM` at all is not a terminal anyone describes (a test harness, a
    // pipe): keep the 24-bit default rather than guess a palette.
    if term.is_empty() {
        return Depth::TrueColor;
    }
    Depth::Ansi16
}

/// ASCII under a CJK locale or a locale that names a non-UTF-8 charset. An
/// unset, `C` or `POSIX` locale keeps the Unicode ladder: terminals that
/// leave the locale unset still draw UTF-8 almost universally.
fn glyphs_from(locale: Option<String>) -> Glyphs {
    let Some(locale) = locale else {
        return Glyphs::Unicode;
    };
    let lower = locale.to_ascii_lowercase();
    if ["zh", "ja", "ko"].iter().any(|p| lower.starts_with(p)) {
        return Glyphs::Ascii;
    }
    if matches!(lower.as_str(), "c" | "posix") || lower.contains("utf-8") || lower.contains("utf8")
    {
        return Glyphs::Unicode;
    }
    Glyphs::Ascii
}

/// `COLORFGBG` is `fg;bg` (or `fg;default;bg`); the ANSI index of the
/// background says light (7, 9–15) or dark (0–6, 8).
fn colorfgbg_mode(value: &str) -> Option<Mode> {
    let bg: u8 = value.rsplit(';').next()?.trim().parse().ok()?;
    Some(if bg == 7 || (9..=15).contains(&bg) {
        Mode::Light
    } else {
        Mode::Dark
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn depth_follows_no_color_colorterm_and_term() {
        let depth = |pairs: &[(&str, &str)]| Outer::from_env(env(pairs), None).depth;
        assert_eq!(
            depth(&[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")]),
            Depth::TrueColor
        );
        assert_eq!(depth(&[("COLORTERM", "24bit")]), Depth::TrueColor);
        assert_eq!(depth(&[("TERM", "xterm-direct")]), Depth::TrueColor);
        assert_eq!(depth(&[("TERM", "xterm-256color")]), Depth::Ansi256);
        assert_eq!(depth(&[("TERM", "screen-256color")]), Depth::Ansi256);
        assert_eq!(depth(&[("TERM", "xterm")]), Depth::Ansi16);
        assert_eq!(depth(&[("TERM", "linux")]), Depth::Ansi16);
        assert_eq!(depth(&[("TERM", "dumb")]), Depth::Mono);
        assert_eq!(
            depth(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]),
            Depth::Mono,
            "NO_COLOR beats every capability"
        );
        assert_eq!(
            depth(&[("NO_COLOR", ""), ("TERM", "xterm")]),
            Depth::Ansi16,
            "an empty NO_COLOR is unset"
        );
        assert_eq!(depth(&[]), Depth::TrueColor);
    }

    #[test]
    fn the_ladder_goes_ascii_under_cjk_or_non_utf8() {
        let glyphs = |pairs: &[(&str, &str)]| Outer::from_env(env(pairs), None).glyphs;
        assert_eq!(glyphs(&[("LANG", "en_US.UTF-8")]), Glyphs::Unicode);
        assert_eq!(glyphs(&[("LANG", "ja_JP.UTF-8")]), Glyphs::Ascii);
        assert_eq!(glyphs(&[("LC_CTYPE", "zh_CN.utf8")]), Glyphs::Ascii);
        assert_eq!(glyphs(&[("LANG", "ko_KR.UTF-8")]), Glyphs::Ascii);
        assert_eq!(glyphs(&[("LANG", "de_DE.ISO-8859-1")]), Glyphs::Ascii);
        assert_eq!(
            glyphs(&[("LC_ALL", "en_US.UTF-8"), ("LANG", "ja_JP.UTF-8")]),
            Glyphs::Unicode,
            "LC_ALL wins"
        );
        assert_eq!(glyphs(&[("LANG", "C")]), Glyphs::Unicode);
        assert_eq!(glyphs(&[]), Glyphs::Unicode);
    }

    #[test]
    fn colorfgbg_hints_the_mode_and_the_probe_is_carried() {
        let hint = |v: &str| Outer::from_env(env(&[("COLORFGBG", v)]), None).hinted_mode;
        assert_eq!(hint("15;0"), Some(Mode::Dark));
        assert_eq!(hint("0;15"), Some(Mode::Light));
        assert_eq!(hint("0;default;7"), Some(Mode::Light));
        assert_eq!(hint("12;8"), Some(Mode::Dark));
        assert_eq!(hint("junk"), None);
        let fg = Rgb8::hex(0xdd_dddd);
        let bg = Rgb8::hex(0x10_1010);
        let outer = Outer::from_env(env(&[]), Some((fg, bg)));
        assert_eq!((outer.foreground, outer.background), (Some(fg), Some(bg)));
    }
}
