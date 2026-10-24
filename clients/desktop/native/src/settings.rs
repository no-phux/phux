//! Display preferences, Ghostty configuration, themes, and key chords.
//!
//! Saved display preferences win after the first launch. A Ghostty config
//! seeds preferences only when nothing has been saved, and its keybinds layer
//! over the built-in chords only while `ghostty_keys` is on.

pub mod display;
pub mod ghostty;
pub mod keymap;
pub mod leader;
pub mod provenance;
pub mod skin;
pub mod theme;

pub use display::{
    DisplayPrefs, SIDEBAR_MAX, SIDEBAR_MIN, SidebarGrouping, TabPosition, WindowBackground,
    default_display, display_value, parse_display, sanitize_display,
};
pub use ghostty::{
    Bind, Diagnostic, GhosttyConfig, GhosttySource, Origin, ghostty_action, ghostty_chord,
    ghostty_prefs, ghostty_theme, parse_ghostty, parse_ghostty_sources, read_ghostty_sources,
    read_ghostty_sources_in, skipped_lines_notice, zig_string,
};
pub use keymap::{
    BoundAction, ChordSource, CommandSpec, KeyChord, KeyConflict, app_chords, builtin_commands,
    chord_of, chord_source, classify_command, default_chords, display_chord, effective_keymap,
    key_chord, keymap_conflicts, plain_key, quick_terminal_chords, register_global_hotkeys,
    shown_chords, text_field_has_keys, yields_to_text_field,
};
pub use provenance::{Applies, Provenance, display_provenance};
pub use skin::{Backdrop, Shift, Skin, TabsWhen, resolved_id, skin_by_id, skins};
pub use theme::{
    Palette, Theme, default_theme_id, mix, palette, readable_on, theme_by_id, themes, with_alpha,
};
