//! A skin resolved against the theme into paint values, and the surfaces
//! views build from it. Views ask the desk for a [`Look`] and style with
//! [`Surfaced::surface`] instead of repeating radii, washes and shadows.

use gpui::{
    BoxShadow, Hsla, Styled, Window, WindowAppearance, WindowBackgroundAppearance, point, px,
};

use crate::settings::DisplayPrefs;
use crate::settings::skin::{Backdrop, RIM, Shadow, Skin, tone};
use crate::settings::theme::{Palette, with_alpha};

use super::color::from_hex;

/// Paint values for one frame: the skin and the theme, resolved.
#[derive(Clone)]
pub struct Look {
    pub skin: Skin,
    /// The theme is light; picks the light half of `[light, dark]` pairs.
    pub light: bool,
    /// The palette with the skin's translucent washes and hairlines, so
    /// hovered and selected chrome tints the sheet instead of covering it.
    pub chrome: Palette,
    /// Theme foreground.
    pub ink: Hsla,
    /// The window sheet.
    pub sheet: Hsla,
    /// Top of the sheen gradient; it fades to transparent over
    /// `skin.material.sheen_height`.
    pub sheen: Hsla,
    /// A pane card: the terminal background at the terminal opacity, raised
    /// toward solid by the skin.
    pub card: Hsla,
    /// Dialog and popover backing.
    pub raised: Hsla,
    /// Floating capsule backing.
    pub capsule: Hsla,
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
        let m = &skin.material;
        let theme = &colors.theme;
        let light = theme.appearance == "light";
        let ink = from_hex(&theme.foreground);
        let solid = f64::from(m.card_solid);
        let card_alpha = prefs.terminal_opacity + (1. - prefs.terminal_opacity) * solid;
        let raised = from_hex(&colors.raised);
        let wash = |amount: f32| with_alpha(&theme.foreground, f64::from(amount));
        let washes = m.washes;
        let mut chrome = colors.clone();
        chrome.hover = wash(washes.hover);
        chrome.active = wash(washes.active);
        chrome.border = wash(washes.border);
        chrome.divider = wash(washes.divider);
        let lift = |layers: &[Shadow]| shadows(layers, light);
        Self {
            skin: *skin,
            light,
            ink,
            sheet: from_hex(&theme.background).opacity(tone(m.sheet_alpha, light)),
            sheen: ink.opacity(tone(m.sheen, light)),
            card: from_hex(&with_alpha(&theme.background, card_alpha)),
            raised: raised.opacity(m.raised),
            capsule: raised.opacity(m.capsule),
            lit: ink.opacity(m.lit),
            rim: ink.opacity(tone(RIM, light)),
            hover: from_hex(&chrome.hover),
            active: from_hex(&chrome.active),
            border: from_hex(&chrome.border),
            divider: from_hex(&chrome.divider),
            pane_shadow: lift(&[m.pane_shadow]),
            popover_shadow: lift(&m.popover_shadow),
            capsule_shadow: lift(&[m.capsule_shadow]),
            tab_shadow: lift(&[m.tab_shadow]),
            chrome,
        }
    }

    /// A pane card's shadows `depth` of the way back (0 raised, 1 recessed):
    /// the pane shadow lowering into the skin's softer lift, plus the inner
    /// well under its top edge.
    pub fn pane_depth_shadows(&self, depth: f32) -> Vec<BoxShadow> {
        let m = &self.skin.material;
        let depth = depth.clamp(0., 1.);
        let mut layers = shadows(&[m.pane_shadow.lerp(m.depth.lift, depth)], self.light);
        layers.extend(inset_shadows(
            &[Shadow::NONE.lerp(m.depth.well, depth)],
            self.light,
        ));
        layers
    }
}

/// GPUI shadows for `layers`. Fully transparent layers are left out.
fn shadows(layers: &[Shadow], light: bool) -> Vec<BoxShadow> {
    cast(layers, light, false)
}

/// Inner shadows for `layers`, as [`shadows`] casts outer ones.
fn inset_shadows(layers: &[Shadow], light: bool) -> Vec<BoxShadow> {
    cast(layers, light, true)
}

fn cast(layers: &[Shadow], light: bool, inset: bool) -> Vec<BoxShadow> {
    layers
        .iter()
        .filter(|layer| tone(layer.alpha, light) > 0.)
        .map(|layer| BoxShadow {
            color: gpui::black().opacity(tone(layer.alpha, light)),
            offset: point(px(0.), px(layer.y)),
            blur_radius: px(layer.blur),
            spread_radius: px(layer.spread),
            inset,
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
    /// A pane card: pane radius, terminal background, clear edge, pane
    /// shadow. Set a focused edge with `border_color` after.
    Pane,
    /// A floating pill of controls.
    Capsule,
    /// The selected titlebar tab.
    Tab,
    /// The selected side-rail tab, cornered as the list rows around it.
    SideTab,
}

/// Radius, backing, border and shadow for a [`Surface`] in one call. Later
/// style calls still override any of them.
pub trait Surfaced: Styled + Sized {
    fn surface(self, look: &Look, surface: Surface) -> Self {
        let shape = &look.skin.shape;
        // No radius is a pill.
        let (radius, bg, border, shadow) = match surface {
            Surface::Card => (
                Some(shape.radius),
                look.raised,
                look.border,
                &look.popover_shadow,
            ),
            Surface::Popover => (
                Some(shape.card_radius),
                look.raised,
                look.rim,
                &look.popover_shadow,
            ),
            Surface::Pane => (
                Some(shape.radius),
                look.card,
                gpui::transparent_black(),
                &look.pane_shadow,
            ),
            Surface::Capsule => (None, look.capsule, look.border, &look.capsule_shadow),
            Surface::Tab => (
                Some(shape.tab_radius),
                look.active,
                look.border,
                &look.tab_shadow,
            ),
            Surface::SideTab => (
                Some(super::ui::list::row_radius(&look.skin)),
                look.active,
                look.border,
                &look.tab_shadow,
            ),
        };
        match radius {
            Some(radius) => self.rounded(px(radius)),
            None => self.rounded_full(),
        }
        .bg(bg)
        .border_1()
        .border_color(border)
        .shadow(shadow.clone())
    }
}

impl<T: Styled + Sized> Surfaced for T {}

/// The platform window background for a backdrop. Glass blurs where native
/// glass is unavailable and before [`apply_backdrop`] installs it.
fn background_appearance(backdrop: Backdrop) -> WindowBackgroundAppearance {
    match backdrop {
        Backdrop::Glass => WindowBackgroundAppearance::Blurred,
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
    fn terminal_is_solid_and_its_panes_cast_no_shadow() {
        let colors = palette(&theme_by_id(default_theme_id(), &[]));
        let look = Look::of(&TERMINAL, &colors, &default_display());
        assert_eq!(look.sheet.a, 1.);
        assert_eq!(look.card.a, 1.);
        assert!(look.pane_shadow.is_empty());
        // Menus and the selected tab keep the desk's lift.
        assert_eq!(look.popover_shadow.len(), 2);
        assert!(!look.tab_shadow.is_empty());
        assert_eq!(
            background_appearance(TERMINAL.material.backdrop),
            WindowBackgroundAppearance::Opaque
        );
    }

    #[test]
    fn a_raised_pane_keeps_its_shadow_and_a_recessed_one_lowers_and_sinks() {
        let colors = palette(&theme_by_id(default_theme_id(), &[]));
        let look = Look::of(&GLASS, &colors, &default_display());
        assert_eq!(look.pane_depth_shadows(0.), look.pane_shadow);
        let back = look.pane_depth_shadows(1.);
        let (outer, inner): (Vec<_>, Vec<_>) = back.iter().partition(|shadow| !shadow.inset);
        assert_eq!(outer.len(), 1);
        assert_eq!(inner.len(), 1);
        assert!(outer[0].blur_radius < look.pane_shadow[0].blur_radius);
        assert!(outer[0].offset.y < look.pane_shadow[0].offset.y);
        // Plain casts nothing at any depth: the veil is its only cue.
        let plain = Look::of(&TERMINAL, &colors, &default_display());
        assert!(plain.pane_depth_shadows(1.).is_empty());
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
