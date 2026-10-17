//! Persisted display preferences. Field names here are snake_case; the JSON
//! object uses the existing camelCase keys.

use super::skin::skin_by_id;
use super::theme::{default_theme_id, themes};
use serde_json::{Map, Value};

/// Placement of the workspace tabs, independent of navigator visibility.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TabPosition {
    /// Compact horizontal tabs in the titlebar.
    #[default]
    Top,
    /// Scrollable vertical tabs beside the workspace.
    Side,
}

impl TabPosition {
    /// Stable JSON value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Side => "side",
        }
    }
}

/// What sits behind the window's chrome and panes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum WindowBackground {
    /// Translucent, over a live blur of the desktop. The compositor re-blurs
    /// and blends the window on every presented frame.
    #[default]
    Blur,
    /// Opaque theme colors with no blur. The compositor can treat the window
    /// as an opaque layer and skip blending it with what is behind.
    Solid,
}

impl WindowBackground {
    /// Stable JSON value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blur => "blur",
            Self::Solid => "solid",
        }
    }

    /// Parse the JSON value. Anything unrecognized is the default.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value {
            Some("solid") => Self::Solid,
            _ => Self::Blur,
        }
    }
}

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
    /// Workspace tab placement.
    pub tab_position: TabPosition,
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
    /// Terminal background opacity over the blur. A solid window paints
    /// opaque regardless.
    pub terminal_opacity: f64,
    /// Blurred translucent window or solid opaque one.
    pub window_background: WindowBackground,
    /// Whether Ghostty keybinds override the built-in chords.
    pub ghostty_keys: bool,
    /// Whether a jumping cursor streaks a laser beam behind it.
    pub cursor_laser: bool,
    /// Skin id. Unknown sanitizes to `"glass"`.
    pub skin_id: String,
    /// Plain terminal: wear the terminal skin whatever `skin_id` says.
    pub plain: bool,
    /// Skin motion: pane folds, skin shifts, cursor effects. macOS reduce
    /// motion also turns it off.
    pub animations: bool,
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
        tab_position: TabPosition::Top,
        notifications: true,
        cell_width: 1.0,
        cell_height: 1.0,
        padding_x: 8.0,
        padding_y: 6.0,
        unfocused_opacity: 1.0,
        terminal_opacity: 0.92,
        window_background: WindowBackground::Blur,
        ghostty_keys: true,
        cursor_laser: true,
        skin_id: "glass".to_owned(),
        plain: false,
        animations: true,
    }
}

impl DisplayPrefs {
    /// Terminal background opacity as painted: opaque on a solid window.
    #[must_use]
    pub fn painted_terminal_opacity(&self) -> f64 {
        match self.window_background {
            WindowBackground::Blur => self.terminal_opacity,
            WindowBackground::Solid => 1.0,
        }
    }
}

impl Default for DisplayPrefs {
    fn default() -> Self {
        default_display()
    }
}

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
        tab_position: display.tab_position,
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
        window_background: display.window_background,
        ghostty_keys: display.ghostty_keys,
        cursor_laser: display.cursor_laser,
        skin_id: skin_by_id(&display.skin_id).id.to_owned(),
        plain: display.plain,
        animations: display.animations,
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
        tab_position: match object.get("tabPosition").and_then(Value::as_str) {
            Some("side") => TabPosition::Side,
            _ => TabPosition::Top,
        },
        notifications: object.get("notifications") != Some(&Value::Bool(false)),
        cell_width: number(object, "cellWidth").unwrap_or(fallback.cell_width),
        cell_height: number(object, "cellHeight").unwrap_or(fallback.cell_height),
        padding_x: number(object, "paddingX").unwrap_or(fallback.padding_x),
        padding_y: number(object, "paddingY").unwrap_or(fallback.padding_y),
        unfocused_opacity: number(object, "unfocusedOpacity").unwrap_or(fallback.unfocused_opacity),
        terminal_opacity: number(object, "terminalOpacity").unwrap_or(fallback.terminal_opacity),
        window_background: WindowBackground::parse(
            object.get("windowBackground").and_then(Value::as_str),
        ),
        ghostty_keys: object.get("ghosttyKeys") != Some(&Value::Bool(false)),
        cursor_laser: object.get("cursorLaser") != Some(&Value::Bool(false)),
        skin_id: string(object, "skinId").unwrap_or(fallback.skin_id),
        plain: object.get("plainTerminal") == Some(&Value::Bool(true)),
        animations: object.get("animations") != Some(&Value::Bool(false)),
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
        "tabPosition".to_owned(),
        Value::String(display.tab_position.as_str().to_owned()),
    );
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
    object.insert(
        "windowBackground".to_owned(),
        Value::String(display.window_background.as_str().to_owned()),
    );
    object.insert("ghosttyKeys".to_owned(), Value::Bool(display.ghostty_keys));
    object.insert("cursorLaser".to_owned(), Value::Bool(display.cursor_laser));
    object.insert("skinId".to_owned(), Value::String(display.skin_id));
    object.insert("plainTerminal".to_owned(), Value::Bool(display.plain));
    object.insert("animations".to_owned(), Value::Bool(display.animations));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tab_placement_round_trips_without_changing_other_preferences() {
        for position in [TabPosition::Top, TabPosition::Side] {
            let prefs = DisplayPrefs {
                tab_position: position,
                sidebar_visible: false,
                sidebar_width: 320.,
                font_size: 17.,
                ..default_display()
            };
            let value = display_value(&prefs);
            assert_eq!(value["tabPosition"], position.as_str());
            assert_eq!(parse_display(&value), prefs);
        }
    }

    #[test]
    fn window_background_round_trips_and_defaults_to_blur() {
        for background in [WindowBackground::Blur, WindowBackground::Solid] {
            let prefs = DisplayPrefs {
                window_background: background,
                terminal_opacity: 0.8,
                ..default_display()
            };
            let value = display_value(&prefs);
            assert_eq!(value["windowBackground"], background.as_str());
            assert_eq!(parse_display(&value), prefs);
        }
        for value in [
            None,
            Some(json!("glass")),
            Some(json!(1)),
            Some(json!(null)),
        ] {
            let mut display = json!({"terminalOpacity": 0.7});
            if let Some(value) = value {
                display["windowBackground"] = value;
            }
            let parsed = parse_display(&display);
            assert_eq!(parsed.window_background, WindowBackground::Blur);
            assert_eq!(parsed.terminal_opacity, 0.7);
        }
    }

    #[test]
    fn a_solid_window_paints_the_terminal_opaque_and_keeps_the_saved_opacity() {
        let blur = DisplayPrefs {
            terminal_opacity: 0.7,
            ..default_display()
        };
        assert_eq!(blur.painted_terminal_opacity(), 0.7);
        let solid = DisplayPrefs {
            window_background: WindowBackground::Solid,
            ..blur
        };
        assert_eq!(solid.painted_terminal_opacity(), 1.0);
        assert_eq!(parse_display(&display_value(&solid)).terminal_opacity, 0.7);
    }

    #[test]
    fn old_or_unrecognized_tab_placement_preserves_the_rest_of_the_display() {
        for value in [
            None,
            Some(json!("bottom")),
            Some(json!(null)),
            Some(json!(42)),
        ] {
            let mut display = json!({"sidebarVisible": false, "fontSize": 18});
            if let Some(value) = value {
                display["tabPosition"] = value;
            }
            let parsed = parse_display(&display);
            assert_eq!(parsed.tab_position, TabPosition::Top);
            assert!(!parsed.sidebar_visible);
            assert_eq!(parsed.font_size, 18.);
        }
    }

    #[test]
    fn the_laser_cursor_defaults_on_and_round_trips_off() {
        assert!(parse_display(&json!({})).cursor_laser);
        let off = DisplayPrefs {
            cursor_laser: false,
            ..default_display()
        };
        let value = display_value(&off);
        assert_eq!(value["cursorLaser"], false);
        assert_eq!(parse_display(&value), off);
    }

    #[test]
    fn skin_and_plain_terminal_round_trip_and_unknown_skins_fall_back() {
        let parsed = parse_display(&json!({}));
        assert_eq!(parsed.skin_id, "glass");
        assert!(!parsed.plain);
        assert!(parsed.animations);
        let plain = DisplayPrefs {
            skin_id: "terminal".to_owned(),
            plain: true,
            animations: false,
            ..default_display()
        };
        let value = display_value(&plain);
        assert_eq!(value["skinId"], "terminal");
        assert_eq!(value["plainTerminal"], true);
        assert_eq!(value["animations"], false);
        assert_eq!(parse_display(&value), plain);
        assert_eq!(parse_display(&json!({"skinId": "neon"})).skin_id, "glass");
        assert!(!parse_display(&json!({"plainTerminal": "yes"})).plain);
        assert!(parse_display(&json!({"animations": "no"})).animations);
    }
}
