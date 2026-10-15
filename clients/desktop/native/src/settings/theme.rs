//! Chrome and terminal colours. A theme is a handful of hex values; washes,
//! borders, and text tiers are derived so a new theme is not forty tokens.

/// One named appearance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    /// Stable id. `"ghostty"` is resolved against the user's config.
    pub id: String,
    /// Human name.
    pub name: String,
    /// `"dark"` or `"light"`.
    pub appearance: String,
    /// Terminal default background and the window base.
    pub background: String,
    /// Terminal default foreground.
    pub foreground: String,
    /// Sidebar, tab strip, and panel surface.
    pub surface: String,
    /// Accent.
    pub accent: String,
    /// Cursor colour.
    pub cursor: String,
    /// Selection fill.
    pub selection: String,
    /// Danger.
    pub danger: String,
    /// Warning.
    pub warning: String,
    /// Success.
    pub success: String,
    /// Sixteen ANSI colours. Absent keeps the terminal's own palette.
    pub palette: Option<Vec<String>>,
    /// Selection foreground, when the theme names one.
    pub selection_foreground: Option<String>,
    /// Split divider. Absent uses the derived border.
    pub divider: Option<String>,
}

/// Every token the chrome paints with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// Source theme.
    pub theme: Theme,
    /// Raised surface.
    pub raised: String,
    /// Hairline border.
    pub border: String,
    /// Hover wash.
    pub hover: String,
    /// Pressed wash.
    pub active: String,
    /// Secondary text.
    pub subtext: String,
    /// Muted text.
    pub muted: String,
    /// Faintest text.
    pub faint: String,
    /// Accent wash.
    pub accent_wash: String,
    /// Text that reads on the accent.
    pub accent_text: String,
    /// Split divider.
    pub divider: String,
    /// Modal backdrop, `#rrggbb` plus an alpha byte.
    pub backdrop: String,
}

/// Built-in themes, in picker order.
#[must_use]
pub fn themes() -> Vec<Theme> {
    builtins().to_vec()
}

fn builtins() -> &'static [Theme] {
    static THEMES: std::sync::LazyLock<Vec<Theme>> = std::sync::LazyLock::new(build_themes);
    &THEMES
}

fn build_themes() -> Vec<Theme> {
    vec![
        theme(
            "midnight",
            "phux Midnight",
            "dark",
            "#0d1017",
            "#d8dee9",
            "#12161f",
            "#7aa2f7",
            "#c0caf5",
            "#28406a",
            "#f7768e",
            "#e0af68",
            "#9ece6a",
            &[
                "#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff",
                "#a9b1d6", "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7",
                "#7dcfff", "#c0caf5",
            ],
        ),
        theme(
            "nord",
            "Nord",
            "dark",
            "#2e3440",
            "#d8dee9",
            "#292e39",
            "#88c0d0",
            "#eceff4",
            "#434c5e",
            "#bf616a",
            "#ebcb8b",
            "#a3be8c",
            &[
                "#3b4252", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#88c0d0",
                "#e5e9f0", "#4c566a", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead",
                "#8fbcbb", "#eceff4",
            ],
        ),
        theme(
            "dracula",
            "Dracula",
            "dark",
            "#282a36",
            "#f8f8f2",
            "#21222c",
            "#bd93f9",
            "#f8f8f2",
            "#44475a",
            "#ff5555",
            "#f1fa8c",
            "#50fa7b",
            &[
                "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd",
                "#f8f8f2", "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df",
                "#a4ffff", "#ffffff",
            ],
        ),
        theme(
            "mocha",
            "Catppuccin Mocha",
            "dark",
            "#1e1e2e",
            "#cdd6f4",
            "#181825",
            "#cba6f7",
            "#f5e0dc",
            "#45475a",
            "#f38ba8",
            "#f9e2af",
            "#a6e3a1",
            &[
                "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5",
                "#bac2de", "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7",
                "#94e2d5", "#a6adc8",
            ],
        ),
        theme(
            "latte",
            "Catppuccin Latte",
            "light",
            "#eff1f5",
            "#4c4f69",
            "#e6e9ef",
            "#8839ef",
            "#dc8a78",
            "#ccd0da",
            "#d20f39",
            "#df8e1d",
            "#40a02b",
            &[
                "#5c5f77", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299",
                "#acb0be", "#6c6f85", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb",
                "#179299", "#bcc0cc",
            ],
        ),
    ]
}

#[allow(
    clippy::too_many_arguments,
    reason = "one positional row per built-in theme table entry"
)]
fn theme(
    id: &str,
    name: &str,
    appearance: &str,
    background: &str,
    foreground: &str,
    surface: &str,
    accent: &str,
    cursor: &str,
    selection: &str,
    danger: &str,
    warning: &str,
    success: &str,
    palette: &[&str],
) -> Theme {
    Theme {
        id: id.to_owned(),
        name: name.to_owned(),
        appearance: appearance.to_owned(),
        background: background.to_owned(),
        foreground: foreground.to_owned(),
        surface: surface.to_owned(),
        accent: accent.to_owned(),
        cursor: cursor.to_owned(),
        selection: selection.to_owned(),
        danger: danger.to_owned(),
        warning: warning.to_owned(),
        success: success.to_owned(),
        palette: Some(palette.iter().map(|color| (*color).to_owned()).collect()),
        selection_foreground: None,
        divider: None,
    }
}

/// Built-in id used when a saved theme is unknown.
pub const fn default_theme_id() -> &'static str {
    "midnight"
}

/// Built-ins plus extra themes (a Ghostty import is searched first).
#[must_use]
pub fn theme_by_id(id: &str, extra: &[Theme]) -> Theme {
    extra
        .iter()
        .chain(builtins().iter())
        .find(|theme| theme.id == id)
        .cloned()
        .unwrap_or_else(|| builtins()[0].clone())
}

/// Expand a theme into every chrome token.
#[must_use]
pub fn palette(theme: &Theme) -> Palette {
    let base = &theme.background;
    let ink = &theme.foreground;
    Palette {
        raised: mix(&theme.surface, ink, 0.06),
        border: mix(base, ink, 0.1),
        hover: mix(&theme.surface, ink, 0.07),
        active: mix(&theme.surface, ink, 0.12),
        subtext: mix(base, ink, 0.78),
        muted: mix(base, ink, 0.5),
        faint: mix(base, ink, 0.3),
        accent_wash: mix(&theme.surface, &theme.accent, 0.2),
        accent_text: readable_on(&theme.accent, base, ink),
        divider: theme.divider.clone().unwrap_or_else(|| mix(base, ink, 0.1)),
        backdrop: format!("{}b8", &base[..7.min(base.len())]),
        theme: theme.clone(),
    }
}

/// Linear mix of two `#rrggbb` colours. Unparseable channels are zero.
#[must_use]
pub fn mix(from: &str, to: &str, amount: f64) -> String {
    let left = rgb(from);
    let right = rgb(to);
    let channel = |index: usize| -> String {
        let value = left[index] as f64 + (right[index] as f64 - left[index] as f64) * amount;
        format!("{:02x}", value.round().clamp(0.0, 255.0) as u8)
    };
    format!("#{}{}{}", channel(0), channel(1), channel(2))
}

/// Append an alpha byte to a `#rrggbb` colour.
#[must_use]
pub fn with_alpha(color: &str, amount: f64) -> String {
    let suffix = format!("{:02x}", (amount.clamp(0.0, 1.0) * 255.0).round() as u8);
    let body = if color.len() >= 7 { &color[..7] } else { color };
    format!("{body}{suffix}")
}

/// Whichever of `dark` or `light` contrasts more with `fill`.
#[must_use]
pub fn readable_on(fill: &str, dark: &str, light: &str) -> String {
    let target = luminance(fill);
    if (target - luminance(dark)).abs() > (target - luminance(light)).abs() {
        dark.to_owned()
    } else {
        light.to_owned()
    }
}

fn luminance(hex: &str) -> f64 {
    let [r, g, b] = rgb(hex);
    let channel = |value: u8| -> f64 {
        let unit = f64::from(value) / 255.0;
        if unit <= 0.039_28 {
            unit / 12.92
        } else {
            ((unit + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

fn rgb(hex: &str) -> [u8; 3] {
    let Some(body) = hex.strip_prefix('#') else {
        return [0, 0, 0];
    };
    if body.len() < 6 || !body.as_bytes()[..6].iter().all(u8::is_ascii_hexdigit) {
        return [0, 0, 0];
    }
    let value = u32::from_str_radix(&body[..6], 16).unwrap_or(0);
    [
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    ]
}

/// Corner radii, in points.
pub const RADIUS_PANEL: f32 = 12.0;
/// Control corner radius.
pub const RADIUS_CONTROL: f32 = 8.0;
/// Small corner radius.
pub const RADIUS_SMALL: f32 = 5.0;
/// Chrome font sizes.
pub const UI_FONT: f32 = 12.5;
/// Small chrome font.
pub const UI_FONT_SMALL: f32 = 11.0;
/// Title chrome font.
pub const UI_FONT_TITLE: f32 = 13.0;

#[cfg(test)]
mod lookup_tests {
    use super::*;

    #[test]
    fn cached_builtins_preserve_override_order_and_return_independent_themes() {
        let builtin = theme_by_id(default_theme_id(), &[]);
        let mut custom = builtin.clone();
        custom.background = "#123456".into();
        assert_eq!(theme_by_id(&custom.id, &[custom.clone()]), custom);
        assert_eq!(theme_by_id(default_theme_id(), &[]), builtin);
        assert_eq!(theme_by_id("missing", &[]), builtin);
        let mut choices = themes();
        choices[0].background = "#abcdef".into();
        assert_eq!(theme_by_id(default_theme_id(), &[]), builtin);
    }
}
