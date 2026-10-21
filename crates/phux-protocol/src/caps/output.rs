//! The output tier a client renders at: color support and emitter mode.

/// A client's color tier (SPEC §6.2), most to least permissive. The server
/// rewrites outbound VT bytes to fit; `TrueColor` is the default so an
/// unadvertised client is never silently downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ColorSupport {
    /// 24-bit direct RGB; SGR truecolor is forwarded verbatim.
    #[default]
    TrueColor,
    /// xterm 256-color palette.
    Indexed256,
    /// 16 system colors only.
    Indexed16,
    /// Monochrome: SGR color sequences MUST be stripped.
    Mono,
}

impl ColorSupport {
    /// Wire tag. Stable within v0.x; new variants append.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        match self {
            Self::TrueColor => 0,
            Self::Indexed256 => 1,
            Self::Indexed16 => 2,
            Self::Mono => 3,
        }
    }

    /// Inverse of [`Self::as_wire`]; `None` for an unknown tag.
    #[must_use]
    pub const fn from_wire(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::TrueColor,
            1 => Self::Indexed256,
            2 => Self::Indexed16,
            3 => Self::Mono,
            _ => return None,
        })
    }
}

/// How the server emits terminal content to this consumer (SPEC §6.2).
///
/// Either the byte-faithful raw PTY broadcast, or a per-consumer synthesized
/// state-sync tick for agent / remote consumers that want a coherent grid
/// model. An unknown wire tag decodes as [`OutputMode::Raw`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum OutputMode {
    /// Raw PTY byte broadcast: the default, human-TUI path.
    #[default]
    Raw,
    /// Per-consumer synthesized grid deltas with a monotonic `seq` (ADR-0018).
    StateSync,
}

impl OutputMode {
    /// Wire tag. Stable within v0.x; new variants append.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        match self {
            Self::Raw => 0,
            Self::StateSync => 1,
        }
    }

    /// Inverse of [`Self::as_wire`]; an unknown tag is [`OutputMode::Raw`].
    #[must_use]
    pub const fn from_wire(tag: u8) -> Self {
        match tag {
            1 => Self::StateSync,
            _ => Self::Raw,
        }
    }
}

/// Detect the client terminal's color tier from environment hints.
///
/// `$COLORTERM`, then `$TERM` (suffixes; `dumb` is mono), then
/// `$TERM_PROGRAM`, falling back to truecolor: an over-claim is recoverable,
/// an under-claim silently degrades.
#[must_use]
pub fn detect_color_support() -> ColorSupport {
    detect_from_env(|key| std::env::var(key).ok())
}

/// [`detect_color_support`] over an injected environment lookup.
fn detect_from_env<F>(env: F) -> ColorSupport
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(ct) = env("COLORTERM") {
        let ct_lc = ct.to_ascii_lowercase();
        if ct_lc == "truecolor" || ct_lc == "24bit" {
            return ColorSupport::TrueColor;
        }
    }

    let term = env("TERM").unwrap_or_default();
    let term_lc = term.to_ascii_lowercase();
    if term_lc.ends_with("-direct") || term_lc.ends_with("-truecolor") {
        return ColorSupport::TrueColor;
    }
    if term_lc.ends_with("-256color") {
        return ColorSupport::Indexed256;
    }
    if !term_lc.is_empty() && !term_lc.contains("color") {
        // `xterm`, `linux`, `vt100`: anything richer carries a suffix.
        if term_lc == "dumb" {
            return ColorSupport::Mono;
        }
        return ColorSupport::Indexed16;
    }

    if let Some(tp) = env("TERM_PROGRAM") {
        let tp_lc = tp.to_ascii_lowercase();
        if tp_lc == "iterm.app" || tp_lc == "wezterm" {
            return ColorSupport::TrueColor;
        }
        if tp_lc == "apple_terminal" {
            return ColorSupport::Indexed256;
        }
    }

    ColorSupport::TrueColor
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_map(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn color_support_wire_roundtrips_every_variant() {
        for v in [
            ColorSupport::TrueColor,
            ColorSupport::Indexed256,
            ColorSupport::Indexed16,
            ColorSupport::Mono,
        ] {
            let tag = v.as_wire();
            let back = ColorSupport::from_wire(tag).expect("known tag");
            assert_eq!(back, v);
        }
        assert!(ColorSupport::from_wire(0xFF).is_none());
    }

    #[test]
    fn color_support_detection_from_env() {
        let cases: &[(&[(&str, &str)], ColorSupport)] = &[
            (
                &[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")],
                ColorSupport::TrueColor,
            ),
            (
                &[("COLORTERM", "24bit"), ("TERM", "xterm")],
                ColorSupport::TrueColor,
            ),
            (&[("TERM", "xterm-256color")], ColorSupport::Indexed256),
            (&[("TERM", "xterm-direct")], ColorSupport::TrueColor),
            (&[("TERM", "xterm")], ColorSupport::Indexed16),
            (&[("TERM", "dumb")], ColorSupport::Mono),
            (
                &[("TERM_PROGRAM", "Apple_Terminal")],
                ColorSupport::Indexed256,
            ),
            (&[("TERM_PROGRAM", "iTerm.app")], ColorSupport::TrueColor),
            (&[], ColorSupport::TrueColor),
        ];
        for (pairs, want) in cases {
            assert_eq!(detect_from_env(env_map(pairs)), *want, "{pairs:?}");
        }
    }
}
