//! Standalone native material preview; requires no daemon or production state.
fn main() {
    gpui_platform::application()
        .with_assets(phux_desktop::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            if std::env::var_os("PHUX_GLASS_PREVIEW_LIGHT").is_none() {
                gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            }
            if std::env::args().any(|argument| argument == "--bench") {
                phux_desktop::app::benchmark_menu_fixture(cx).expect("menu creation benchmark");
                cx.quit();
            } else {
                phux_desktop::app::open_menu_fixture(cx).expect("native menu preview");
            }
        });
}
