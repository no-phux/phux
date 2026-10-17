//! Paint and input settings for one terminal view.
//!
//! Identity is not a setting. The view, terminal and client are arguments to
//! [`super::Terminal::new`]. Font size is 6–96, line height 1–3, and cell
//! scales 0.5–2; [`Settings::sanitize`] puts anything else back on the default.

use gpui::{Font, Hsla, font, rgb};

#[derive(Clone, Debug)]
pub struct Settings {
    pub font: Font,
    pub font_size: f32,
    pub line_height: f32,
    pub foreground: Option<Hsla>,
    pub background: Option<Hsla>,
    /// The host already paints [`Self::background`] behind the whole pane, so
    /// the painter leaves default-background cells unfilled. A different
    /// default (reverse video, OSC 11) is still filled.
    pub host_fills_background: bool,
    pub selection_foreground: Hsla,
    /// The theme's 16 ANSI colours; `None` keeps the terminal's own palette.
    pub palette: Option<[Hsla; 16]>,
    /// Multipliers on the measured cell, like Ghostty's `adjust-cell-*`.
    pub cell_width_scale: f32,
    pub cell_height_scale: f32,
    pub selection_background: Hsla,
    pub cursor: Option<Hsla>,
    pub focused: bool,
    pub cursor_visible: bool,
    pub blink_visible: bool,
    /// A jumping cursor streaks a beam behind it ([`super::laser`]).
    pub cursor_laser: bool,
    pub option_as_alt: bool,
    /// This view proposes the terminal's PTY size from its painted bounds.
    pub size_owner: bool,
    /// Non-Command chords the shell binds (e.g. `ctrl+tab`), in
    /// `ctrl+alt+shift+key` form. They reach the window instead of the PTY.
    pub app_chords: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font: font("Paper Mono"),
            font_size: 14.,
            line_height: 1.25,
            foreground: None,
            background: None,
            host_fills_background: false,
            selection_foreground: rgb(0xffffff).into(),
            palette: None,
            cell_width_scale: 1.,
            cell_height_scale: 1.,
            selection_background: rgb(0x385779).into(),
            cursor: None,
            focused: true,
            cursor_visible: true,
            blink_visible: true,
            cursor_laser: false,
            option_as_alt: false,
            size_owner: true,
            app_chords: Vec::new(),
        }
    }
}

impl Settings {
    /// Whether a frame paints identically under both. Input policy, sizing
    /// ownership and app chords never reach the painter.
    pub fn paints_like(&self, other: &Self) -> bool {
        self.font == other.font
            && self.font_size == other.font_size
            && self.line_height == other.line_height
            && self.cell_width_scale == other.cell_width_scale
            && self.cell_height_scale == other.cell_height_scale
            && self.foreground == other.foreground
            && self.background == other.background
            && self.host_fills_background == other.host_fills_background
            && self.selection_foreground == other.selection_foreground
            && self.selection_background == other.selection_background
            && self.palette == other.palette
            && self.cursor == other.cursor
            && self.focused == other.focused
            && self.cursor_visible == other.cursor_visible
            && self.blink_visible == other.blink_visible
    }

    /// Paint settings, overlays and the input policy are the same, so a
    /// configure can return without notifying the view.
    #[must_use]
    pub fn unchanged(&self, other: &Self) -> bool {
        self.paints_like(other)
            && self.cursor_laser == other.cursor_laser
            && self.option_as_alt == other.option_as_alt
            && self.size_owner == other.size_owner
            && self.app_chords == other.app_chords
    }

    /// Clamp metrics the painter cannot use. Non-finite values become the default.
    pub fn sanitize(&mut self) {
        let defaults = Self::default();
        self.font_size = bounded(self.font_size, 6., 96., defaults.font_size);
        self.line_height = bounded(self.line_height, 1., 3., defaults.line_height);
        self.cell_width_scale = bounded(self.cell_width_scale, 0.5, 2., 1.);
        self.cell_height_scale = bounded(self.cell_height_scale, 0.5, 2., 1.);
    }
}

fn bounded(value: f32, minimum: f32, maximum: f32, fallback: f32) -> f32 {
    if value.is_finite() && (minimum..=maximum).contains(&value) {
        value
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_range_metrics_return_to_the_defaults() {
        let mut settings = Settings {
            font_size: -1.,
            line_height: 1_000_000.,
            cell_width_scale: 0.92,
            cell_height_scale: f32::NAN,
            ..Settings::default()
        };
        settings.sanitize();
        assert_eq!(settings.font_size, 14.);
        assert_eq!(settings.line_height, 1.25);
        assert_eq!(settings.cell_width_scale, 0.92);
        assert_eq!(settings.cell_height_scale, 1.);
    }

    #[test]
    fn paint_equality_ignores_input_and_sizing_policy() {
        let settings = Settings::default();
        let mut other = settings.clone();
        other.size_owner = false;
        other.option_as_alt = true;
        other.app_chords = vec!["ctrl+tab".into()];
        assert!(settings.paints_like(&other));
        other.focused = false;
        assert!(!settings.paints_like(&other));
    }

    #[test]
    fn unchanged_covers_paint_and_input_policy() {
        let settings = Settings::default();
        let mut other = settings.clone();
        assert!(settings.unchanged(&other));
        other.app_chords.push("ctrl+tab".into());
        assert!(!settings.unchanged(&other));
        other.app_chords.clear();
        other.cursor_laser = true;
        assert!(
            settings.paints_like(&other),
            "the beam never enters the scene"
        );
        assert!(!settings.unchanged(&other));
        other.cursor_laser = false;
        other.focused = false;
        assert!(!settings.unchanged(&other));
    }
}
