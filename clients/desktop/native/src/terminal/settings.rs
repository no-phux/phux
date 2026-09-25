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
    pub selection_background: gpui::Hsla,
    pub cursor: Option<gpui::Hsla>,
    pub focused: bool,
    pub cursor_visible: bool,
    pub blink_visible: bool,
    pub option_as_alt: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            client_handle: String::new(),
            terminal_id: String::new(),
            view_id: None,
            font: gpui::font("Menlo"),
            font_size: 14.,
            line_height: 1.25,
            foreground: None,
            background: None,
            selection_foreground: gpui::rgb(0xffffff).into(),
            selection_background: gpui::rgb(0x385779).into(),
            cursor: None,
            focused: true,
            cursor_visible: true,
            blink_visible: true,
            option_as_alt: false,
        }
    }
}

impl Settings {
    pub fn set(&mut self, key: &str, value: &Value) {
        match key {
            "clientHandle" => self.client_handle = value.as_str().unwrap_or_default().into(),
            "terminalId" => self.terminal_id = value.as_str().unwrap_or_default().into(),
            "viewId" => self.view_id = value.as_str().and_then(parse_view),
            "font" => self.set_font(value),
            "theme" => self.set_theme(value),
            "optionAsAlt" => self.option_as_alt = value.as_bool().unwrap_or(false),
            _ => self.set_visibility(key, value),
        }
    }

    fn set_visibility(&mut self, key: &str, value: &Value) {
        let visible = value.as_bool().unwrap_or(true);
        match key {
            "focused" => self.focused = visible,
            "cursorVisible" => self.cursor_visible = visible,
            "blinkVisible" => self.blink_visible = visible,
            // An invalidation token, deliberately not a publication generation.
            "paintRevision" => (),
            _ => (),
        }
    }

    fn set_font(&mut self, value: &Value) {
        let defaults = Self::default();
        self.font = gpui::font(value["family"].as_str().unwrap_or("Menlo").to_owned());
        self.font_size = number(&value["size"], 6., 96.).unwrap_or(defaults.font_size);
        self.line_height = number(&value["lineHeight"], 1., 3.).unwrap_or(defaults.line_height);
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
    }
}

fn number(value: &Value, minimum: f32, maximum: f32) -> Option<f32> {
    let number = value.as_f64()? as f32;
    (minimum..=maximum).contains(&number).then_some(number)
}

fn color(value: &Value) -> Option<gpui::Hsla> {
    let hex = value.as_str()?.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    Some(gpui::rgb(u32::from_str_radix(hex, 16).ok()?).into())
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
        settings.set("theme", &json!({"foreground": "#123456"}));
        assert!(settings.foreground.is_some());
        settings.set("theme", &Value::Null);
        assert!(settings.foreground.is_none());
    }
}
