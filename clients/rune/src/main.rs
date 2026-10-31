//! phux on Rune: an alpha desktop client beside the GPUI one.
//!
//! One window over the shared client runtime: a sidebar of every session
//! and terminal the server lists, the selected terminal, and a status line.

mod activity;
mod alerts;
mod desk;
mod dock;
mod grid;
mod inbox;
mod keys;
mod launch;
mod links;
mod machines;
mod palette;
mod perf;
mod theme;
mod ui;
mod updates;

use desk::Desk;
use rune::{AppSpec, Chrome};

fn main() {
    // The updater asks a new binary this before trusting it can run here.
    if rune::extra_args().iter().any(|arg| arg == "--version") {
        println!("phux-rune {}", updates::VERSION);
        return;
    }
    rune::prewarm_gpu();
    // Rune keeps its own flags (`--snap`, `--script`, ...); the rest are ours.
    let options = match launch::Options::parse(rune::extra_args().iter().cloned()) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    // The window opens first; `Desk::listen` finds the server on a worker.
    let resolved = launch::resolve(&options);
    let spec = AppSpec {
        min_size: Some((640.0, 360.0)),
        fonts: grid::FONTS,
        families: grid::FAMILIES,
        setup: Some(|desk: &mut Desk, waker| desk.listen(waker)),
        poll: Some(|desk: &mut Desk, clock, tx| desk.poll(clock, tx)),
        on_key: Some(|desk: &mut Desk, event, tx| desk.key(event, tx)),
        ..AppSpec::new("phux", (1100.0, 720.0), placeholder_init, ui::view)
    };
    perf::report_to_file();
    let app = rune::App::new(spec)
        .init(move || Desk::new(&resolved))
        .chrome(Chrome {
            full_size_content: true,
            traffic_lights: Some((14.0, 14.0)),
            drag_regions: true,
        })
        .standard_menus()
        // ⌘ alone shows the link under a still pointer.
        .modifier_keys()
        .on_ime(|desk: &mut Desk, event, _| desk.ime(event))
        .on_focus(|desk: &mut Desk, focused, _| desk.focus_changed(focused))
        .on_frame_drawn(|rt: &rune::Runtime<Desk>| rt.state.drawn());
    match perf::frame_recorder() {
        Some(record) => app.on_frame_encoded(record).run(),
        None => app.run(),
    }
}

/// `AppSpec` needs a plain `fn`; `App::init` supplies the real state.
fn placeholder_init() -> Desk {
    Desk::new(&launch::resolve(&launch::Options::default()))
}
