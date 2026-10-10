use super::{ViewId, gpui, parse_view};
use serde_json::Value;

#[derive(Clone)]
pub(super) struct Settings {
    pub client_handle: String,
    pub terminal_id: String,
    pub view_id: Option<ViewId>,
    pub font: gpui::Font,
    pub font_size: f32,
    pub line_height: f32,
    pub foreground: Option<gpui::Hsla>,
    pub background: Option<gpui::Hsla>,
    pub selection_foreground: gpui::Hsla,
    /// The theme's 16 ANSI colours; `None` keeps the terminal's own palette.
    pub palette: Option<[gpui::Hsla; 16]>,
    /// Multipliers on the measured cell, like Ghostty's `adjust-cell-*`.
    pub cell_width_scale: f32,
    pub cell_height_scale: f32,
    pub selection_background: gpui::Hsla,
    pub cursor: Option<gpui::Hsla>,
    pub focused: bool,
    pub cursor_visible: bool,
    pub blink_visible: bool,
    pub option_as_alt: bool,
    /// This view proposes the terminal's PTY size from its painted bounds.
    pub size_owner: bool,
    /// Non-Command chords the shell binds (e.g. `ctrl+tab`), in the shell's
    /// `ctrl+alt+shift+key` form. They reach the window instead of the PTY.
    pub app_chords: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            client_handle: String::new(),
            terminal_id: String::new(),
            view_id: None,
            font: gpui::font("Paper Mono"),
            font_size: 14.,
            line_height: 1.25,
            foreground: None,
            background: None,
            selection_foreground: gpui::rgb(0xffffff).into(),
            palette: None,
            cell_width_scale: 1.,
            cell_height_scale: 1.,
            selection_background: gpui::rgb(0x385779).into(),
            cursor: None,
            focused: true,
            cursor_visible: true,
            blink_visible: true,
            option_as_alt: false,
            size_owner: true,
            app_chords: Vec::new(),
        }
    }
}

impl Settings {
    /// Whether a frame paints identically under both. Identity props are
    /// checked through the frame itself; input, sizing and the
    /// `paintRevision` token never reach the painter.
    pub fn paints_like(&self, other: &Self) -> bool {
        self.font == other.font
            && self.font_size == other.font_size
            && self.line_height == other.line_height
            && self.cell_width_scale == other.cell_width_scale
            && self.cell_height_scale == other.cell_height_scale
            && self.foreground == other.foreground
            && self.background == other.background
            && self.selection_foreground == other.selection_foreground
            && self.selection_background == other.selection_background
            && self.palette == other.palette
            && self.cursor == other.cursor
            && self.focused == other.focused
            && self.cursor_visible == other.cursor_visible
            && self.blink_visible == other.blink_visible
    }

    pub fn set(&mut self, key: &str, value: &Value) {
        match key {
            "clientHandle" => self.client_handle = value.as_str().unwrap_or_default().into(),
            "terminalId" => self.terminal_id = value.as_str().unwrap_or_default().into(),
            "viewId" => self.view_id = value.as_str().and_then(parse_view),
            "font" => self.set_font(value),
            "theme" => self.set_theme(value),
            "optionAsAlt" => self.option_as_alt = value.as_bool().unwrap_or(false),
            "appChords" => {
                self.app_chords = value
                    .as_array()
                    .map(|chords| {
                        chords
                            .iter()
                            .filter_map(|chord| chord.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
            }
            _ => self.set_visibility(key, value),
        }
    }

    fn set_visibility(&mut self, key: &str, value: &Value) {
        let visible = value.as_bool().unwrap_or(true);
        match key {
            "focused" => self.focused = visible,
            "cursorVisible" => self.cursor_visible = visible,
            "blinkVisible" => self.blink_visible = visible,
            "sizeOwner" => self.size_owner = visible,
            // An invalidation token, deliberately not a publication generation.
            "paintRevision" => (),
            _ => (),
        }
    }

    fn set_font(&mut self, value: &Value) {
        let defaults = Self::default();
        self.font = gpui::font(value["family"].as_str().unwrap_or("Paper Mono").to_owned());
        self.font_size = number(&value["size"], 6., 96.).unwrap_or(defaults.font_size);
        self.line_height = number(&value["lineHeight"], 1., 3.).unwrap_or(defaults.line_height);
        self.cell_width_scale = number(&value["cellWidth"], 0.5, 2.).unwrap_or(1.);
        self.cell_height_scale = number(&value["cellHeight"], 0.5, 2.).unwrap_or(1.);
    }

    fn set_theme(&mut self, value: &Value) {
        let defaults = Self::default();
        self.foreground = color(&value["foreground"]);
        self.background = color(&value["background"]);
        self.cursor = color(&value["cursor"]);
        self.selection_foreground =
            color(&value["selectionForeground"]).unwrap_or(defaults.selection_foreground);
        self.selection_background =
            color(&value["selectionBackground"]).unwrap_or(defaults.selection_background);
        self.palette = palette(&value["palette"]);
    }
}

/// Exactly 16 valid colours, or no theme palette at all.
fn palette(value: &Value) -> Option<[gpui::Hsla; 16]> {
    let entries = value.as_array()?;
    if entries.len() != 16 {
        return None;
    }
    let mut colors = [gpui::Hsla::default(); 16];
    for (slot, entry) in colors.iter_mut().zip(entries) {
        *slot = color(entry)?;
    }
    Some(colors)
}

fn number(value: &Value, minimum: f32, maximum: f32) -> Option<f32> {
    let number = value.as_f64()? as f32;
    (minimum..=maximum).contains(&number).then_some(number)
}

fn color(value: &Value) -> Option<gpui::Hsla> {
    let hex = value.as_str()?.strip_prefix('#')?;
    if hex.len() != 6 && hex.len() != 8 {
        return None;
    }
    let rgb = u32::from_str_radix(&hex[..6], 16).ok()?;
    let alpha = if hex.len() == 8 {
        f32::from(u8::from_str_radix(&hex[6..], 16).ok()?) / 255.
    } else {
        1.
    };
    Some(gpui::rgb(rgb).opacity(alpha))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn invalid_identity_replaces_old_view_instead_of_reusing_it() {
        let mut settings = Settings::default();
        settings.set("viewId", &json!("9007199254740993"));
        assert_eq!(
            settings.view_id.expect("lossless view").get(),
            9_007_199_254_740_993
        );
        settings.set("viewId", &json!(9007199254740993_u64));
        assert_eq!(settings.view_id, None);
        settings.set("viewId", &json!("0"));
        assert_eq!(settings.view_id, None);
    }

    #[test]
    fn font_and_theme_reset_are_total_and_bounded() {
        let mut settings = Settings::default();
        settings.set("font", &json!({"size": -1, "lineHeight": 1000000}));
        assert_eq!(settings.font_size, 14.);
        assert_eq!(settings.line_height, 1.25);
        settings.set(
            "theme",
            &json!({"foreground": "#123456", "background": "#123456cc"}),
        );
        assert!(settings.foreground.is_some());
        assert!(settings.background.expect("alpha background").a < 1.);
        settings.set("theme", &Value::Null);
        assert!(settings.foreground.is_none());
        let sixteen: Vec<_> = (0..16).map(|index| format!("#0000{index:02x}")).collect();
        settings.set("theme", &json!({ "palette": sixteen }));
        assert!(settings.palette.is_some());
        settings.set("theme", &json!({ "palette": ["#000000"] }));
        assert!(settings.palette.is_none());
        settings.set("font", &json!({"cellWidth": 0.92, "cellHeight": 9}));
        assert_eq!(settings.cell_width_scale, 0.92);
        assert_eq!(settings.cell_height_scale, 1.);
    }
}
