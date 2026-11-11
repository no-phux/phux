//! phux on Rune: the desktop client.
//!
//! One window over the shared client runtime: a sidebar of every session
//! and terminal the server lists, the selected terminal, and a status line.

mod activity;
mod alerts;
mod creature;
mod desk;
mod diag;
mod dock;
mod galaxy;
mod grid;
mod hive;
mod host_color;
mod inbox;
mod keys;
mod kitchen;
mod launch;
mod links;
mod machines;
pub mod mobile;
mod onboarding;
mod palette;
mod paths;
mod perf;
mod pitch;
mod pitlane;
mod reef;
mod standing;
mod system_settings;
mod theme;
mod ui;
mod updates;

use desk::Desk;
use rune::{AppSpec, Chrome};

/// The desktop entry's name, and what `phux omarchy setup` tags as a terminal.
const APP_ID: &str = "sh.phux.rune";

pub fn run_desktop() {
    // The updater asks a new binary this before trusting it can run here.
    if rune::extra_args().iter().any(|arg| arg == "--version") {
        println!("phux-rune {}", updates::VERSION);
        return;
    }
    diag::install();
    // The window's Wayland app_id and X11 class: what matches the .desktop
    // entry, the icon, and a compositor's rules (Omarchy's terminal tag).
    rune::set_app_id(APP_ID);
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
    let phone = options.phone;
    let (size, min_size) = if phone {
        ((393.0, 852.0), (320.0, 568.0))
    } else {
        ((1100.0, 720.0), (640.0, 360.0))
    };
    let spec = app_spec(size, min_size);
    perf::report_to_file();
    let app = desk::with_menus(rune::App::new(spec));
    let window = app.handle();
    let app = app
        .init(move || {
            let mut desk = Desk::new(&resolved).with_phone(phone);
            desk.onboarding.window = Some(window);
            desk
        })
        .chrome(Chrome {
            full_size_content: true,
            traffic_lights: Some((14.0, 14.0)),
            drag_regions: true,
        })
        .standard_menus()
        // Rune's native host drops on_modifiers; keep logical events.
        .modifier_keys()
        .on_ime(|desk: &mut Desk, event, _| desk.ime(event))
        .on_focus(|desk: &mut Desk, focused, _| desk.focus_changed(focused))
        .on_frame_drawn(|rt: &rune::Runtime<Desk>| rt.state.drawn());
    match perf::frame_recorder() {
        Some(record) => app.on_frame_encoded(record).run(),
        None => app.run(),
    }
}

fn app_spec(size: (f32, f32), min_size: (f32, f32)) -> AppSpec<Desk> {
    AppSpec {
        min_size: Some(min_size),
        fonts: grid::FONTS,
        families: grid::FAMILIES,
        setup: Some(|desk: &mut Desk, waker| desk.listen(waker)),
        poll: Some(|desk: &mut Desk, clock, tx| desk.poll(clock, tx)),
        on_key: Some(|desk: &mut Desk, event, tx| desk.key(event, tx)),
        ..AppSpec::new("phux", size, placeholder_init, ui::view)
    }
}

/// `AppSpec` needs a plain `fn`; `App::init` supplies the real state.
fn placeholder_init() -> Desk {
    Desk::new(&launch::resolve(&launch::Options::default()))
}
