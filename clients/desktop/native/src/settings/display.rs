//! Persisted display preferences. Field names here are snake_case; the JSON
//! object uses the existing camelCase keys.

use super::skin::skin_by_id;
use super::theme::{default_theme_id, themes};
use serde_json::{Value, json};

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
    let family = display
        .font_family
        .trim()
        .chars()
        .take(64)
        .collect::<String>();
    let hundredths = |value: f64, fallback: f64| round_to(finite(value, fallback), 100.0);
    DisplayPrefs {
        font_family: if family.is_empty() {
            fallback.font_family
        } else {
            family
        },
        font_size: finite(display.font_size, fallback.font_size)
            .round()
            .clamp(9.0, 28.0),
        line_height: round_to(finite(display.line_height, fallback.line_height), 20.0)
            .clamp(1.0, 2.0),
        theme_id: if display.theme_id == "ghostty"
            || themes().iter().any(|theme| theme.id == display.theme_id)
        {
            display.theme_id.clone()
        } else {
            fallback.theme_id
        },
        sidebar_width: finite(display.sidebar_width, fallback.sidebar_width)
            .round()
            .clamp(SIDEBAR_MIN, SIDEBAR_MAX),
        cell_width: hundredths(display.cell_width, 1.0).clamp(0.5, 2.0),
        cell_height: hundredths(display.cell_height, 1.0).clamp(0.5, 2.0),
        padding_x: finite(display.padding_x, fallback.padding_x)
            .round()
            .clamp(0.0, 64.0),
        padding_y: finite(display.padding_y, fallback.padding_y)
            .round()
            .clamp(0.0, 64.0),
        unfocused_opacity: hundredths(display.unfocused_opacity, 1.0).clamp(0.15, 1.0),
        terminal_opacity: hundredths(display.terminal_opacity, 1.0).clamp(0.55, 1.0),
        skin_id: skin_by_id(&display.skin_id).id.to_owned(),
        ..display.clone()
    }
}

/// Read a `display` object. A non-object becomes the defaults.
#[must_use]
pub fn parse_display(value: &Value) -> DisplayPrefs {
    let fallback = default_display();
    let Some(object) = value.as_object() else {
        return fallback;
    };
    let string = |key: &str, fallback: String| {
        object
            .get(key)
            .and_then(Value::as_str)
            .map_or(fallback, str::to_owned)
    };
    let number =
        |key: &str, fallback: f64| object.get(key).and_then(Value::as_f64).unwrap_or(fallback);
    // Only a JSON boolean overrides a default.
    let flag =
        |key: &str, fallback: bool| object.get(key).and_then(Value::as_bool).unwrap_or(fallback);
    sanitize_display(&DisplayPrefs {
        font_family: string("fontFamily", fallback.font_family),
        font_size: number("fontSize", fallback.font_size),
        line_height: number("lineHeight", fallback.line_height),
        option_as_alt: flag("optionAsAlt", fallback.option_as_alt),
        theme_id: string("themeId", fallback.theme_id),
        sidebar_visible: flag("sidebarVisible", fallback.sidebar_visible),
        sidebar_width: number("sidebarWidth", fallback.sidebar_width),
        tab_position: match object.get("tabPosition").and_then(Value::as_str) {
            Some("side") => TabPosition::Side,
            _ => TabPosition::Top,
        },
        notifications: flag("notifications", fallback.notifications),
        cell_width: number("cellWidth", fallback.cell_width),
        cell_height: number("cellHeight", fallback.cell_height),
        padding_x: number("paddingX", fallback.padding_x),
        padding_y: number("paddingY", fallback.padding_y),
        unfocused_opacity: number("unfocusedOpacity", fallback.unfocused_opacity),
        terminal_opacity: number("terminalOpacity", fallback.terminal_opacity),
        window_background: WindowBackground::parse(
            object.get("windowBackground").and_then(Value::as_str),
        ),
        ghostty_keys: flag("ghosttyKeys", fallback.ghostty_keys),
        cursor_laser: flag("cursorLaser", fallback.cursor_laser),
        skin_id: string("skinId", fallback.skin_id),
        plain: flag("plainTerminal", fallback.plain),
        animations: flag("animations", fallback.animations),
    })
}

/// CamelCase JSON object matching the existing snapshot.
#[must_use]
pub fn display_value(display: &DisplayPrefs) -> Value {
    let display = sanitize_display(display);
    json!({
        "fontFamily": display.font_family,
        "fontSize": display.font_size,
        "lineHeight": display.line_height,
        "optionAsAlt": display.option_as_alt,
        "themeId": display.theme_id,
        "sidebarVisible": display.sidebar_visible,
        "sidebarWidth": display.sidebar_width,
        "tabPosition": display.tab_position.as_str(),
        "notifications": display.notifications,
        "cellWidth": display.cell_width,
        "cellHeight": display.cell_height,
        "paddingX": display.padding_x,
        "paddingY": display.padding_y,
        "unfocusedOpacity": display.unfocused_opacity,
        "terminalOpacity": display.terminal_opacity,
        "windowBackground": display.window_background.as_str(),
        "ghosttyKeys": display.ghostty_keys,
        "cursorLaser": display.cursor_laser,
        "skinId": display.skin_id,
        "plainTerminal": display.plain,
        "animations": display.animations,
    })
}

fn finite(value: f64, fallback: f64) -> f64 {
    if value.is_finite() { value } else { fallback }
}

fn round_to(value: f64, steps: f64) -> f64 {
    (value * steps).round() / steps
}

#[cfg(test)]
mod tests {
    use super::*;

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
