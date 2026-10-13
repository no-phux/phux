//! Persisted display preferences. Field names here are snake_case; the JSON
//! object uses the existing camelCase keys.

use super::theme::{default_theme_id, themes};
use serde_json::{Map, Value};

/// What the window paints and which chords it keeps.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayPrefs {
    /// Font family. Empty sanitizes to Paper Mono.
    pub font_family: String,
    /// Font size in points.
    pub font_size: f64,
    /// Line height multiplier.
    pub line_height: f64,
    /// Whether Option sends Alt.
    pub option_as_alt: bool,
    /// Theme id, or `"ghostty"`.
    pub theme_id: String,
    /// Sidebar visibility.
    pub sidebar_visible: bool,
    /// Sidebar width in points.
    pub sidebar_width: f64,
    /// Agent notifications.
    pub notifications: bool,
    /// Cell width multiplier.
    pub cell_width: f64,
    /// Cell height multiplier.
    pub cell_height: f64,
    /// Horizontal padding in points.
    pub padding_x: f64,
    /// Vertical padding in points.
    pub padding_y: f64,
    /// Opacity of unfocused panes. `1` is no dimming.
    pub unfocused_opacity: f64,
    /// Terminal background opacity.
    pub terminal_opacity: f64,
    /// Whether Ghostty keybinds override the built-in chords.
    pub ghostty_keys: bool,
}

/// Shipped defaults.
#[must_use]
pub fn default_display() -> DisplayPrefs {
    DisplayPrefs {
        font_family: "Paper Mono".to_owned(),
        font_size: 13.0,
        line_height: 1.3,
        option_as_alt: false,
        theme_id: default_theme_id().to_owned(),
        sidebar_visible: true,
        sidebar_width: 248.0,
        notifications: true,
        cell_width: 1.0,
        cell_height: 1.0,
        padding_x: 8.0,
        padding_y: 6.0,
        unfocused_opacity: 1.0,
        terminal_opacity: 0.92,
        ghostty_keys: true,
    }
}

impl Default for DisplayPrefs {
    fn default() -> Self {
        default_display()
    }
}

/// Faces the settings page offers, besides the Ghostty face and the current one.
pub const FONT_FAMILIES: &[&str] = &[
    "Paper Mono",
    "Menlo",
    "SF Mono",
    "Monaco",
    "JetBrains Mono",
    "Fira Code",
    "Berkeley Mono",
    "Iosevka",
];

/// Narrowest sidebar, in points.
pub const SIDEBAR_MIN: f64 = 180.0;
/// Widest sidebar, in points.
pub const SIDEBAR_MAX: f64 = 440.0;

/// Clamp every field the way the TypeScript snapshot does.
#[must_use]
pub fn sanitize_display(display: &DisplayPrefs) -> DisplayPrefs {
    let fallback = default_display();
    let family = display.font_family.trim();
    let family = family.chars().take(64).collect::<String>();
    DisplayPrefs {
        font_family: if family.is_empty() {
            fallback.font_family
        } else {
            family
        },
        font_size: clamp(
            finite(display.font_size, fallback.font_size).round(),
            9.0,
            28.0,
        ),
        line_height: clamp(
            (finite(display.line_height, fallback.line_height) * 20.0).round() / 20.0,
            1.0,
            2.0,
        ),
        option_as_alt: display.option_as_alt,
        theme_id: if display.theme_id == "ghostty"
            || themes().iter().any(|theme| theme.id == display.theme_id)
        {
            display.theme_id.clone()
        } else {
            fallback.theme_id
        },
        sidebar_visible: display.sidebar_visible,
        sidebar_width: clamp(
            finite(display.sidebar_width, fallback.sidebar_width).round(),
            SIDEBAR_MIN,
            SIDEBAR_MAX,
        ),
        notifications: display.notifications,
        cell_width: clamp(round_to(finite(display.cell_width, 1.0), 100.0), 0.5, 2.0),
        cell_height: clamp(round_to(finite(display.cell_height, 1.0), 100.0), 0.5, 2.0),
        padding_x: clamp(
            finite(display.padding_x, fallback.padding_x).round(),
            0.0,
            64.0,
        ),
        padding_y: clamp(
            finite(display.padding_y, fallback.padding_y).round(),
            0.0,
            64.0,
        ),
        unfocused_opacity: clamp(
            round_to(finite(display.unfocused_opacity, 1.0), 100.0),
            0.15,
            1.0,
        ),
        terminal_opacity: clamp(
            round_to(finite(display.terminal_opacity, 1.0), 100.0),
            0.55,
            1.0,
        ),
        ghostty_keys: display.ghostty_keys,
    }
}

/// Read a `display` object. A non-object becomes the defaults.
#[must_use]
pub fn parse_display(value: &Value) -> DisplayPrefs {
    let Some(object) = value.as_object() else {
        return default_display();
    };
    let fallback = default_display();
    sanitize_display(&DisplayPrefs {
        font_family: string(object, "fontFamily").unwrap_or(fallback.font_family),
        font_size: number(object, "fontSize").unwrap_or(fallback.font_size),
        line_height: number(object, "lineHeight").unwrap_or(fallback.line_height),
        option_as_alt: object.get("optionAsAlt") == Some(&Value::Bool(true)),
        theme_id: string(object, "themeId").unwrap_or(fallback.theme_id),
        sidebar_visible: object.get("sidebarVisible") != Some(&Value::Bool(false)),
        sidebar_width: number(object, "sidebarWidth").unwrap_or(fallback.sidebar_width),
        notifications: object.get("notifications") != Some(&Value::Bool(false)),
        cell_width: number(object, "cellWidth").unwrap_or(fallback.cell_width),
        cell_height: number(object, "cellHeight").unwrap_or(fallback.cell_height),
        padding_x: number(object, "paddingX").unwrap_or(fallback.padding_x),
        padding_y: number(object, "paddingY").unwrap_or(fallback.padding_y),
        unfocused_opacity: number(object, "unfocusedOpacity").unwrap_or(fallback.unfocused_opacity),
        terminal_opacity: number(object, "terminalOpacity").unwrap_or(fallback.terminal_opacity),
        ghostty_keys: object.get("ghosttyKeys") != Some(&Value::Bool(false)),
    })
}

/// CamelCase JSON object matching the existing snapshot.
#[must_use]
pub fn display_value(display: &DisplayPrefs) -> Value {
    let display = sanitize_display(display);
    let mut object = Map::new();
    object.insert("fontFamily".to_owned(), Value::String(display.font_family));
    object.insert("fontSize".to_owned(), num(display.font_size));
    object.insert("lineHeight".to_owned(), num(display.line_height));
    object.insert("optionAsAlt".to_owned(), Value::Bool(display.option_as_alt));
    object.insert("themeId".to_owned(), Value::String(display.theme_id));
    object.insert(
        "sidebarVisible".to_owned(),
        Value::Bool(display.sidebar_visible),
    );
    object.insert("sidebarWidth".to_owned(), num(display.sidebar_width));
    object.insert(
        "notifications".to_owned(),
        Value::Bool(display.notifications),
    );
    object.insert("cellWidth".to_owned(), num(display.cell_width));
    object.insert("cellHeight".to_owned(), num(display.cell_height));
    object.insert("paddingX".to_owned(), num(display.padding_x));
    object.insert("paddingY".to_owned(), num(display.padding_y));
    object.insert(
        "unfocusedOpacity".to_owned(),
        num(display.unfocused_opacity),
    );
    object.insert("terminalOpacity".to_owned(), num(display.terminal_opacity));
    object.insert("ghosttyKeys".to_owned(), Value::Bool(display.ghostty_keys));
    Value::Object(object)
}

fn num(value: f64) -> Value {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::from(0))
}

fn string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn number(object: &Map<String, Value>, key: &str) -> Option<f64> {
    object.get(key).and_then(Value::as_f64)
}

fn finite(value: f64, fallback: f64) -> f64 {
    if value.is_finite() { value } else { fallback }
}

fn clamp(value: f64, low: f64, high: f64) -> f64 {
    value.clamp(low, high)
}

fn round_to(value: f64, steps: f64) -> f64 {
    (value * steps).round() / steps
}
