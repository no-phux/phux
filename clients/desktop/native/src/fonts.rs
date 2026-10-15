//! Embedded terminal fonts, registered before native views measure or paint.
use gpui::{self, App, Font, FontFallbacks};
use std::borrow::Cow;
use std::collections::HashSet;

pub const BUNDLED: &str = "Paper Mono";
pub const SYMBOLS: &str = "JetBrainsMonoNL Nerd Font Mono";

/// System font families, read once at install.
// ponytail: a family installed while the app runs stays unresolved until restart.
struct Registered {
    families: HashSet<String>,
    fallbacks: FontFallbacks,
}
impl gpui::Global for Registered {}

pub fn install(cx: &mut App) {
    if cx.has_global::<Registered>() {
        return;
    }
    cx.text_system()
        .add_fonts(vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/PaperMono-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/PaperMono-Medium.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/PaperMono-SemiBold.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/PaperMono-Bold.ttf")),
            Cow::Borrowed(include_bytes!(
                "../assets/fonts/JetBrainsMonoNLNerdFontMono-Regular.ttf"
            )),
            Cow::Borrowed(include_bytes!(
                "../assets/fonts/JetBrainsMonoNLNerdFontMono-Bold.ttf"
            )),
            Cow::Borrowed(include_bytes!(
                "../assets/fonts/JetBrainsMonoNLNerdFontMono-Italic.ttf"
            )),
            Cow::Borrowed(include_bytes!(
                "../assets/fonts/JetBrainsMonoNLNerdFontMono-BoldItalic.ttf"
            )),
        ])
        .expect("bundled terminal fonts must be valid");
    let families = cx.text_system().all_font_names().into_iter().collect();
    cx.set_global(Registered {
        families,
        fallbacks: FontFallbacks::from_fonts(vec![SYMBOLS.to_owned()]),
    });
}

/// A family the system lacks would resolve to gpui's proportional fallback
/// (Helvetica): wide cells, narrow glyphs. Ghostty configs often name
/// "JetBrains Mono", which Ghostty embeds but macOS does not install.
pub fn resolve(font: &mut Font, cx: &App) {
    let registered = cx.global::<Registered>();
    substitute(font, &registered.families);
    // CoreText keeps its system cascade after this explicit symbol family,
    // including emoji and CJK. Font variants inherit the same fallback.
    font.fallbacks = Some(registered.fallbacks.clone());
}

fn substitute(font: &mut Font, families: &HashSet<String>) {
    if font.family != BUNDLED && !families.contains(font.family.as_ref()) {
        font.family = BUNDLED.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_family_falls_back_to_bundled_mono() {
        let families = HashSet::from(["Menlo".to_owned()]);
        let mut missing = gpui::font("JetBrains Mono");
        substitute(&mut missing, &families);
        assert_eq!(missing.family, BUNDLED);
        let mut present = gpui::font("Menlo");
        substitute(&mut present, &families);
        assert_eq!(present.family, "Menlo");
    }
}
