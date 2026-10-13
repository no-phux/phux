//! Display preferences, Ghostty configuration, themes, and key chords.
//!
//! Saved display preferences win after the first launch. A Ghostty config
//! seeds preferences only when nothing has been saved, and its keybinds layer
//! over the built-in chords only while `ghostty_keys` is on.

pub mod display;
pub mod ghostty;
pub mod keymap;
pub mod theme;

pub use display::{
    FONT_FAMILIES, SIDEBAR_MAX, SIDEBAR_MIN, DisplayPrefs, default_display, display_value,
    parse_display, sanitize_display,
};
pub use ghostty::{
    GhosttyConfig, ghostty_action, ghostty_chord, ghostty_prefs, ghostty_theme, parse_ghostty,
    read_ghostty_config, zig_string,
};
pub use keymap::{
    BoundAction, CommandSpec, KeyChord, app_chords, builtin_commands, chord_of, classify_command,
    default_chords, display_chord, effective_keymap, key_chord, plain_key, quick_terminal_chords,
    register_global_hotkeys, shown_chords, text_field_has_keys, yields_to_text_field,
};
pub use theme::{
    Palette, Theme, agent_colors, default_theme_id, mix, palette, readable_on, theme_by_id,
    themes, with_alpha,
};
