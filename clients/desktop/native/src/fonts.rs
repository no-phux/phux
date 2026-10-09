//! Embedded terminal fonts, registered before native views measure or paint.
use gpuix_native::native_extensions::gpui;
use std::borrow::Cow;
use std::collections::HashSet;

pub const BUNDLED: &str = "Paper Mono";

/// System font families, read once at install.
// ponytail: a family installed while the app runs stays unresolved until restart.
struct Registered(HashSet<String>);
impl gpui::Global for Registered {}

pub fn install(cx: &mut gpui::App) {
    if cx.has_global::<Registered>() {
        return;
    }
    cx.text_system()
        .add_fonts(vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/PaperMono-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/PaperMono-Medium.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/PaperMono-SemiBold.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/PaperMono-Bold.ttf")),
        ])
        .expect("bundled Paper Mono fonts must be valid");
    let families = cx.text_system().all_font_names().into_iter().collect();
    cx.set_global(Registered(families));
}

/// A family the system lacks would resolve to gpui's proportional fallback
/// (Helvetica): wide cells, narrow glyphs. Ghostty configs often name
/// "JetBrains Mono", which Ghostty embeds but macOS does not install.
pub fn resolve(font: &mut gpui::Font, cx: &gpui::App) {
    substitute(font, &cx.global::<Registered>().0);
}

fn substitute(font: &mut gpui::Font, families: &HashSet<String>) {
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
