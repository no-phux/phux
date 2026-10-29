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

/// One row per theme: id, name, appearance, its colours in [`Theme`] field
/// order, then its ANSI palette.
fn build_themes() -> Vec<Theme> {
    [
        (
            "midnight",
            "phux Midnight",
            "dark",
            [
                "#0d1017", "#d8dee9", "#12161f", "#7aa2f7", "#c0caf5", "#28406a", "#f7768e",
                "#e0af68", "#9ece6a",
            ],
            [
                "#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff",
                "#a9b1d6", "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7",
                "#7dcfff", "#c0caf5",
            ],
        ),
        (
            "nord",
            "Nord",
            "dark",
            [
                "#2e3440", "#d8dee9", "#292e39", "#88c0d0", "#eceff4", "#434c5e", "#bf616a",
                "#ebcb8b", "#a3be8c",
            ],
            [
                "#3b4252", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#88c0d0",
                "#e5e9f0", "#4c566a", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead",
                "#8fbcbb", "#eceff4",
            ],
        ),
        (
            "dracula",
            "Dracula",
            "dark",
            [
                "#282a36", "#f8f8f2", "#21222c", "#bd93f9", "#f8f8f2", "#44475a", "#ff5555",
                "#f1fa8c", "#50fa7b",
            ],
            [
                "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd",
                "#f8f8f2", "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df",
                "#a4ffff", "#ffffff",
            ],
        ),
        (
            "mocha",
            "Catppuccin Mocha",
            "dark",
            [
                "#1e1e2e", "#cdd6f4", "#181825", "#cba6f7", "#f5e0dc", "#45475a", "#f38ba8",
                "#f9e2af", "#a6e3a1",
            ],
            [
                "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5",
                "#bac2de", "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7",
                "#94e2d5", "#a6adc8",
            ],
        ),
        (
            "tokyo-night",
            "Tokyo Night",
            "dark",
            [
                "#1a1b26", "#c0caf5", "#16161e", "#7aa2f7", "#c0caf5", "#283457", "#f7768e",
                "#e0af68", "#9ece6a",
            ],
            [
                "#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff",
                "#a9b1d6", "#414868", "#ff899d", "#9fe044", "#faba4a", "#8db0ff", "#c7a9ff",
                "#a4daff", "#c0caf5",
            ],
        ),
        (
            "frappe",
            "Catppuccin Frappé",
            "dark",
            [
                "#303446", "#c6d0f5", "#292c3c", "#ca9ee6", "#f2d5cf", "#51576d", "#e78284",
                "#e5c890", "#a6d189",
            ],
            [
                "#51576d", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be",
                "#b5bfe2", "#626880", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4",
                "#81c8be", "#a5adce",
            ],
        ),
        (
            "macchiato",
            "Catppuccin Macchiato",
            "dark",
            [
                "#24273a", "#cad3f5", "#1e2030", "#c6a0f6", "#f4dbd6", "#494d64", "#ed8796",
                "#eed49f", "#a6da95",
            ],
            [
                "#494d64", "#ed8796", "#a6da95", "#eed49f", "#8aadf4", "#f5bde6", "#8bd5ca",
                "#b8c0e0", "#5b6078", "#ed8796", "#a6da95", "#eed49f", "#8aadf4", "#f5bde6",
                "#8bd5ca", "#a5adcb",
            ],
        ),
        (
            "tokyo-night-storm",
            "Tokyo Night Storm",
            "dark",
            [
                "#24283b", "#c0caf5", "#1f2335", "#7aa2f7", "#c0caf5", "#2e3c64", "#f7768e",
                "#e0af68", "#9ece6a",
            ],
            [
                "#1d202f", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff",
                "#a9b1d6", "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7",
                "#7dcfff", "#c0caf5",
            ],
        ),
        (
            "gruvbox-dark",
            "Gruvbox Dark",
            "dark",
            [
                "#282828", "#ebdbb2", "#1d2021", "#83a598", "#ebdbb2", "#504945", "#fb4934",
                "#fabd2f", "#b8bb26",
            ],
            [
                "#282828", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a",
                "#a89984", "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b",
                "#8ec07c", "#ebdbb2",
            ],
        ),
        (
            "kanagawa",
            "Kanagawa Wave",
            "dark",
            [
                "#1f1f28", "#dcd7ba", "#16161d", "#7e9cd8", "#c8c093", "#2d4f67", "#e82424",
                "#ff9e3b", "#98bb6c",
            ],
            [
                "#16161d", "#c34043", "#76946a", "#c0a36e", "#7e9cd8", "#957fb8", "#6a9589",
                "#c8c093", "#727169", "#e82424", "#98bb6c", "#e6c384", "#7fb4ca", "#938aa9",
                "#7aa89f", "#dcd7ba",
            ],
        ),
        (
            "everforest-dark",
            "Everforest Dark",
            "dark",
            [
                "#2d353b", "#d3c6aa", "#272e33", "#7fbbb3", "#d3c6aa", "#475258", "#e67e80",
                "#dbbc7f", "#a7c080",
            ],
            [
                "#475258", "#e67e80", "#a7c080", "#dbbc7f", "#7fbbb3", "#d699b6", "#83c092",
                "#d3c6aa", "#7a8478", "#e67e80", "#a7c080", "#dbbc7f", "#7fbbb3", "#d699b6",
                "#83c092", "#d3c6aa",
            ],
        ),
        (
            "rose-pine",
            "Rosé Pine",
            "dark",
            [
                "#191724", "#e0def4", "#1f1d2e", "#c4a7e7", "#e0def4", "#403d52", "#eb6f92",
                "#f6c177", "#9ccfd8",
            ],
            [
                "#26233a", "#eb6f92", "#31748f", "#f6c177", "#9ccfd8", "#c4a7e7", "#ebbcba",
                "#e0def4", "#6e6a86", "#eb6f92", "#31748f", "#f6c177", "#9ccfd8", "#c4a7e7",
                "#ebbcba", "#e0def4",
            ],
        ),
        (
            "rose-pine-moon",
            "Rosé Pine Moon",
            "dark",
            [
                "#232136", "#e0def4", "#2a273f", "#c4a7e7", "#e0def4", "#44415a", "#eb6f92",
                "#f6c177", "#9ccfd8",
            ],
            [
                "#393552", "#eb6f92", "#3e8fb0", "#f6c177", "#9ccfd8", "#c4a7e7", "#ea9a97",
                "#e0def4", "#6e6a86", "#eb6f92", "#3e8fb0", "#f6c177", "#9ccfd8", "#c4a7e7",
                "#ea9a97", "#e0def4",
            ],
        ),
        (
            "one-dark",
            "One Dark",
            "dark",
            [
                "#282c34", "#abb2bf", "#21252b", "#61afef", "#528bff", "#3e4451", "#e06c75",
                "#e5c07b", "#98c379",
            ],
            [
                "#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2",
                "#abb2bf", "#5c6370", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd",
                "#56b6c2", "#ffffff",
            ],
        ),
        (
            "github-dark",
            "GitHub Dark",
            "dark",
            [
                "#0d1117", "#e6edf3", "#161b22", "#2f81f7", "#2f81f7", "#1f3a5f", "#f85149",
                "#d29922", "#3fb950",
            ],
            [
                "#484f58", "#ff7b72", "#3fb950", "#d29922", "#58a6ff", "#bc8cff", "#39c5cf",
                "#b1bac4", "#6e7681", "#ffa198", "#56d364", "#e3b341", "#79c0ff", "#d2a8ff",
                "#56d4dd", "#ffffff",
            ],
        ),
        (
            "solarized-dark",
            "Solarized Dark",
            "dark",
            [
                "#002b36", "#93a1a1", "#00212b", "#268bd2", "#93a1a1", "#274642", "#dc322f",
                "#b58900", "#859900",
            ],
            [
                "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198",
                "#eee8d5", "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4",
                "#93a1a1", "#fdf6e3",
            ],
        ),
        (
            "night-owl",
            "Night Owl",
            "dark",
            [
                "#011627", "#d6deeb", "#01111d", "#82aaff", "#80a4c2", "#1d3b53", "#ef5350",
                "#ffeb95", "#22da6e",
            ],
            [
                "#011627", "#ef5350", "#22da6e", "#addb67", "#82aaff", "#c792ea", "#21c7a8",
                "#ffffff", "#575656", "#ef5350", "#22da6e", "#ffeb95", "#82aaff", "#c792ea",
                "#7fdbca", "#ffffff",
            ],
        ),
        (
            "flexoki-dark",
            "Flexoki Dark",
            "dark",
            [
                "#100f0f", "#cecdc3", "#1c1b1a", "#4385be", "#cecdc3", "#343331", "#d14d41",
                "#d0a215", "#879a39",
            ],
            [
                "#100f0f", "#d14d41", "#879a39", "#d0a215", "#4385be", "#ce5d97", "#3aa99f",
                "#878580", "#575653", "#af3029", "#66800b", "#ad8301", "#205ea6", "#a02f6f",
                "#24837b", "#cecdc3",
            ],
        ),
        (
            "latte",
            "Catppuccin Latte",
            "light",
            [
                "#eff1f5", "#4c4f69", "#e6e9ef", "#8839ef", "#dc8a78", "#ccd0da", "#d20f39",
                "#df8e1d", "#40a02b",
            ],
            [
                "#5c5f77", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299",
                "#acb0be", "#6c6f85", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb",
                "#179299", "#bcc0cc",
            ],
        ),
        (
            "gruvbox-light",
            "Gruvbox Light",
            "light",
            [
                "#fbf1c7", "#3c3836", "#f2e5bc", "#076678", "#3c3836", "#d5c4a1", "#9d0006",
                "#b57614", "#79740e",
            ],
            [
                "#fbf1c7", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a",
                "#7c6f64", "#928374", "#9d0006", "#79740e", "#b57614", "#076678", "#8f3f71",
                "#427b58", "#3c3836",
            ],
        ),
        (
            "rose-pine-dawn",
            "Rosé Pine Dawn",
            "light",
            [
                "#faf4ed", "#575279", "#fffaf3", "#907aa9", "#575279", "#dfdad9", "#b4637a",
                "#ea9d34", "#56949f",
            ],
            [
                "#f2e9e1", "#b4637a", "#286983", "#ea9d34", "#56949f", "#907aa9", "#d7827e",
                "#575279", "#9893a5", "#b4637a", "#286983", "#ea9d34", "#56949f", "#907aa9",
                "#d7827e", "#575279",
            ],
        ),
        (
            "solarized-light",
            "Solarized Light",
            "light",
            [
                "#fdf6e3", "#40555c", "#eee8d5", "#268bd2", "#40555c", "#d9d2c2", "#dc322f",
                "#b58900", "#859900",
            ],
            [
                "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198",
                "#eee8d5", "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4",
                "#93a1a1", "#fdf6e3",
            ],
        ),
        (
            "github-light",
            "GitHub Light",
            "light",
            [
                "#ffffff", "#1f2328", "#f6f8fa", "#0969da", "#0969da", "#b6d7ff", "#cf222e",
                "#9a6700", "#1a7f37",
            ],
            [
                "#24292f", "#cf222e", "#116329", "#4d2d00", "#0969da", "#8250df", "#1b7c83",
                "#6e7781", "#57606a", "#a40e26", "#1a7f37", "#633c01", "#218bff", "#a475f9",
                "#3192aa", "#8c959f",
            ],
        ),
        (
            "tokyo-night-day",
            "Tokyo Night Day",
            "light",
            [
                "#e1e2e7", "#3058b0", "#f0f1f5", "#2e7de9", "#3058b0", "#b7c1e3", "#f52a65",
                "#8c6c3e", "#587539",
            ],
            [
                "#e9e9ed", "#f52a65", "#587539", "#8c6c3e", "#2e7de9", "#9854f1", "#007197",
                "#6172b0", "#a1a6c5", "#f52a65", "#587539", "#8c6c3e", "#2e7de9", "#9854f1",
                "#007197", "#3760bf",
            ],
        ),
        (
            "flexoki-light",
            "Flexoki Light",
            "light",
            [
                "#fffcf0", "#100f0f", "#f2f0e5", "#205ea6", "#100f0f", "#e6e4d9", "#af3029",
                "#ad8301", "#66800b",
            ],
            [
                "#100f0f", "#af3029", "#66800b", "#ad8301", "#205ea6", "#a02f6f", "#24837b",
                "#6f6e69", "#b7b5ac", "#d14d41", "#879a39", "#d0a215", "#4385be", "#ce5d97",
                "#3aa99f", "#cecdc3",
            ],
        ),
        (
            "everforest-light",
            "Everforest Light",
            "light",
            [
                "#fdf6e3", "#4f5d65", "#f4f0d9", "#3a94c5", "#4f5d65", "#e6e2cc", "#f85552",
                "#dfa000", "#8da101",
            ],
            [
                "#5c6a72", "#f85552", "#8da101", "#dfa000", "#3a94c5", "#df69ba", "#35a77c",
                "#dfddc8", "#829181", "#f85552", "#8da101", "#dfa000", "#3a94c5", "#df69ba",
                "#35a77c", "#e6e2cc",
            ],
        ),
    ]
    .into_iter()
    .map(|(id, name, appearance, colors, palette)| {
        let [
            background,
            foreground,
            surface,
            accent,
            cursor,
            selection,
            danger,
            warning,
            success,
        ] = colors.map(str::to_owned);
        Theme {
            id: id.to_owned(),
            name: name.to_owned(),
            appearance: appearance.to_owned(),
            background,
            foreground,
            surface,
            accent,
            cursor,
            selection,
            danger,
            warning,
            success,
            palette: Some(palette.map(str::to_owned).to_vec()),
            selection_foreground: None,
            divider: None,
        }
    })
    .collect()
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
    let raised = mix(&theme.surface, ink, 0.06);
    Palette {
        raised: raised.clone(),
        border: mix(base, ink, 0.1),
        hover: mix(&theme.surface, ink, 0.07),
        active: mix(&theme.surface, ink, 0.12),
        subtext: text_tone(base, &raised, ink, 0.78),
        muted: text_tone(base, &raised, ink, 0.5),
        faint: mix(base, ink, 0.3),
        accent_wash: mix(&theme.surface, &theme.accent, 0.2),
        accent_text: readable_on(&theme.accent, base, ink),
        divider: theme.divider.clone().unwrap_or_else(|| mix(base, ink, 0.1)),
        backdrop: format!("{}b8", &base[..7.min(base.len())]),
        theme: theme.clone(),
    }
}

/// Secondary labels remain readable on both the sheet and raised controls.
/// Keep the preferred quiet tone when it passes; move only as far toward the
/// theme's foreground as required. Custom themes keep their own foreground.
fn text_tone(base: &str, raised: &str, ink: &str, preferred: f64) -> String {
    let readable =
        |color: &str| contrast_ratio(color, base).min(contrast_ratio(color, raised)) >= 4.5;
    let quiet = mix(base, ink, preferred);
    if readable(&quiet) {
        return quiet;
    }
    let (mut low, mut high) = (preferred, 1.);
    for _ in 0..12 {
        let mid = (low + high) / 2.;
        if readable(&mix(base, ink, mid)) {
            high = mid;
        } else {
            low = mid;
        }
    }
    mix(base, ink, high)
}

fn contrast_ratio(a: &str, b: &str) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
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

#[cfg(test)]
mod lookup_tests {
    use super::*;

    #[test]
    fn secondary_labels_meet_text_contrast_on_sheet_and_raised_surfaces() {
        for theme in themes() {
            let colors = palette(&theme);
            for text in [&colors.muted, &colors.subtext] {
                for surface in [&theme.background, &colors.raised] {
                    assert!(
                        contrast_ratio(text, surface) >= 4.5,
                        "{}: {text} on {surface}",
                        theme.name
                    );
                }
            }
            assert_eq!(colors.theme, theme, "terminal colours are never changed");
        }
    }

    #[test]
    fn custom_low_contrast_themes_keep_their_foreground_instead_of_inventing_one() {
        assert_eq!(text_tone("#111111", "#121212", "#131313", 0.5), "#131313");
        assert_eq!(
            text_tone("#000000", "#000000", "#ffffff", 0.78),
            mix("#000000", "#ffffff", 0.78)
        );
    }

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

    #[test]
    fn the_picker_lists_tokyo_night() {
        let theme = themes()
            .into_iter()
            .find(|theme| theme.id == "tokyo-night")
            .expect("desktop picker lists tokyo-night");
        assert_eq!(theme.name, "Tokyo Night");
        assert_eq!(theme.appearance, "dark");
        assert_eq!(theme.background, "#1a1b26");
        assert_eq!(theme.foreground, "#c0caf5");
        assert_eq!(theme.surface, "#16161e");
        assert_eq!(theme.accent, "#7aa2f7");
        assert_eq!(theme.cursor, "#c0caf5");
        assert_eq!(theme.selection, "#283457");
        assert_eq!(theme.danger, "#f7768e");
        assert_eq!(theme.warning, "#e0af68");
        assert_eq!(theme.success, "#9ece6a");
        assert_eq!(
            theme.palette.expect("ansi palette"),
            [
                "#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff",
                "#a9b1d6", "#414868", "#ff899d", "#9fe044", "#faba4a", "#8db0ff", "#c7a9ff",
                "#a4daff", "#c0caf5",
            ]
            .map(str::to_owned)
            .to_vec()
        );
    }
}
