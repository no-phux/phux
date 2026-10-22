//! The `[theme]` slot vocabulary: every chrome colour slot the renderer
//! reads, and the colour syntax it accepts.
//!
//! `[theme]` is free-form in the schema (a `slot -> color` map), so the
//! vocabulary lives here: the settings page lists [`SLOT_SPECS`], `phux
//! config check` flags names outside it and colours [`is_color`] rejects, and
//! phux-tui's `Theme` is pinned to both by agreement tests.

use crate::settings::{Applies, SettingKind, SettingSection, SettingSpec};

/// Whether `name` is a `[theme]` colour slot (not a reserved selector key).
#[must_use]
pub fn is_slot(name: &str) -> bool {
    SLOT_SPECS.iter().any(|spec| spec.leaf() == name)
}

/// Every slot name, in [`SLOT_SPECS`] order (for did-you-mean suggestions).
#[must_use]
pub fn slot_names() -> Vec<&'static str> {
    SLOT_SPECS
        .iter()
        .map(|spec| spec.key.strip_prefix("theme.").unwrap_or(spec.key))
        .collect()
}

/// Whether the renderer can parse `spec` as a slot colour.
///
/// Accepts a named colour (`cyan`, `light-red`, `bright_blue`, `grey`,
/// `reset`, ...), an indexed colour (`0` to `255`), or `#rrggbb`. Mirrors
/// ratatui's `Color::from_str`; phux-tui pins the agreement with a test.
#[must_use]
pub fn is_color(spec: &str) -> bool {
    is_named_color(spec) || spec.parse::<u8>().is_ok() || is_hex_color(spec)
}

/// The same normalization and name table as ratatui's `Color::from_str`.
fn is_named_color(spec: &str) -> bool {
    let normalized = spec
        .to_lowercase()
        .replace([' ', '-', '_'], "")
        .replace("bright", "light")
        .replace("grey", "gray")
        .replace("silver", "gray")
        .replace("lightblack", "darkgray")
        .replace("lightwhite", "white")
        .replace("lightgray", "white");
    matches!(
        normalized.as_str(),
        "reset"
            | "black"
            | "red"
            | "green"
            | "yellow"
            | "blue"
            | "magenta"
            | "cyan"
            | "gray"
            | "darkgray"
            | "lightred"
            | "lightgreen"
            | "lightyellow"
            | "lightblue"
            | "lightmagenta"
            | "lightcyan"
            | "white"
    )
}

/// `#rrggbb`, parsed the way ratatui parses it.
fn is_hex_color(spec: &str) -> bool {
    if !spec.starts_with('#') || spec.len() != 7 {
        return false;
    }
    [1..3, 3..5, 5..7].into_iter().all(|range| {
        spec.get(range)
            .is_some_and(|byte| u8::from_str_radix(byte, 16).is_ok())
    })
}

/// The theme slots as settings-page rows (ADR-0101), one per colour slot the
/// renderer reads. Every slot reloads live.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_names_are_the_spec_leaves() {
        let names = slot_names();
        assert_eq!(names.len(), SLOT_SPECS.len());
        for (name, spec) in names.iter().zip(SLOT_SPECS) {
            assert_eq!(spec.table(), "theme", "{}", spec.key);
            assert_eq!(*name, spec.leaf());
            assert!(is_slot(name));
        }
        for reserved in crate::theme::RESERVED_KEYS {
            assert!(!is_slot(reserved), "{reserved} is a selector, not a slot");
        }
        assert!(!is_slot("acent"));
    }

    #[test]
    fn color_syntax() {
        for good in [
            "cyan",
            "Light-Red",
            "bright_blue",
            "grey",
            "reset",
            "12",
            "#1E66f5",
        ] {
            assert!(is_color(good), "{good}");
        }
        for bad in [
            "",
            "#12345",
            "#1234567",
            "#gggggg",
            "256",
            "-1",
            "teal",
            "rgb(1,2,3)",
        ] {
            assert!(!is_color(bad), "{bad}");
        }
    }
}
