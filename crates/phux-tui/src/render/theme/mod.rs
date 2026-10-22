//! Chrome + overlay color theme: every chrome and overlay color resolves
//! through one [`Theme`] of named semantic slots, owned by the attach driver
//! and threaded into the paint path.
//!
//! [`SLOT_SPECS`] (owned by [`phux_config::theme_slots`], the slot
//! vocabulary `phux config check` validates against) documents each slot;
//! tests pin it to [`Theme`]'s fields.
//!
//! ## Contrast
//!
//! Every slot that paints text or a rule clears WCAG AA **4.5:1** against
//! [`Theme::surface`] (the shipped dark panel fill): a rule you cannot see
//! is missing, not subtle. "Recessive" is a relationship expressed as three
//! rungs that all clear the floor: structure (`border`/`divider`) < recessive
//! text (`dim`) < focus (`accent`, plus bold, in a saturated hue). Tests pin
//! the floor and the ordering. On a light terminal the recessive rungs land
//! near 3.5:1; every slot is overridable.
//!
//! ## Overrides
//!
//! [`Theme::from_cfg`] layers `[theme]` ([`phux_config::ThemeCfg`], a
//! `slot -> color` map) over the defaults; unknown keys and unparseable
//! colors are ignored with a warning (and flagged by `phux config check`).

use std::str::FromStr;

use phux_config::plugin::PluginManifest;
use phux_config::theme::{ThemeColors, ThemeError, ThemeMode};
pub use phux_config::theme_slots::SLOT_SPECS;
use ratatui::style::Color;

/// Named color slots for chrome + overlay painting (ratatui [`Color`]s).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Modal titles (help / prompt border title text).
    pub accent: Color,
    /// Keybinding chords in the help table.
    pub chord: Color,
    /// Action labels. Distinct slot so a theme can diverge chord vs
    /// action coloring without a code change; defaults to the terminal
    /// foreground (no explicit color) like the action column does today.
    pub action: Color,
    /// De-emphasized text: footer hints, the "no bindings" notice.
    pub dim: Color,
    /// Modal borders.
    pub border: Color,
    /// Window / section titles where a theme wants them distinct from
    /// `accent`. Defaults to the same value as `accent`.
    pub title: Color,
    /// Section headings inside grouped discovery surfaces.
    pub section_header: Color,
    /// Error / alarm text.
    pub error: Color,
    /// Panel fill shared by floating overlays and the sidebar. Override with
    /// `Reset` to inherit the terminal background.
    pub surface: Color,
    /// Drop-shadow color painted one cell below + right of a floating
    /// modal. `Reset` (the shipped default) disables the shadow so the
    /// overlay is a sheet, not a window. Set a dark colour to opt in.
    pub shadow: Color,
    /// Foreground of selection chrome: the copy-mode status strip (and
    /// future selected list rows).
    pub selection_fg: Color,
    /// Background of selection chrome: the copy-mode status strip (and
    /// future selected list rows).
    pub selection_bg: Color,
    /// Attention chrome: the sidebar tab marker and the
    /// status-bar hint painted when an agent in a pane is waiting on a
    /// human answer (ADR-0035 `AgentEvent::Asked`).
    pub attention: Color,
    /// Sidebar section headers: the muted lowercase
    /// `spaces` / `agents` headings of the herdr-shaped sidebar.
    pub sidebar_section: Color,
    /// Agent lifecycle coloring: an `idle` agent row's
    /// glyph + state text in the sidebar's agents section.
    pub agent_idle: Color,
    /// Agent lifecycle coloring: a `working` agent row.
    pub agent_working: Color,
    /// Agent lifecycle coloring: a `blocked` agent row
    /// (waiting on a human).
    pub agent_blocked: Color,
    /// Agent lifecycle coloring: a `done` agent row.
    pub agent_done: Color,
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
    /// Body copy on a filled `surface` panel. Unlike `action` (`Reset`, for
    /// text on the host background), a panel supplies its own background, so
    /// its text must supply its own foreground.
    pub text: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            // docs/experience.md: one lime focus signal, mint key chords, and neutral
            // slate structure. Panels always supply both foreground and fill.
            accent: Color::Rgb(0xbe, 0xf2, 0x64),
            chord: Color::Rgb(0x86, 0xef, 0xac),
            action: Color::Reset,
            dim: Color::Rgb(0x9a, 0xa4, 0xb2),
            border: Color::Rgb(0x7c, 0x86, 0x96),
            title: Color::Rgb(0xbe, 0xf2, 0x64),
            section_header: Color::Rgb(0x9a, 0xa4, 0xb2),
            error: Color::Rgb(0xf8, 0x71, 0x71),
            surface: Color::Rgb(0x17, 0x1b, 0x23),
            shadow: Color::Reset,
            selection_fg: Color::Rgb(0xf4, 0xf7, 0xfb),
            selection_bg: Color::Rgb(0x29, 0x36, 0x28),
            attention: Color::Rgb(0xfd, 0xe0, 0x47),
            sidebar_section: Color::Rgb(0x9a, 0xa4, 0xb2),
            agent_idle: Color::Rgb(0x9a, 0xa4, 0xb2),
            agent_working: Color::Rgb(0x86, 0xef, 0xac),
            agent_blocked: Color::Rgb(0xfd, 0xe0, 0x47),
            agent_done: Color::Rgb(0xbe, 0xf2, 0x64),
            divider: Color::Rgb(0x7c, 0x86, 0x96),
            divider_focus: Color::Rgb(0xbe, 0xf2, 0x64),
            pane_title: Color::Rgb(0x9a, 0xa4, 0xb2),
            pane_title_focus: Color::Rgb(0xbe, 0xf2, 0x64),
            text: Color::Rgb(0xf4, 0xf7, 0xfb),
        }
    }
}

impl Theme {
    /// The selected theme (`[theme] name` / `file`, ADR-0157) or the shipped
    /// palette, with the remaining `[theme]` slot overrides layered on.
    /// Tolerant: a theme that does not load is warned about and the shipped
    /// palette stands in. Values parse like ratatui's `Color` (`"cyan"`,
    /// `"#cdd6f4"`, `"12"`); unknown keys and unparseable values keep the
    /// default (warned).
    #[must_use]
    pub fn from_cfg(cfg: &phux_config::ThemeCfg, plugins: &[PluginManifest]) -> Self {
        let base = match phux_config::theme::resolve_selected(cfg, plugins) {
            Ok(Some(colors)) => Self::from_colors(&colors),
            Ok(None) => Self::default(),
            Err(err) => {
                tracing::warn!(error = %err, "selected theme did not load; using the shipped palette");
                Self::default()
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
        let base = phux_config::theme::resolve_selected(cfg, plugins)?
            .map_or_else(Self::default, |colors| Self::from_colors(&colors));
        Ok(base.with_overrides(cfg))
    }

    /// Map a theme's semantic colours onto the chrome slots (ADR-0157
    /// decision 5). Text rides `foreground` / `bright_foreground`, rules
    /// ride `muted`, the one focus hue is `accent`, agent lifecycle borrows
    /// the terminal's green / yellow, and panels fill with the raised shade
    /// of the background (`lighter_background` on dark, `dark_background` on
    /// light) so a panel reads as a surface against the terminal.
    #[must_use]
    pub fn from_colors(colors: &ThemeColors) -> Self {
        let rgb = |key: &str| {
            colors
                .get(key)
                .map_or(Color::Reset, |c| Color::Rgb(c.r, c.g, c.b))
        };
        let accent = rgb("accent");
        let green = rgb("green");
        let yellow = rgb("yellow");
        let fg = rgb("foreground");
        let bright_fg = rgb("bright_foreground");
        let muted = rgb("muted");
        let surface = match colors.mode {
            ThemeMode::Dark => rgb("lighter_background"),
            ThemeMode::Light => rgb("dark_background"),
        };
        Self {
            accent,
            chord: green,
            action: Color::Reset,
            dim: fg,
            border: muted,
            title: accent,
            section_header: fg,
            error: rgb("red"),
            surface,
            shadow: Color::Reset,
            selection_fg: rgb("selection_foreground"),
            selection_bg: rgb("selection"),
            attention: yellow,
            sidebar_section: fg,
            agent_idle: fg,
            agent_working: green,
            agent_blocked: yellow,
            agent_done: accent,
            divider: muted,
            divider_focus: accent,
            pane_title: fg,
            pane_title_focus: accent,
            text: bright_fg,
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

    /// Mutable handle to the slot named `key`, or `None` if `key` is not
    /// a recognized slot. Slot names match the field names.
    fn slot_mut(&mut self, key: &str) -> Option<&mut Color> {
        match key {
            "accent" => Some(&mut self.accent),
            "chord" => Some(&mut self.chord),
            "action" => Some(&mut self.action),
            "dim" => Some(&mut self.dim),
            "border" => Some(&mut self.border),
            "title" => Some(&mut self.title),
            "section_header" => Some(&mut self.section_header),
            "error" => Some(&mut self.error),
            "surface" => Some(&mut self.surface),
            "shadow" => Some(&mut self.shadow),
            "selection_fg" => Some(&mut self.selection_fg),
            "selection_bg" => Some(&mut self.selection_bg),
            "attention" => Some(&mut self.attention),
            "sidebar_section" => Some(&mut self.sidebar_section),
            "agent_idle" => Some(&mut self.agent_idle),
            "agent_working" => Some(&mut self.agent_working),
            "agent_blocked" => Some(&mut self.agent_blocked),
            "agent_done" => Some(&mut self.agent_done),
            "divider" => Some(&mut self.divider),
            "divider_focus" => Some(&mut self.divider_focus),
            "pane_title" => Some(&mut self.pane_title),
            "pane_title_focus" => Some(&mut self.pane_title_focus),
            "text" => Some(&mut self.text),
            _ => None,
        }
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
