//! Isolated native monitor preview: no daemon, production socket or installed app.
//! Pass an output directory to capture the three tabs and exit.

#[global_allocator]
static ALLOC: phux_perf::alloc::Counting = phux_perf::alloc::Counting::system();

fn main() {
    let capture = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    phux_perf::alloc::enable();
    gpui_platform::application()
        .with_assets(phux_desktop::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            phux_desktop::app::preview_performance_monitor(capture, cx).expect("monitor preview");
        });
}
