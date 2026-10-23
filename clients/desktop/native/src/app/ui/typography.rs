//! App typography, separate from every terminal's chosen font and geometry.

use gpui::{App, px};
use gpui_kit::component::Theme;
use std::borrow::Cow;

pub(crate) const FAMILY: &str = "Inter";
pub(crate) const BASE: f32 = 15.;
pub(crate) const LEADING: f32 = 1.25;

struct Installed;
impl gpui::Global for Installed {}

pub(crate) fn install(cx: &mut App) {
    if !cx.has_global::<Installed>() {
        cx.text_system()
            .add_fonts(vec![Cow::Borrowed(include_bytes!(
                "../../../assets/fonts/Inter.ttf"
            ))])
            .expect("bundled interface font must be valid");
        cx.set_global(Installed);
    }
    Theme::update(cx, |theme| {
        theme.font_family = FAMILY.into();
        theme.font_size = px(BASE);
    });
}
