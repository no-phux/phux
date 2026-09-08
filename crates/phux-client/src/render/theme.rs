//! Chrome + overlay color theme (phux-ahv.4).
//!
//! Single source of truth for the hand-picked colors that the chrome
//! (status bar, dividers) and overlays (help, prompt) paint with. Before
//! this module those colors were scattered `Color::Cyan` / `Color::Yellow`
//! literals inside each overlay's `render`; now every chrome/overlay slot
//! resolves through one [`Theme`] value, owned by the attach driver
//! alongside the keybindings snapshot and threaded into the paint path.
//!
//! ## Slots
//!
//! A [`Theme`] is a flat set of named [`Color`] slots, each mapped to one
//! semantic role:
//!
//! - [`accent`] — modal titles (e.g. the help / prompt border title).
//! - [`chord`] — keybinding chords in the help table.
//! - [`action`] — reserved for action labels (kept distinct from `chord`
//!   so a future restyle can split them without churning callers).
//! - [`dim`] — de-emphasized text (footer hints, "no bindings" notice).
//! - [`border`] — modal borders.
//! - [`title`] — alias slot for window/section titles distinct from
//!   `accent` when a theme wants them to diverge.
//! - [`section_header`] — section headings inside grouped discovery surfaces.
//! - [`error`] — error / alarm text.
//! - [`sidebar_section`] — the sidebar's muted `spaces` / `agents`
//!   section headers (phux-foz.9).
//! - [`divider`] / [`divider_focus`] — the pane-divider rules: the
//!   recessive structural tone, and the focused pane's own frame.
//! - [`pane_title`] / [`pane_title_focus`] — the label inset into a
//!   pane's top rule.
//! - [`text`] — body copy that sits on a filled [`Theme::surface`] panel, where
//!   inheriting the terminal foreground would be unreadable.
//! - [`agent_idle`] / [`agent_working`] / [`agent_blocked`] /
//!   [`agent_done`] — agent lifecycle state colors in the sidebar's
//!   agents section (phux-foz.9).
//!
//! [`accent`]: Theme::accent
//! [`chord`]: Theme::chord
//! [`action`]: Theme::action
//! [`dim`]: Theme::dim
//! [`border`]: Theme::border
//! [`title`]: Theme::title
//! [`section_header`]: Theme::section_header
//! [`error`]: Theme::error
//! [`sidebar_section`]: Theme::sidebar_section
//! [`agent_idle`]: Theme::agent_idle
//! [`agent_working`]: Theme::agent_working
//! [`agent_blocked`]: Theme::agent_blocked
//! [`agent_done`]: Theme::agent_done
//! [`divider`]: Theme::divider
//! [`divider_focus`]: Theme::divider_focus
//! [`pane_title`]: Theme::pane_title
//! [`pane_title_focus`]: Theme::pane_title_focus
//! [`text`]: Theme::text
//!
//! ## Contrast
//!
//! Every slot that paints TEXT OR A RULE must clear **4.5:1** against
//! [`Theme::surface`] (#171b23, the shipped panel fill and a fair stand-in for
//! a dark terminal background). That is the WCAG 2.1 AA floor for normal
//! text, and it is the floor here too, because a chrome rule you cannot
//! see is not subtle — it is missing.
//!
//! "Recessive" is a RELATIONSHIP between slots, not a licence to sit at
//! the edge of visibility. The register is therefore expressed as three
//! rungs that all clear the floor:
//!
//! | Rung                                 | Slot                  | Ratio |
//! |--------------------------------------|-----------------------|-------|
//! | structure (rules, modal borders)     | `border` / `divider`  | >=4.5:1 |
//! | recessive text (hints, sub-lines)    | `dim` and its trackers| >rules |
//! | what you are looking at              | `accent` (plus BOLD)  | >text |
//!
//! Focus is separated from the rest by three things at once — a brighter
//! tone, a SATURATED hue against desaturated blue-greys, and `BOLD` — so
//! the hierarchy survives a terminal that flattens any one of them.
//! `contrast_floor_is_met` asserts the floor; it is a test rather than a
//! comment so a future retune cannot quietly drop below it.
//!
//! The floor is measured against a dark background because the shipped
//! palette is a dark one throughout. On a light terminal the recessive
//! rungs land near 3.5:1; `[theme]` is the escape hatch, and every slot
//! below is overridable.
//!
//! ## Overrides
//!
//! [`Theme::from_cfg`] reads `[theme]` from `phux_config` — a free-form
//! `slot -> color-string` map ([`phux_config::ThemeCfg`]). Recognized
//! slot keys override the default; an unknown key is ignored and an
//! unparseable color string falls back to the slot's default (both
//! logged at `warn`).

use std::str::FromStr;

use ratatui::style::Color;

/// Named color slots for chrome + overlay painting.
///
/// Construct the default with [`Theme::default`] or layer config
/// overrides with [`Theme::from_cfg`]. Each field is a ratatui [`Color`]
/// so consumers under `render/` can drop it straight into a [`Style`].
///
/// [`Style`]: ratatui::style::Style
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
    /// modal, giving it depth over the live panes. A subtle dark by
    /// default so it reads as a shadow on most terminals.
    pub shadow: Color,
    /// Foreground of selection chrome: the copy-mode status strip (and
    /// future selected list rows).
    pub selection_fg: Color,
    /// Background of selection chrome: the copy-mode status strip (and
    /// future selected list rows).
    pub selection_bg: Color,
    /// Attention chrome (phux-foz.1): the sidebar tab marker and the
    /// status-bar hint painted when an agent in a pane is waiting on a
    /// human answer (ADR-0035 `AgentEvent::Asked`).
    pub attention: Color,
    /// Sidebar section headers (phux-foz.9): the muted lowercase
    /// `spaces` / `agents` headings of the herdr-shaped sidebar.
    pub sidebar_section: Color,
    /// Agent lifecycle coloring (phux-foz.9): an `idle` agent row's
    /// glyph + state text in the sidebar's agents section.
    pub agent_idle: Color,
    /// Agent lifecycle coloring (phux-foz.9): a `working` agent row.
    pub agent_working: Color,
    /// Agent lifecycle coloring (phux-foz.9): a `blocked` agent row
    /// (waiting on a human).
    pub agent_blocked: Color,
    /// Agent lifecycle coloring (phux-foz.9): a `done` agent row.
    pub agent_done: Color,
    /// Pane-divider rules that do not touch the focused pane. The
    /// recessive structural register: a rule is scaffolding, never
    /// content. Defaults to the same tone as `border` so every rule in
    /// the chrome — modal frames, the sidebar's edge, the pane grid —
    /// reads as one material.
    pub divider: Color,
    /// The rules bounding the FOCUSED pane. Focus is carried by color
    /// (plus `BOLD`), never by a heavier box-drawing weight: mixed-weight
    /// junctions (`\u{2545}`, `\u{2548}`, ...) are missing or misaligned in
    /// most terminal fonts, so a uniformly light grid tinted at the focus
    /// is both sharper and more portable.
    pub divider_focus: Color,
    /// A pane's label, inset into its top rule, when the pane is not
    /// focused. Recessive like every other unfocused affordance.
    pub pane_title: Color,
    /// The focused pane's label. Rides `accent` (with `BOLD`) so "where
    /// am I typing" is answerable from the frame alone.
    pub pane_title_focus: Color,
    /// Body copy painted ON a filled `surface` panel.
    ///
    /// Distinct from `action` on purpose. `action` is `Reset` because it
    /// labels things drawn on the HOST background (the sidebar, the
    /// status row), which the user chose. A modal panel supplies its own
    /// background, so its text has to supply its own foreground or it
    /// inverts into unreadability on a light terminal.
    pub text: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            // DESIGN.md: one lime focus signal, mint key chords, and neutral
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
            shadow: Color::Rgb(0x09, 0x0b, 0x0f),
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
    /// Build a theme from the default, layering `[theme]` config
    /// overrides on top.
    ///
    /// Each recognized slot key in `cfg.slots` whose value parses as a
    /// color replaces the default for that slot. Unknown keys are
    /// ignored (warn); unparseable color strings keep the default
    /// (warn). Parsing accepts everything ratatui's [`Color`] `FromStr`
    /// accepts: named colors (`"cyan"`), hex (`"#cdd6f4"`), and ANSI
    /// indices (`"12"`).
    #[must_use]
    pub fn from_cfg(cfg: &phux_config::ThemeCfg) -> Self {
        let mut theme = Self::default();
        for (key, spec) in &cfg.slots {
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
        theme
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

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn cfg(pairs: &[(&str, &str)]) -> phux_config::ThemeCfg {
        let mut slots = BTreeMap::new();
        for (k, v) in pairs {
            slots.insert((*k).to_owned(), (*v).to_owned());
        }
        phux_config::ThemeCfg { slots }
    }

    #[test]
    fn default_slots_match_shipped_colors() {
        let t = Theme::default();
        assert_eq!(t.accent, Color::Rgb(0xbe, 0xf2, 0x64));
        assert_eq!(t.chord, Color::Rgb(0x86, 0xef, 0xac));
        assert_eq!(t.action, Color::Reset);
        assert_eq!(t.dim, Color::Rgb(0x9a, 0xa4, 0xb2));
        assert_eq!(t.border, Color::Rgb(0x7c, 0x86, 0x96));
        assert_eq!(t.title, Color::Rgb(0xbe, 0xf2, 0x64));
        assert_eq!(t.section_header, Color::Rgb(0x9a, 0xa4, 0xb2));
        assert_eq!(t.error, Color::Rgb(0xf8, 0x71, 0x71));
        // Design tokens for floating-modal depth + selection chrome.
        assert_eq!(t.surface, Color::Rgb(0x17, 0x1b, 0x23));
        assert_eq!(t.shadow, Color::Rgb(0x09, 0x0b, 0x0f));
        assert_eq!(t.selection_fg, Color::Rgb(0xf4, 0xf7, 0xfb));
        assert_eq!(t.selection_bg, Color::Rgb(0x29, 0x36, 0x28));
        assert_eq!(t.attention, Color::Rgb(0xfd, 0xe0, 0x47));
        assert_eq!(t.sidebar_section, Color::Rgb(0x9a, 0xa4, 0xb2));
        assert_eq!(t.agent_idle, Color::Rgb(0x9a, 0xa4, 0xb2));
        assert_eq!(t.agent_working, Color::Rgb(0x86, 0xef, 0xac));
        assert_eq!(t.agent_blocked, Color::Rgb(0xfd, 0xe0, 0x47));
        assert_eq!(t.agent_done, Color::Rgb(0xbe, 0xf2, 0x64));
    }

    /// The structural chrome roles (phux-l96p.8) ride the same lime/slate
    /// palette; split from the test above only to keep each one readable.
    #[test]
    fn structural_slots_match_shipped_colors() {
        let t = Theme::default();
        assert_eq!(t.divider, Color::Rgb(0x7c, 0x86, 0x96));
        assert_eq!(t.divider_focus, Color::Rgb(0xbe, 0xf2, 0x64));
        assert_eq!(t.pane_title, Color::Rgb(0x9a, 0xa4, 0xb2));
        assert_eq!(t.pane_title_focus, Color::Rgb(0xbe, 0xf2, 0x64));
        assert_eq!(t.text, Color::Rgb(0xf4, 0xf7, 0xfb));
    }

    /// The shipped palette is a system, not a bag of colors: the slots
    /// that are documented as sharing a tone must actually share it, so a
    /// future retune of one cannot silently split the pair.
    #[test]
    fn shared_register_slots_stay_in_step() {
        let t = Theme::default();
        assert_eq!(t.title, t.accent, "titles ride the accent hue");
        assert_eq!(t.sidebar_section, t.dim, "section headers are dim-register");
        assert_eq!(
            t.agent_idle, t.dim,
            "an idle agent recedes like any dim chrome"
        );
        assert_eq!(
            t.agent_blocked, t.attention,
            "a blocked agent and an attention marker are one semantic"
        );
        assert_eq!(
            t.agent_working, t.chord,
            "working shares the live-progress green"
        );
        assert_eq!(
            t.divider, t.border,
            "every rule in the chrome is one material"
        );
        assert_eq!(
            t.divider_focus, t.accent,
            "the focused frame rides the one phux hue"
        );
        assert_eq!(
            t.pane_title, t.dim,
            "an unfocused pane label recedes like any dim chrome"
        );
        assert_eq!(
            t.pane_title_focus, t.accent,
            "the focused pane label rides the same hue as its frame"
        );
        assert_eq!(
            t.text, t.selection_fg,
            "panel body copy and a selected row are the same text"
        );
    }

    /// phux-foz.9: every sidebar/agent slot is config-overridable like the
    /// rest — unknown-slot warnings would otherwise silently eat them.
    #[test]
    fn sidebar_and_agent_slots_are_overridable() {
        let t = Theme::from_cfg(&cfg(&[
            ("sidebar_section", "#6c7086"),
            ("agent_idle", "white"),
            ("agent_working", "green"),
            ("agent_blocked", "red"),
            ("agent_done", "blue"),
        ]));
        assert_eq!(t.sidebar_section, Color::Rgb(0x6c, 0x70, 0x86));
        assert_eq!(t.agent_idle, Color::White);
        assert_eq!(t.agent_working, Color::Green);
        assert_eq!(t.agent_blocked, Color::Red);
        assert_eq!(t.agent_done, Color::Blue);
        assert_eq!(t.accent, Theme::default().accent);
    }

    #[test]
    fn attention_slot_is_overridable() {
        let t = Theme::from_cfg(&cfg(&[("attention", "#f38ba8")]));
        assert_eq!(t.attention, Color::Rgb(0xf3, 0x8b, 0xa8));
        assert_eq!(t.accent, Theme::default().accent);
    }

    #[test]
    fn structural_chrome_slots_are_overridable() {
        let t = Theme::from_cfg(&cfg(&[
            ("divider", "#45475a"),
            ("divider_focus", "#89b4fa"),
            ("pane_title", "#6c7086"),
            ("pane_title_focus", "#89b4fa"),
            ("text", "#cdd6f4"),
        ]));
        assert_eq!(t.divider, Color::Rgb(0x45, 0x47, 0x5a));
        assert_eq!(t.divider_focus, Color::Rgb(0x89, 0xb4, 0xfa));
        assert_eq!(t.pane_title, Color::Rgb(0x6c, 0x70, 0x86));
        assert_eq!(t.pane_title_focus, Color::Rgb(0x89, 0xb4, 0xfa));
        assert_eq!(t.text, Color::Rgb(0xcd, 0xd6, 0xf4));
        assert_eq!(t.accent, Theme::default().accent);
    }

    /// A transparent modal stays one config line away: the shipped
    /// default fills the panel, but `surface = "reset"` restores the
    /// pre-polish see-through box.
    #[test]
    fn surface_can_be_made_transparent_again() {
        let t = Theme::from_cfg(&cfg(&[("surface", "reset")]));
        assert_eq!(t.surface, Color::Reset);
    }

    #[test]
    fn surface_and_selection_slots_are_overridable() {
        let t = Theme::from_cfg(&cfg(&[
            ("surface", "#1e1e2e"),
            ("shadow", "#000000"),
            ("selection_bg", "blue"),
            ("selection_fg", "15"),
        ]));
        assert_eq!(t.surface, Color::Rgb(0x1e, 0x1e, 0x2e));
        assert_eq!(t.shadow, Color::Rgb(0, 0, 0));
        assert_eq!(t.selection_bg, Color::Blue);
        assert_eq!(t.selection_fg, Color::Indexed(15));
        // Untouched slots keep their defaults.
        assert_eq!(t.accent, Theme::default().accent);
    }

    /// Relative luminance per WCAG 2.1, for the contrast assertion below.
    fn channel(v: u8) -> f64 {
        let v = f64::from(v) / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else {
            panic!("contrast is only defined for the rgb slots: {c:?}")
        };
        0.0722f64.mul_add(
            channel(b),
            0.2126f64.mul_add(channel(r), 0.7152 * channel(g)),
        )
    }

    fn contrast(a: Color, b: Color) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        let (hi, lo) = if x > y { (x, y) } else { (y, x) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// The palette's accessibility floor, as a test rather than a
    /// comment: every slot that paints text or a rule clears WCAG AA
    /// (4.5:1) against the shipped `surface`.
    ///
    /// This exists because the first cut of the structural roles shipped
    /// a `divider` at 1.7:1 and a `pane_title` at 2.8:1 — recessive to
    /// the point of being absent for anyone without excellent contrast
    /// vision on a well-calibrated display.
    #[test]
    fn contrast_floor_is_met() {
        let t = Theme::default();
        let bg = t.surface;
        for (name, slot) in [
            ("divider", t.divider),
            ("divider_focus", t.divider_focus),
            ("pane_title", t.pane_title),
            ("pane_title_focus", t.pane_title_focus),
            ("border", t.border),
            ("dim", t.dim),
            ("sidebar_section", t.sidebar_section),
            ("agent_idle", t.agent_idle),
            ("agent_working", t.agent_working),
            ("agent_blocked", t.agent_blocked),
            ("agent_done", t.agent_done),
            ("accent", t.accent),
            ("chord", t.chord),
            ("text", t.text),
            ("section_header", t.section_header),
            ("error", t.error),
            ("attention", t.attention),
        ] {
            let ratio = contrast(slot, bg);
            assert!(
                ratio >= 4.5,
                "{name} is {ratio:.2}:1 against surface; the floor is 4.5:1"
            );
        }
    }

    #[test]
    fn selection_text_and_sidebar_context_clear_contrast_floor() {
        let t = Theme::default();
        for fg in [t.selection_fg, t.dim, t.accent, t.attention] {
            assert!(
                contrast(fg, t.selection_bg) >= 4.5,
                "selection contrast: {fg:?}"
            );
        }
    }

    /// The three rungs stay ordered. Focus must read as brighter than
    /// recessive text, which must read as brighter than the rules — a
    /// retune that clears the floor by flattening the hierarchy would
    /// otherwise pass `contrast_floor_is_met` and look wrong.
    #[test]
    fn the_recessive_rungs_stay_ordered() {
        let t = Theme::default();
        let bg = t.surface;
        let rules = contrast(t.divider, bg);
        let recessive = contrast(t.dim, bg);
        let focus = contrast(t.accent, bg);
        assert!(
            rules < recessive,
            "rules ({rules:.2}) must recede behind recessive text ({recessive:.2})"
        );
        assert!(
            recessive < focus,
            "recessive text ({recessive:.2}) must recede behind focus ({focus:.2})"
        );
    }

    #[test]
    fn from_cfg_empty_is_default() {
        let t = Theme::from_cfg(&phux_config::ThemeCfg::default());
        assert_eq!(t, Theme::default());
    }

    #[test]
    fn named_color_override_applies() {
        let t = Theme::from_cfg(&cfg(&[("accent", "magenta")]));
        assert_eq!(t.accent, Color::Magenta);
        // Untouched slots keep their default.
        assert_eq!(t.chord, Theme::default().chord);
    }

    #[test]
    fn hex_color_override_applies() {
        let t = Theme::from_cfg(&cfg(&[("section_header", "#cdd6f4")]));
        assert_eq!(t.section_header, Color::Rgb(0xcd, 0xd6, 0xf4));
    }

    #[test]
    fn indexed_color_override_applies() {
        let t = Theme::from_cfg(&cfg(&[("chord", "12")]));
        assert_eq!(t.chord, Color::Indexed(12));
    }

    #[test]
    fn unknown_slot_is_ignored() {
        let t = Theme::from_cfg(&cfg(&[("not_a_slot", "red")]));
        assert_eq!(t, Theme::default());
    }

    #[test]
    fn unparseable_color_keeps_default() {
        let t = Theme::from_cfg(&cfg(&[("accent", "definitely-not-a-color")]));
        assert_eq!(t.accent, Theme::default().accent);
    }

    #[test]
    fn multiple_overrides_apply_independently() {
        let t = Theme::from_cfg(&cfg(&[
            ("accent", "blue"),
            ("error", "yellow"),
            ("dim", "white"),
        ]));
        assert_eq!(t.accent, Color::Blue);
        assert_eq!(t.error, Color::Yellow);
        assert_eq!(t.dim, Color::White);
        assert_eq!(t.section_header, Theme::default().section_header);
    }
}
