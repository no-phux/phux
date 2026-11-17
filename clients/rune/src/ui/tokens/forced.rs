//! Forced colours (§1.11): with a system contrast theme on, every role
//! resolves to one of its colours. Layers are the window, ink is its text,
//! accent and selection its highlight; washes and shadows drop, and status
//! keeps its mark and word in the text colour.

use phux_design::tokens::{FlatRoles, StatusRoles};
use phux_design::{Rgb8, Rgba8, Roles};

/// The system colours, as tokens compare and hash them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Forced {
    window: Rgb8,
    text: Rgb8,
    highlight: Rgb8,
    highlight_text: Rgb8,
    disabled: Rgb8,
    link: Rgb8,
}

fn rgb(c: rune::Color) -> Rgb8 {
    Rgba8::parse(&c.hex()).map_or(Rgb8::BLACK, Rgba8::rgb)
}

impl From<rune::SystemContrast> for Forced {
    fn from(s: rune::SystemContrast) -> Self {
        Self {
            window: rgb(s.window),
            text: rgb(s.text),
            highlight: rgb(s.highlight),
            highlight_text: rgb(s.highlight_text),
            disabled: rgb(s.disabled),
            link: rgb(s.link),
        }
    }
}

const fn clear(c: Rgb8) -> Rgba8 {
    c.with_alpha_byte(0)
}

impl Forced {
    pub(crate) fn roles(&self, base: &Roles) -> Roles {
        let (w, t) = (self.window.opaque(), self.text.opaque());
        let mut r = *base;
        r.layer.ground = w;
        r.layer.sidebar = w;
        r.layer.raised = w;
        r.layer.overlay = w;
        r.text.text = t;
        r.text.dim = t;
        r.text.faint = self.disabled.opaque();
        r.text.inverse = w;
        for b in [
            &mut r.border.muted,
            &mut r.border.base,
            &mut r.border.strong,
            &mut r.border.control,
            &mut r.border.focus,
        ] {
            *b = t;
        }
        for wash in [&mut r.wash.hover, &mut r.wash.press, &mut r.wash.current] {
            *wash = clear(self.text);
        }
        // Text keeps CanvasText, so selection is the Highlight cue bar
        // (increased contrast is on with forced colours), not a fill.
        r.wash.selected = clear(self.text);
        r.accent.accent = self.highlight.opaque();
        r.accent.on_accent = self.highlight_text.opaque();
        r.accent.accent_text = self.link.opaque();
        r.accent.hi = r.accent.accent;
        r.accent.lo = r.accent.accent;
        r.status = status(self.text);
        r.selection.bg = self.highlight.opaque();
        r.selection.fg = self.highlight_text.opaque();
        r.r#match.bg = self.highlight.opaque();
        r.r#match.current_bg = self.highlight.opaque();
        r.r#match.current_fg = self.highlight_text.opaque();
        r.diff.add = clear(self.text);
        r.diff.remove = clear(self.text);
        r.diff.add_text = t;
        r.diff.remove_text = t;
        r.syntax = [t; 16];
        r.flat = flat(&r);
        r
    }
}

fn status(text: Rgb8) -> StatusRoles {
    let (t, c) = (text.opaque(), clear(text));
    StatusRoles {
        needs_you: t,
        needs_you_wash: c,
        needs_you_edge: t,
        needs_you_glow: c,
        working: t,
        working_wash: c,
        working_edge: t,
        working_glow: c,
        done: t,
        done_wash: c,
        done_edge: t,
        done_glow: c,
        failed: t,
        failed_wash: c,
        failed_edge: t,
        failed_glow: c,
        idle: t,
        idle_wash: c,
        idle_edge: t,
        idle_glow: c,
        unknown: t,
        unknown_wash: c,
        unknown_edge: t,
        unknown_glow: c,
        bell: t,
        bell_wash: c,
        bell_edge: t,
        bell_glow: c,
    }
}

fn flat(r: &Roles) -> FlatRoles {
    phux_design::solve::flatten(r)
}
