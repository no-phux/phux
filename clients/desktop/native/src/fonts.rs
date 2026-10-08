//! Embedded terminal fonts, registered before native views measure or paint.
use gpuix_native::native_extensions::gpui;
use std::borrow::Cow;

struct Registered;
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
    cx.set_global(Registered);
}
