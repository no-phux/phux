//! phux on Rune: the desktop client.
//!
//! One window over the shared client runtime: a sidebar of every session
//! and terminal the server lists, the selected terminal, and a status line.

mod activity;
mod addressable;
mod alerts;
mod askpass;
mod changes;
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
mod pair;
mod palette;
mod paths;
mod perf;
mod pitch;
mod pitlane;
mod reef;
mod session_color;
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
    // ssh runs this executable to ask for a password or 2FA code.
    if let Some(code) = askpass::answer() {
        std::process::exit(code);
    }
    // The updater asks a new binary this before trusting it can run here.
    if rune::extra_args().iter().any(|arg| arg == "--version") {
        println!("phux-rune {}", updates::VERSION);
        return;
    }
    diag::install();
    // Desk's destructor closes popup commands and releases server view leases;
    // the performance recorder also flushes on drop. Rune now exits without
    // destructors by default, so retain the window's orderly teardown.
    rune::orderly_exit();
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
    let mut app = desk::with_menus(rune::App::new(spec));
    if let Some(chord) = desk::quick::configured().filter(|_| !phone) {
        desk::quick::add(&mut app, &chord, resolved.clone());
    }
    let window = app.handle();
    let app = app
        .init(move || {
            let mut desk = Desk::new(&resolved).with_phone(phone);
            desk.onboarding.dock.window = Some(window);
            desk
        })
        .chrome(Chrome {
            full_size_content: true,
            traffic_lights: Some((14.0, 14.0)),
            drag_regions: true,
        })
        .standard_menus()
        // Transparent so the glass slider can show Liquid Glass behind the
        // chrome; the root paints it solid until glass is turned up.
        .window_options(rune::WindowOptions {
            transparent: cfg!(target_os = "macos"),
            ..rune::WindowOptions::default()
        })
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

pub(crate) fn app_spec(size: (f32, f32), min_size: (f32, f32)) -> AppSpec<Desk> {
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
