//! Chrome and overlay color theme: one [`Theme`] of named slots.
//!
//! The slots are pinned to [`SLOT_SPECS`] by tests and filled from the phux
//! design language (`phux_design`, spec §4.2).
//!
//! Theme sources layer in order:
//!
//! 1. the house look (`phux` / `phux-light`), solved against the terminal's
//!    reported background when it answered OSC 11;
//! 2. a `[theme] name|file` selection, solved by [`phux_design::derive`];
//! 3. per-slot `[theme]` overrides, which always win.
//!
//! The solver puts every text and rule slot at 4.5:1 or better on
//! [`Theme::surface`] (`text.faint` for rules, `text.dim` for recessive
//! text, `accent.accent_text` for focus). [`Term`] then fits the slots to
//! the terminal's colour depth (truecolor, 256, 16, or `NO_COLOR`).

mod depth;
mod term;

use std::str::FromStr;

use phux_config::plugin::PluginManifest;
use phux_config::theme::{ThemeColors, ThemeError, ThemeMode};
pub use phux_config::theme_slots::SLOT_SPECS;
use phux_design::{Mode, Rgb8, Rgba8, Roles};
use ratatui::style::{Color, Modifier, Style};

pub use depth::{identity_256, is_24_bit, nearest_256, xterm_rgb};
pub use term::{Depth, Glyphs, Outer, Term, record_reported};

/// Named color slots for chrome + overlay painting (ratatui [`Color`]s),
/// plus the [`Term`] they were fitted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Modal titles (help / prompt border title text).
    pub accent: Color,
    /// Keybinding chords in the help table: body text, drawn bold, so a
    /// chord never borrows a status hue.
    pub chord: Color,
    /// Action labels. Distinct slot so a theme can diverge chord vs
    /// action coloring without a code change.
    pub action: Color,
    /// De-emphasized text: footer hints, the "no bindings" notice.
    pub dim: Color,
    /// Modal borders.
    pub border: Color,
    /// Window / section titles where a theme wants them distinct from
    /// `text`. Defaults to body text.
    pub title: Color,
    /// Section headings inside grouped discovery surfaces.
    pub section_header: Color,
    /// Error / alarm text: the `failed` status tone.
    pub error: Color,
    /// Panel fill shared by floating overlays and the sidebar
    /// (`layer.sidebar`). Override with `Reset` to inherit the terminal
    /// background.
    pub surface: Color,
    /// Drop-shadow color painted one cell below + right of a floating
    /// modal. `Reset` (the shipped default) disables the shadow so the
    /// overlay is a sheet, not a window. Set a dark colour to opt in.
    pub shadow: Color,
    /// Foreground of selection chrome: the copy-mode status strip and
    /// selected list rows.
    pub selection_fg: Color,
    /// Background of selection chrome: the copy-mode status strip and
    /// selected list rows.
    pub selection_bg: Color,
    /// Attention chrome: the sidebar tab marker and the status-bar hint
    /// painted when an agent in a pane needs you (ADR-0035
    /// `AgentEvent::Asked`).
    pub attention: Color,
    /// Sidebar section headers: the faint lowercase headings.
    pub sidebar_section: Color,
    /// Agent lifecycle coloring: an `idle` agent.
    pub agent_idle: Color,
    /// Agent lifecycle coloring: a `working` agent.
    pub agent_working: Color,
    /// Agent lifecycle coloring: an agent that needs you.
    pub agent_blocked: Color,
    /// Agent lifecycle coloring: a `done` agent.
    pub agent_done: Color,
    /// Agent lifecycle coloring: a pane that `failed` (nonzero exit,
    /// crashed, or its host was lost).
    pub agent_failed: Color,
    /// Rules not touching the focused pane: recessive scaffolding, the same
    /// tone as `border` so every rule reads as one material.
    pub divider: Color,
    /// Rules bounding the focused pane. Focus is colour (plus bold), never a
    /// heavier stroke: mixed-weight junctions are broken in most fonts.
    pub divider_focus: Color,
    /// A pane's label, inset into its top rule, when the pane is not
    /// focused. Recessive like every other unfocused affordance.
    pub pane_title: Color,
    /// The focused pane's label. Rides `accent` (with `BOLD`) so "where
    /// am I typing" is answerable from the frame alone.
    pub pane_title_focus: Color,
    /// Body copy on a filled `surface` panel. A panel supplies its own
    /// background, so its text must supply its own foreground.
    pub text: Color,
    /// The terminal the slots were fitted to (not a `[theme]` slot).
    pub term: Term,
}

impl Default for Theme {
    /// The house look on a truecolor terminal that answered nothing.
    fn default() -> Self {
        Self::house(&Outer::default())
    }
}

/// An 8-bit role as a 24-bit ratatui colour.
const fn rgb(c: Rgba8) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

const fn rgb8(c: phux_config::theme::Rgb) -> Rgb8 {
    Rgb8 {
        r: c.r,
        g: c.g,
        b: c.b,
    }
}

impl Theme {
    /// The selected theme (ADR-0157) or the house look, with `[theme]` slot
    /// overrides layered on, for the terminal this process paints. Tolerant:
    /// what does not load or parse is warned about and the default stands.
    #[must_use]
    pub fn from_cfg(cfg: &phux_config::ThemeCfg, plugins: &[PluginManifest]) -> Self {
        let outer = Outer::detect();
        let base = match phux_config::theme::resolve_selected(cfg, plugins) {
            Ok(Some(colors)) => Self::from_colors_in(&colors, &outer),
            Ok(None) => Self::house(&outer),
            Err(err) => {
                tracing::warn!(error = %err, "selected theme did not load; using the house look");
                Self::house(&outer)
            }
        };
        base.with_overrides(cfg)
    }

    /// [`Self::from_cfg`], refusing instead of warning when the selected
    /// theme does not load (the strict build behind `phux config reload`).
    ///
    /// # Errors
    ///
    /// The [`ThemeError`] naming what did not load.
    pub fn resolve(
        cfg: &phux_config::ThemeCfg,
        plugins: &[PluginManifest],
    ) -> Result<Self, ThemeError> {
        let outer = Outer::detect();
        let base = phux_config::theme::resolve_selected(cfg, plugins)?.map_or_else(
            || Self::house(&outer),
            |colors| Self::from_colors_in(&colors, &outer),
        );
        Ok(base.with_overrides(cfg))
    }

    /// The house look. A reported background is solved against directly
    /// (with the house terminal's ANSI colours and accent), since that is
    /// the ground chrome text sits on; otherwise the authored roles in the
    /// `COLORFGBG` mode, else dark.
    #[must_use]
    pub fn house(outer: &Outer) -> Self {
        let Some(bg) = outer.background else {
            let mode = outer.hinted_mode.unwrap_or(Mode::Dark);
            return Self::from_roles(&Roles::house(mode), outer, false);
        };
        let p = phux_design::Params::house();
        let probe = phux_design::Theme {
            bg,
            fg: bg,
            ansi: [Rgb8::BLACK; 16],
            cursor: None,
            accent: None,
            selection_bg: None,
            dark: None,
        };
        let mode = phux_design::solve::mode_of(&probe, &p);
        let palette = match mode {
            Mode::Dark => &phux_design::tokens::TERMINAL.dark,
            Mode::Light => &phux_design::tokens::TERMINAL.light,
        };
        let mut ansi = [Rgb8::BLACK; 16];
        for (slot, c) in ansi.iter_mut().zip(palette.ansi) {
            *slot = c.rgb();
        }
        let input = phux_design::Theme {
            bg,
            fg: outer.foreground.unwrap_or_else(|| palette.fg.rgb()),
            ansi,
            cursor: Some(palette.cursor.rgb()),
            accent: Some(Roles::house(mode).accent.accent.rgb()),
            selection_bg: None,
            dark: Some(mode.is_dark()),
        };
        Self::from_roles(&phux_design::derive(&input, &p), outer, true)
    }

    /// Map a theme's colours onto the chrome slots through the one solver
    /// (spec §4.2) for the terminal `outer`: bg ← `background`, fg ←
    /// `foreground`, ANSI ← `color0..15`, accent ← `accent`, selection ←
    /// `selection`, cursor ← `cursor`.
    #[must_use]
    pub fn from_colors_in(colors: &ThemeColors, outer: &Outer) -> Self {
        let input = phux_design::Theme {
            bg: rgb8(colors.background()),
            fg: rgb8(colors.foreground()),
            ansi: colors.ansi16().map(rgb8),
            cursor: Some(rgb8(colors.cursor())),
            accent: Some(rgb8(colors.accent())),
            selection_bg: Some(rgb8(colors.selection())),
            dark: Some(colors.mode == ThemeMode::Dark),
        };
        Self::from_input(&input, outer)
    }

    /// Solve one terminal theme for `outer`. The ground is known only when
    /// the terminal reported this very background.
    #[must_use]
    pub fn from_input(input: &phux_design::Theme, outer: &Outer) -> Self {
        let roles = phux_design::derive(input, &phux_design::Params::house());
        Self::from_roles(&roles, outer, outer.background == Some(input.bg))
    }

    /// The slot map of spec §4.2, fitted to `outer`'s depth.
    #[must_use]
    pub fn from_roles(roles: &Roles, outer: &Outer, ground_known: bool) -> Self {
        let text = rgb(roles.text.text);
        let dim = rgb(roles.text.dim);
        let faint = rgb(roles.text.faint);
        let accent = rgb(roles.accent.accent_text);
        let s = &roles.status;
        let mut theme = Self {
            accent,
            chord: text,
            action: accent,
            dim,
            border: faint,
            title: text,
            section_header: dim,
            error: rgb(s.failed),
            surface: rgb(roles.layer.sidebar),
            shadow: Color::Reset,
            selection_fg: rgb(roles.selection.fg),
            selection_bg: rgb(roles.selection.bg),
            attention: rgb(s.needs_you),
            sidebar_section: faint,
            agent_idle: rgb(s.idle),
            agent_working: rgb(s.working),
            agent_blocked: rgb(s.needs_you),
            agent_done: rgb(s.done),
            agent_failed: rgb(s.failed),
            divider: faint,
            divider_focus: accent,
            pane_title: dim,
            pane_title_focus: accent,
            text,
            term: Term {
                depth: outer.depth,
                glyphs: outer.glyphs,
                ground_known,
            },
        };
        let ground = ground_known.then(|| roles.layer.ground.rgb());
        match outer.depth {
            Depth::TrueColor => {}
            Depth::Ansi256 => depth::to_256(&mut theme, roles.mode, ground),
            Depth::Ansi16 => depth::ansi16(&mut theme, roles.mode, ground_known),
            Depth::Mono => depth::mono(&mut theme),
        }
        theme
    }

    /// The fill under chrome drawn on the terminal's own background (pane
    /// rules and titles). `Reset` when the roles were solved against that
    /// background, or when the slots are the terminal's own ANSI colours;
    /// otherwise `surface`, since no ink is safe on an unknown ground.
    #[must_use]
    pub const fn ground_fill(&self) -> Color {
        match self.term.depth {
            Depth::TrueColor | Depth::Ansi256 if !self.term.ground_known => self.surface,
            _ => Color::Reset,
        }
    }

    /// Whether a selection is reverse video rather than a fill: with 16
    /// colours or none, no fill is legible under every ink.
    #[must_use]
    pub const fn selection_reverses(&self) -> bool {
        matches!(self.term.depth, Depth::Ansi16 | Depth::Mono)
    }

    /// `style` on the selection bed, or in reverse video where the depth
    /// has no safe fill ([`Self::selection_reverses`]).
    #[must_use]
    pub const fn on_selection(&self, style: Style) -> Style {
        let style = style.bg(self.selection_bg);
        if self.selection_reverses() {
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        }
    }

    /// A bold strip on the `error` fill (a warn notice, the config-error
    /// line). Its ink is `surface`: the failed tone clears 4.5:1 against
    /// the surface by construction, and contrast is symmetric. Without
    /// colour, reverse video carries the strip.
    #[must_use]
    pub fn alarm(&self) -> Style {
        let style = Style::default()
            .fg(self.surface)
            .bg(self.error)
            .add_modifier(Modifier::BOLD);
        if self.term.is_mono() {
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        }
    }

    /// An identity colour (`[[sessions]]`, a host) fitted to the depth.
    #[must_use]
    pub fn identity(&self, color: Color) -> Color {
        match self.term.depth {
            Depth::TrueColor | Depth::Ansi16 => color,
            Depth::Ansi256 => identity_256(color),
            Depth::Mono => Color::Reset,
        }
    }

    /// Layer the `[theme]` slot overrides (every key but the reserved
    /// selectors) onto `self`.
    fn with_overrides(mut self, cfg: &phux_config::ThemeCfg) -> Self {
        let theme = &mut self;
        for (key, spec) in cfg.slot_overrides() {
            let Some(slot) = theme.slot_mut(key) else {
                tracing::warn!(slot = key, "unknown theme slot; ignoring");
                continue;
            };
            match parse_color(spec) {
                Some(color) => *slot = color,
                None => {
                    tracing::warn!(
                        slot = key,
                        color = spec,
                        "unparseable theme color; keeping default"
                    );
                }
            }
        }
        self
    }

    /// The color in the slot named `key`, or `None` if `key` is not a
    /// recognized slot. Slot names match the field names.
    #[must_use]
    pub fn slot(&self, key: &str) -> Option<Color> {
        let mut copy = *self;
        copy.slot_mut(key).map(|slot| *slot)
    }

    /// Every slot, by name, in field order (the depth fitters walk them).
    const fn slots_mut(&mut self) -> [(&'static str, &mut Color); 24] {
        [
            ("accent", &mut self.accent),
            ("chord", &mut self.chord),
            ("action", &mut self.action),
            ("dim", &mut self.dim),
            ("border", &mut self.border),
            ("title", &mut self.title),
            ("section_header", &mut self.section_header),
            ("error", &mut self.error),
            ("surface", &mut self.surface),
            ("shadow", &mut self.shadow),
            ("selection_fg", &mut self.selection_fg),
            ("selection_bg", &mut self.selection_bg),
            ("attention", &mut self.attention),
            ("sidebar_section", &mut self.sidebar_section),
            ("agent_idle", &mut self.agent_idle),
            ("agent_working", &mut self.agent_working),
            ("agent_blocked", &mut self.agent_blocked),
            ("agent_done", &mut self.agent_done),
            ("agent_failed", &mut self.agent_failed),
            ("divider", &mut self.divider),
            ("divider_focus", &mut self.divider_focus),
            ("pane_title", &mut self.pane_title),
            ("pane_title_focus", &mut self.pane_title_focus),
            ("text", &mut self.text),
        ]
    }

    /// Mutable handle to the slot named `key`, or `None` if `key` is not
    /// a recognized slot. Slot names match the field names.
    fn slot_mut(&mut self, key: &str) -> Option<&mut Color> {
        let mut slots = self.slots_mut().into_iter();
        slots.find(|(name, _)| *name == key).map(|(_, slot)| slot)
    }
}

/// Parse a color string into a ratatui [`Color`]. `None` when ratatui
/// can't interpret it (caller keeps the slot default).
fn parse_color(spec: &str) -> Option<Color> {
    Color::from_str(spec).ok()
}

/// Render a [`Color`] the way `[theme]` accepts it back: `#rrggbb` for
/// RGB, the index for an indexed color, the lowercase name otherwise
/// (`reset` for the terminal default).
#[must_use]
pub fn color_to_string(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(index) => index.to_string(),
        other => other.to_string().to_lowercase(),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
