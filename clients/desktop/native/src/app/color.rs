//! Hex colours from the settings theme, as GPUI paint colours, and the Kit theme they drive.

use gpui::{App, Hsla, rgb};
use gpui_kit::component::{Theme, ThemeMode};

use crate::settings::theme::{Palette, mix};

/// Parse `#RRGGBB` or `#RRGGBBAA`.
pub fn from_hex(color: &str) -> Hsla {
    let hex = color.trim().trim_start_matches('#');
    let (body, alpha) = if hex.len() == 8 {
        (
            &hex[..6],
            u8::from_str_radix(&hex[6..], 16).unwrap_or(255) as f32 / 255.,
        )
    } else if hex.len() == 6 {
        (hex, 1.)
    } else {
        return rgb(0).into();
    };
    let value = u32::from_str_radix(body, 16).unwrap_or(0);
    rgb(value).opacity(alpha).into()
}

/// Paint Kit controls (buttons, inputs, palette, settings, switches, sliders)
/// with the phux palette, in the theme's own light or dark mode.
pub fn apply_kit_theme(colors: &Palette, cx: &mut App) {
    let theme = &colors.theme;
    let mode = if theme.appearance == "light" { ThemeMode::Light } else { ThemeMode::Dark };
    if Theme::global(cx).mode != mode {
        Theme::change(mode, None, cx);
    }
    let fg = from_hex(&theme.foreground);
    let accent = from_hex(&theme.accent);
    let accent_hover = from_hex(&mix(&theme.accent, &theme.foreground, 0.12));
    let accent_active = from_hex(&mix(&theme.accent, &theme.background, 0.18));
    let accent_text = from_hex(&colors.accent_text);
    let surface = from_hex(&theme.surface);
    let raised = from_hex(&colors.raised);
    let border = from_hex(&colors.border);
    let hover = from_hex(&colors.hover);
    let active = from_hex(&colors.active);
    let muted = from_hex(&colors.muted);
    let danger = from_hex(&theme.danger);
    let danger_hover = from_hex(&mix(&theme.danger, &theme.foreground, 0.12));
    let success = from_hex(&theme.success);
    let warning = from_hex(&theme.warning);
    let on_status = from_hex(&theme.background);
    Theme::update(cx, |state| {
        let kit = &mut state.colors;
        kit.background = from_hex(&theme.background);
        kit.foreground = fg;
        kit.border = border;
        kit.input = border;
        kit.ring = accent;
        kit.caret = from_hex(&theme.cursor);
        kit.selection = from_hex(&theme.selection);
        kit.overlay = from_hex(&colors.backdrop);
        kit.popover = raised;
        kit.popover_foreground = fg;
        kit.muted = surface;
        kit.muted_foreground = muted;
        kit.accent = hover;
        kit.accent_foreground = fg;
        kit.primary = accent;
        kit.primary_hover = accent_hover;
        kit.primary_active = accent_active;
        kit.primary_foreground = accent_text;
        kit.secondary = surface;
        kit.secondary_hover = hover;
        kit.secondary_active = active;
        kit.secondary_foreground = fg;
        kit.button = surface;
        kit.button_hover = hover;
        kit.button_active = active;
        kit.button_foreground = fg;
        kit.button_primary = accent;
        kit.button_primary_hover = accent_hover;
        kit.button_primary_active = accent_active;
        kit.button_primary_foreground = accent_text;
        kit.button_secondary = surface;
        kit.button_secondary_hover = hover;
        kit.button_secondary_active = active;
        kit.button_secondary_foreground = fg;
        kit.button_danger = danger;
        kit.button_danger_hover = danger_hover;
        kit.button_danger_active = danger;
        kit.button_danger_foreground = on_status;
        kit.danger = danger;
        kit.danger_hover = danger_hover;
        kit.danger_active = danger;
        kit.danger_foreground = on_status;
        kit.success = success;
        kit.success_foreground = on_status;
        kit.warning = warning;
        kit.warning_foreground = on_status;
        kit.list = raised;
        kit.list_hover = hover;
        kit.list_active = from_hex(&colors.accent_wash);
        kit.list_active_border = accent;
        kit.group_box = surface;
        kit.group_box_foreground = fg;
        kit.sidebar = surface;
        kit.sidebar_foreground = fg;
        kit.sidebar_border = border;
        kit.sidebar_accent = hover;
        kit.sidebar_accent_foreground = fg;
        kit.sidebar_primary = accent;
        kit.sidebar_primary_foreground = accent_text;
        kit.tab_bar = surface;
        kit.tab = surface;
        kit.tab_foreground = muted;
        kit.tab_active = active;
        kit.tab_active_foreground = fg;
        kit.title_bar = surface;
        kit.title_bar_border = border;
        kit.status_bar = surface;
        kit.status_bar_border = border;
        kit.slider_bar = accent;
        kit.slider_thumb = fg;
        kit.switch = border;
        kit.link = accent;
        kit.link_hover = accent_hover;
        kit.link_active = accent_active;
    });
}
