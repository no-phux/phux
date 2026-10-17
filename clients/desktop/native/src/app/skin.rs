//! A skin resolved against the theme into paint values, and the surfaces
//! views build from it. Views ask the desk for a [`Look`] and style with
//! [`Surfaced::surface`] instead of repeating radii, washes and shadows.

use gpui::{
    BoxShadow, Hsla, Styled, Window, WindowAppearance, WindowBackgroundAppearance, point, px,
};

use crate::settings::DisplayPrefs;
use crate::settings::skin::{Backdrop, Shadow, Skin, tone};
use crate::settings::theme::{Palette, with_alpha};

use super::color::from_hex;

/// Paint values for one frame: the skin and the theme, resolved.
#[derive(Clone)]
pub struct Look {
    pub skin: Skin,
    /// The palette with the skin's washes, as [`chrome_palette`] builds it.
    pub chrome: Palette,
    /// Theme foreground.
    pub ink: Hsla,
    /// The window sheet.
    pub sheet: Hsla,
    /// Top of the sheen gradient; it fades to transparent over
    /// `skin.material.sheen_height`.
    pub sheen: Hsla,
    /// A pane card: the terminal background.
    pub card: Hsla,
    /// Dialog and popover backing.
    pub raised: Hsla,
    /// Floating capsule backing.
    pub capsule: Hsla,
    /// A quiet pane's edge.
    pub edge: Hsla,
    /// A focused pane's edge.
    pub lit: Hsla,
    /// Popover rim.
    pub rim: Hsla,
    pub hover: Hsla,
    pub active: Hsla,
    pub border: Hsla,
    pub divider: Hsla,
    pub pane_shadow: Vec<BoxShadow>,
    pub popover_shadow: Vec<BoxShadow>,
    pub capsule_shadow: Vec<BoxShadow>,
    pub tab_shadow: Vec<BoxShadow>,
}

impl Look {
    pub fn of(skin: &Skin, colors: &Palette, prefs: &DisplayPrefs) -> Self {
        let light = colors.theme.appearance == "light";
        let ink = from_hex(&colors.theme.foreground);
        let m = &skin.material;
        let chrome = chrome_palette(colors, skin);
        Self {
            skin: *skin,
            ink,
            sheet: from_hex(&colors.theme.background).opacity(tone(m.sheet_alpha, light)),
            sheen: ink.opacity(tone(m.sheen, light)),
            card: card_color(skin, colors, prefs),
            raised: from_hex(&colors.raised).opacity(m.raised),
            capsule: from_hex(&colors.raised).opacity(m.capsule),
            edge: if m.edge > 0. {
                ink.opacity(m.edge)
            } else {
                gpui::transparent_black()
            },
            lit: ink.opacity(m.lit),
            rim: ink.opacity(tone(m.rim, light)),
            hover: from_hex(&chrome.hover),
            active: from_hex(&chrome.active),
            border: from_hex(&chrome.border),
            divider: from_hex(&chrome.divider),
            pane_shadow: shadows(&[m.pane_shadow], light),
            popover_shadow: shadows(&m.popover_shadow, light),
            capsule_shadow: shadows(&[m.capsule_shadow], light),
            tab_shadow: shadows(&[m.tab_shadow], light),
            chrome,
        }
    }
}

/// The palette with translucent washes and hairlines from the skin, so
/// hovered and selected chrome tints the sheet instead of covering it.
pub fn chrome_palette(colors: &Palette, skin: &Skin) -> Palette {
    let mut colors = colors.clone();
    let ink = colors.theme.foreground.clone();
    let washes = &skin.material.washes;
    let wash = |amount: f32| with_alpha(&ink, f64::from(amount));
    colors.hover = wash(washes.hover);
    colors.active = wash(washes.active);
    colors.border = wash(washes.border);
    colors.divider = wash(washes.divider);
    colors
}

/// The terminal background a pane card paints: the theme background at the
/// terminal opacity, raised toward solid by the skin.
pub fn card_color(skin: &Skin, colors: &Palette, prefs: &DisplayPrefs) -> Hsla {
    let opacity = prefs.terminal_opacity;
    let alpha = opacity + (1. - opacity) * f64::from(skin.material.card_solid);
    from_hex(&with_alpha(&colors.theme.background, alpha))
}

/// GPUI shadows for `layers`. Fully transparent layers are left out.
pub fn shadows(layers: &[Shadow], light: bool) -> Vec<BoxShadow> {
    layers
        .iter()
        .filter(|layer| tone(layer.alpha, light) > 0.)
        .map(|layer| BoxShadow {
            color: gpui::black().opacity(tone(layer.alpha, light)),
            offset: point(px(0.), px(layer.y)),
            blur_radius: px(layer.blur),
            spread_radius: px(layer.spread),
            inset: false,
        })
        .collect()
}

/// A styled surface on the sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// A raised notice card (notification peek, drop hint): pane radius,
    /// raised backing, hairline border, popover shadow.
    Card,
    /// A dialog, palette or popover card: card radius, raised backing, rim,
    /// popover shadow.
    Popover,
    /// A pane card: pane radius, terminal background, quiet edge, pane
    /// shadow. Set a focused edge with `border_color` after.
    Pane,
    /// A floating pill of controls.
    Capsule,
    /// The selected titlebar tab.
    Tab,
    /// The selected side-rail tab.
    SideTab,
}

/// Radius, backing, border and shadow for a [`Surface`] in one call. Later
/// style calls still override any of them.
pub trait Surfaced: Styled + Sized {
    fn surface(self, look: &Look, surface: Surface) -> Self {
        let shape = &look.skin.shape;
        match surface {
            Surface::Card => self
                .rounded(px(shape.radius))
                .bg(look.raised)
                .border_1()
                .border_color(look.border)
                .shadow(look.popover_shadow.clone()),
            Surface::Popover => self
                .rounded(px(shape.card_radius))
                .bg(look.raised)
                .border_1()
                .border_color(look.rim)
                .shadow(look.popover_shadow.clone()),
            Surface::Pane => self
                .rounded(px(shape.radius))
                .bg(look.card)
                .border_1()
                .border_color(look.edge)
                .shadow(look.pane_shadow.clone()),
            Surface::Capsule => self
                .rounded_full()
                .bg(look.capsule)
                .border_1()
                .border_color(look.border)
                .shadow(look.capsule_shadow.clone()),
            Surface::Tab | Surface::SideTab => self
                .rounded(px(
                    shape.tab_radius[usize::from(surface == Surface::SideTab)]
                ))
                .bg(look.active)
                .border_1()
                .border_color(look.border)
                .shadow(look.tab_shadow.clone()),
        }
    }
}

impl<T: Styled + Sized> Surfaced for T {}

/// The platform window background for a backdrop. Glass blurs where native
/// glass is unavailable and before [`apply_backdrop`] installs it.
pub fn background_appearance(backdrop: Backdrop) -> WindowBackgroundAppearance {
    match backdrop {
        Backdrop::Glass | Backdrop::Blur => WindowBackgroundAppearance::Blurred,
        Backdrop::Opaque => WindowBackgroundAppearance::Opaque,
    }
}

/// Put the window's background behind the sheet. Glass installs the native
/// material where the platform has it and blurs otherwise; every other
/// backdrop removes any installed material first. `appearance` is the
/// theme's light or dark.
pub fn apply_backdrop(window: &mut Window, backdrop: Backdrop, appearance: WindowAppearance) {
    // Radius 0: the window frame already clips content to its rounded corners.
    if backdrop == Backdrop::Glass && window.set_glass_effect(px(0.), appearance) {
        return;
    }
    window.clear_glass_effect();
    window.set_background_appearance(background_appearance(backdrop));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::skin::{GLASS, TERMINAL};
    use crate::settings::theme::{default_theme_id, palette, theme_by_id, themes};
    use crate::settings::{default_display, resolved_id, skin_by_id};

    fn palettes() -> Vec<Palette> {
        themes()
            .iter()
            .map(|theme| palette(&theme_by_id(&theme.id, &[])))
            .chain([palette(&theme_by_id(default_theme_id(), &[]))])
            .collect()
    }

    #[test]
    fn glass_reproduces_the_shipped_paint() {
        for colors in palettes() {
            let prefs = default_display();
            let look = Look::of(&GLASS, &colors, &prefs);
            let light = colors.theme.appearance == "light";
            let ink = from_hex(&colors.theme.foreground);
            assert_eq!(
                look.sheet,
                from_hex(&colors.theme.background).opacity(if light { 0.58 } else { 0.44 })
            );
            assert_eq!(look.sheen, ink.opacity(if light { 0.07 } else { 0.05 }));
            assert_eq!(
                look.card,
                from_hex(&with_alpha(
                    &colors.theme.background,
                    prefs.terminal_opacity
                ))
            );
            assert_eq!(look.edge, gpui::transparent_black());
            assert_eq!(look.lit, ink.opacity(0.22));
            assert_eq!(look.rim, ink.opacity(if light { 0.12 } else { 0.14 }));
            assert_eq!(look.raised, from_hex(&colors.raised).opacity(0.97));
            let ink_hex = &colors.theme.foreground;
            assert_eq!(look.chrome.hover, with_alpha(ink_hex, 0.06));
            assert_eq!(look.chrome.active, with_alpha(ink_hex, 0.11));
            assert_eq!(look.chrome.border, with_alpha(ink_hex, 0.08));
            assert_eq!(look.chrome.divider, with_alpha(ink_hex, 0.08));
            assert_eq!(look.pane_shadow.len(), 1);
            assert_eq!(
                look.pane_shadow[0].color,
                gpui::black().opacity(if light { 0.08 } else { 0.18 })
            );
            assert_eq!(look.pane_shadow[0].spread_radius, px(-8.));
            assert_eq!(look.popover_shadow.len(), 2);
            assert_eq!(look.popover_shadow[0].blur_radius, px(64.));
        }
    }

    #[test]
    fn terminal_is_solid_and_casts_no_shadow() {
        let colors = palette(&theme_by_id(default_theme_id(), &[]));
        let look = Look::of(&TERMINAL, &colors, &default_display());
        assert_eq!(look.sheet.a, 1.);
        assert_eq!(look.card.a, 1.);
        assert!(look.pane_shadow.is_empty());
        assert!(look.popover_shadow.is_empty());
        assert_eq!(
            background_appearance(TERMINAL.material.backdrop),
            WindowBackgroundAppearance::Opaque
        );
    }

    #[test]
    fn plain_resolves_to_the_terminal_skin() {
        let prefs = crate::settings::DisplayPrefs {
            plain: true,
            ..default_display()
        };
        let skin = skin_by_id(resolved_id(&prefs.skin_id, prefs.plain));
        assert_eq!(skin.id, "terminal");
        assert_eq!(
            background_appearance(GLASS.material.backdrop),
            WindowBackgroundAppearance::Blurred
        );
    }
}
