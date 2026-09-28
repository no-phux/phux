//! Chrome + overlay color theme: every chrome and overlay color resolves
//! through one [`Theme`] of named semantic slots, owned by the attach driver
//! and threaded into the paint path.
//!
//! [`SLOT_SPECS`] documents each slot.
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
//! colors are ignored with a warning.

use std::str::FromStr;

use phux_config::settings::{Applies, SettingKind, SettingSection, SettingSpec};
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
    /// The default theme with `[theme]` overrides layered on. Values parse
    /// like ratatui's `Color` (`"cyan"`, `"#cdd6f4"`, `"12"`); unknown keys
    /// and unparseable values keep the default (warned).
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

/// The theme slots as settings-page rows (ADR-0101); `[theme]` is free-form
/// in the schema, so the vocabulary lives here. A test pins it to
/// `Theme::slot_mut`. Every slot reloads live.
pub const SLOT_SPECS: &[SettingSpec] = &[
    SettingSpec {
        key: "theme.accent",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Modal titles, query caret, active tab",
        detail: "The one hue that says this is phux talking: modal titles, the palette's query caret, the active window tab, and the focused pane's frame.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.chord",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Keybinding chords in help and which-key",
        detail: "The green of the keys you press. Distinct enough from accent to scan a help table by column; agent_working tracks it.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.action",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Action labels on the host background",
        detail: "Labels drawn on the terminal's own background (sidebar, status row). Defaults to reset so the readable body text is never phux's decision.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.dim",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Sub-lines, affordances, inactive tabs",
        detail: "The recessive text register: branch sub-lines, affordances, empty-state placeholders, inactive window tabs. Recessive still clears 4.5:1 against surface.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.border",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Modal borders and the sidebar rule",
        detail: "Rules and modal borders read as structure, never content. A step below dim; divider tracks it so every rule in the chrome is one material.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.title",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Titles that diverge from accent",
        detail: "Alias slot for window and section titles when a theme wants them to differ from accent. Tracks accent by default.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.section_header",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Section headings inside help and pickers",
        detail: "The dim category headers of grouped discovery surfaces, legible as headings without competing with accent.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.error",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Error and alarm text",
        detail: "An explicit red rather than ANSI Red, which maps to wildly different hues across terminal palettes.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.text",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Body copy on a filled surface panel",
        detail: "Modal body text. A panel supplies its own background (surface), so its copy must supply its own foreground or it inverts on a light terminal.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.surface",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Modal interior background",
        detail: "The panel fill that makes an overlay read as a surface floating over the panes. Set reset for a transparent modal.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.shadow",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Modal drop shadow",
        detail: "The one-cell band below and right of a floating modal. Shipped as reset (no shadow). Set a dark colour to opt in.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.selection_fg",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Selected row foreground",
        detail: "Foreground of a selected list row and the copy-mode strip. Tracks text: a selected row is the same text on a different bed.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.selection_bg",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Selected row background",
        detail: "Background of a selected list row and the copy-mode strip.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.attention",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Agent-attention chrome",
        detail: "The asked marker and hint on the status bar and the hot rows of the fleet dashboard; agent_blocked tracks it.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.sidebar_section",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Sidebar zone headers and affordance glyphs",
        detail: "The needs-you, here, and spaces zone headers of the sidebar plus its affordance glyphs. Tracks dim.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.agent_idle",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Sidebar agent row while idle",
        detail: "An agent that is waiting for nothing and doing nothing. Tracks dim.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.agent_working",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Sidebar agent row while working",
        detail: "An agent mid-task. Tracks chord: the green of live progress.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.agent_blocked",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Sidebar agent row while blocked",
        detail: "An agent waiting on you. Tracks attention: a blocked agent and an attention marker are one fact seen from two places.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.agent_done",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Sidebar agent row when done",
        detail: "An agent that finished its task.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.divider",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "Pane rules off the focused frame",
        detail: "The recessive structural tone of the pane grid. Tracks border.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.divider_focus",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "The focused pane's own rules",
        detail: "The focused pane's frame, also bold. Tracks accent.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.pane_title",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "An unfocused pane's rail label",
        detail: "The label inset into an unfocused pane's top rule. Tracks dim.",
        applies: Applies::LiveReload,
    },
    SettingSpec {
        key: "theme.pane_title_focus",
        section: SettingSection::Theme,
        kind: SettingKind::Color,
        summary: "The focused pane's rail label",
        detail: "The focused pane's label, also bold. Tracks accent.",
        applies: Applies::LiveReload,
    },
];

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
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// ADR-0101: the settings page's theme rows are exactly the slots the
    /// renderer reads, no more and no fewer.
    #[test]
    fn slot_specs_name_every_slot() {
        let mut theme = Theme::default();
        let mut seen = std::collections::BTreeSet::new();
        for spec in SLOT_SPECS {
            assert_eq!(spec.table(), "theme", "{}", spec.key);
            assert!(
                theme.slot_mut(spec.leaf()).is_some(),
                "SLOT_SPECS names `{}`, which Theme does not read",
                spec.key
            );
            assert!(seen.insert(spec.leaf()), "duplicate row {}", spec.key);
            assert!(
                !spec.summary.ends_with('.'),
                "{}: summary ends with a period",
                spec.key
            );
        }
        // Every slot is a row: probe the field count through Debug, which
        // lists one `name: Color` pair per field.
        let debug = format!("{:?}", Theme::default());
        let field_count = debug.matches(": ").count();
        assert_eq!(
            field_count,
            SLOT_SPECS.len(),
            "Theme has {field_count} fields but SLOT_SPECS has {} rows",
            SLOT_SPECS.len()
        );
    }

    #[test]
    fn color_to_string_round_trips_through_the_parser() {
        for color in [
            Color::Rgb(0x7a, 0xa2, 0xf7),
            Color::Indexed(12),
            Color::Reset,
            Color::Cyan,
            Color::LightRed,
        ] {
            let text = color_to_string(color);
            assert_eq!(
                Color::from_str(&text).expect("renders a parseable color"),
                color,
                "{text}"
            );
        }
        // The default palette renders as the hex the docs table lists.
        let Color::Rgb(r, g, b) = Theme::default().accent else {
            panic!("the shipped accent is an RGB color");
        };
        assert_eq!(
            Theme::default().slot("accent").map(color_to_string),
            Some(format!("#{r:02x}{g:02x}{b:02x}"))
        );
    }

    fn cfg(pairs: &[(&str, &str)]) -> phux_config::ThemeCfg {
        let mut slots = BTreeMap::new();
        for (k, v) in pairs {
            slots.insert((*k).to_owned(), (*v).to_owned());
        }
        phux_config::ThemeCfg { slots }
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

    /// Every settings-page slot is overridable in every color syntax (an
    /// unknown-slot warning would otherwise silently eat one); other slots
    /// keep their defaults, and unknown or unparseable entries change nothing.
    #[test]
    fn every_slot_is_overridable_and_bad_entries_are_ignored() {
        for spec in SLOT_SPECS {
            let t = Theme::from_cfg(&cfg(&[(spec.leaf(), "#123456")]));
            assert_eq!(
                t.slot(spec.leaf()),
                Some(Color::Rgb(0x12, 0x34, 0x56)),
                "{}",
                spec.key
            );
        }
        let t = Theme::from_cfg(&cfg(&[
            ("accent", "magenta"),
            ("chord", "12"),
            ("surface", "reset"),
        ]));
        assert_eq!(
            (t.accent, t.chord, t.surface),
            (Color::Magenta, Color::Indexed(12), Color::Reset)
        );
        assert_eq!(t.dim, Theme::default().dim);
        for bad in [("not_a_slot", "red"), ("accent", "definitely-not-a-color")] {
            assert_eq!(Theme::from_cfg(&cfg(&[bad])), Theme::default(), "{bad:?}");
        }
        assert_eq!(
            Theme::from_cfg(&phux_config::ThemeCfg::default()),
            Theme::default()
        );
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
}
