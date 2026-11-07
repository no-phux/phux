//! `phux agent app run NAME`: launch an application and mirror it from the
//! pane this runs in, until it quits (`docs/resource-kinds.md#applications`).
//!
//! The pane is the supervisor, as the ACP guardian's is: the helper keeps
//! running in a Terminal, so it inherits supervision, logs and upgrade
//! survival. It runs the existing mirrors in-process against its own pane:
//! a browser's page (control `page`), or a window's surface (control
//! `window`) and the app's accessibility tree. The app quitting ends the
//! helper, which closes the pane and, with it, the documents; the pane
//! being killed (`SIGHUP`, `SIGTERM`, `SIGINT`) ends the app the helper
//! launched, and never another instance.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::app_launch::{self, Launched, Plan};
use super::surface::{FrameOpts, Frames};
use super::{mirror, page_mirror, surface_mirror};
use crate::commands::{cli_runtime, json_err};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// How long the app gets to appear, then to show its first window.
const STARTUP: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(250);

/// The application the helper launched, for the signal thread to end.
static LAUNCHED: Mutex<Option<Launched>> = Mutex::new(None);

/// `phux agent app run`: an application as resources.
#[derive(Debug, usage::Subcommands)]
pub(crate) enum AppAction {
    /// Launch an application and mirror it into this pane until it quits;
    /// killing the pane quits the app. `phux spawn --app` runs this in a new pane.
    Run {
        /// The application: its name on macOS (`TextEdit`), a command or a
        /// `.desktop` id on Linux; with `--browser`, the browser (`chrome`,
        /// `chromium`, or a path).
        name: String,
        /// Launch the browser NAME with a fresh profile and a debugging port,
        /// open URL (default `about:blank`), and mirror the page.
        #[usage(
            long,
            value_name = "URL",
            num_args = 0..=1,
            default_missing = "about:blank"
        )]
        browser: Option<String>,
        /// Run the browser with no window on screen.
        #[usage(long, requires("--browser"))]
        headless: bool,
        /// Milliseconds between captures and tree reads (each mirror's default when absent).
        #[usage(long, value_name = "MS")]
        interval: Option<u64>,
        #[usage(flatten)]
        frames: FrameOpts,
        /// Print what would be launched and exit, launching nothing.
        #[usage(long)]
        dry_run: bool,
        /// Arguments for the application; they follow `--`.
        #[usage(trailing_var_arg)]
        args: Vec<String>,
    },
}

pub(crate) fn run_app(action: &AppAction, socket: Option<PathBuf>) -> ExitCode {
    let AppAction::Run {
        name,
        browser,
        headless,
        interval,
        frames,
        dry_run,
        args,
    } = action;
    let frames = match frames.resolve(false) {
        Ok(frames) => frames,
        Err(code) => return code,
    };
    let profile = std::env::temp_dir().join(format!("phux-app-{}", std::process::id()));
    let plan = match app_launch::plan(name, browser.as_deref(), *headless, args, profile) {
        Ok(plan) => plan,
        Err(message) => return fail(&message, "name an installed application", EXIT_USAGE),
    };
    if *dry_run {
        outln!(
            "{}",
            serde_json::json!({ "schema_version": 1, "launch": plan.argv() })
        );
        return ExitCode::SUCCESS;
    }
    if std::env::var_os("PHUX_TERMINAL_ID").is_none() {
        return fail(
            "not running in a phux pane",
            "run `phux spawn --app NAME`, or run this inside a pane",
            EXIT_USAGE,
        );
    }
    // The pane's title names the app.
    out!("\x1b]2;{name}\x07");
    if let Err(message) = end_app_on_signal() {
        return fail(&message, "retry", EXIT_FAILURE);
    }
    let launched = match Launched::start(&plan, STARTUP) {
        Ok(launched) => launched,
        Err(message) => return fail(&message, "check the name and that it runs", EXIT_FAILURE),
    };
    outln!("launched {name} (pid {})", launched.pid);
    let mirrors = Mirrors {
        name: name.clone(),
        interval: *interval,
        frames,
        socket,
    };
    *LAUNCHED.lock().unwrap_or_else(PoisonError::into_inner) = Some(launched);
    if let Err(message) = start_producers(&plan, mirrors) {
        if let Some(launched) = take() {
            launched.quit();
        }
        return fail(
            &message,
            "the app started but could not be mirrored",
            EXIT_FAILURE,
        );
    }
    loop {
        std::thread::sleep(POLL);
        if let Some(app) = take_if_quit() {
            app.forget();
            outln!("{name} quit");
            return ExitCode::SUCCESS;
        }
    }
}

/// The launched app, taken once it has quit.
fn take_if_quit() -> Option<Launched> {
    let mut guard = LAUNCHED.lock().unwrap_or_else(PoisonError::into_inner);
    let quit = guard.as_mut().is_some_and(|app| !app.alive());
    let taken = quit.then(|| guard.take()).flatten();
    drop(guard);
    taken
}

fn take() -> Option<Launched> {
    LAUNCHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}

/// End the launched app and exit once the pane is killed or interrupted.
fn end_app_on_signal() -> Result<(), String> {
    use tokio::signal::unix::{SignalKind, signal};
    let rt = cli_runtime().map_err(|_| "no runtime for signals".to_owned())?;
    let mut signals = {
        let _entered = rt.enter();
        [
            SignalKind::hangup(),
            SignalKind::terminate(),
            SignalKind::interrupt(),
        ]
        .map(signal)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| format!("could not watch for signals: {err}"))?
    };
    std::thread::spawn(move || {
        rt.block_on(async {
            let [hup, term, int] = signals.as_mut_slice() else {
                return;
            };
            tokio::select! {
                _ = hup.recv() => {}
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
        });
        if let Some(launched) = take() {
            launched.quit();
        }
        std::process::exit(0);
    });
    Ok(())
}

/// What every mirror the helper runs shares.
struct Mirrors {
    name: String,
    interval: Option<u64>,
    frames: Frames,
    socket: Option<PathBuf>,
}

/// Wait for the app's first window (a browser's first page), then run its
/// mirrors on their own threads against this pane.
fn start_producers(plan: &Plan, m: Mirrors) -> Result<(), String> {
    match plan {
        Plan::Browser { profile, .. } => {
            let port_file = profile.join("DevToolsActivePort");
            let port = wait_for(|| {
                let text = std::fs::read_to_string(&port_file).ok()?;
                text.lines().next()?.trim().parse::<u16>().ok()
            })?
            .ok_or("the browser never opened its debugging port")?;
            let rt = cli_runtime().map_err(|_| "no runtime".to_owned())?;
            wait_for(|| {
                let listing = rt.block_on(page_mirror::list_pages(port)).ok()?;
                page_mirror::pick_page("first", &listing).ok()
            })?
            .ok_or("the browser opened no page")?;
            std::thread::spawn(move || {
                page_mirror::run_mirror(page_mirror::MirrorArgs {
                    page: "first",
                    port,
                    pane: None,
                    interval_ms: m.interval.unwrap_or(1000),
                    frames: m.frames,
                    json: false,
                    socket: m.socket,
                })
            });
        }
        Plan::MacApp { .. } => {
            let pid = current_pid()?;
            let window = wait_for(|| app_launch::window_of_pid(pid, &app_launch::mac_windows()))?;
            spawn_tree(&m, pid);
            match window {
                Some(id) => spawn_surface(m, Some(id), None),
                None => warn!("no window appeared; mirroring the tree only"),
            }
        }
        Plan::Command { .. } => {
            let pid = current_pid()?;
            let class = linux_window(pid)?;
            spawn_tree(&m, pid);
            match class {
                Some(class) => spawn_surface(m, None, Some(class)),
                None => warn!("no Hyprland window found; mirroring the tree only"),
            }
        }
    }
    Ok(())
}

fn current_pid() -> Result<u32, String> {
    LAUNCHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|app| app.pid)
        .ok_or_else(|| "the app is gone".to_owned())
}

/// The Hyprland class of `pid`'s window; `None` at once without Hyprland.
#[cfg(target_os = "linux")]
fn linux_window(pid: u32) -> Result<Option<String>, String> {
    use super::surface_grim::{hyprctl, session_env};
    let env = session_env();
    if hyprctl("version", &env).is_err() {
        return Ok(None);
    }
    wait_for(|| app_launch::class_of_pid(pid, &hyprctl("clients", &env).ok()?))
}

#[cfg(not(target_os = "linux"))]
#[expect(clippy::unnecessary_wraps, reason = "one signature for every platform")]
const fn linux_window(_pid: u32) -> Result<Option<String>, String> {
    Ok(None)
}

fn spawn_tree(m: &Mirrors, pid: u32) {
    let (name, interval, socket) = (m.name.clone(), m.interval, m.socket.clone());
    std::thread::spawn(move || {
        mirror::run_mirror(mirror::MirrorArgs {
            app: &name,
            pid: Some(pid),
            pane: None,
            interval_ms: interval.unwrap_or(500),
            depth: 24,
            json: false,
            socket,
        })
    });
}

/// The window's surface: by CoreGraphics id on macOS, by Hyprland class on Linux.
fn spawn_surface(m: Mirrors, window_id: Option<u32>, class: Option<String>) {
    std::thread::spawn(move || {
        surface_mirror::run_mirror(surface_mirror::MirrorArgs {
            window: class.as_deref().unwrap_or(&m.name),
            window_id,
            pane: None,
            interval_ms: m.interval.unwrap_or(1000),
            frames: m.frames,
            dry_run_input: false,
            json: false,
            socket: m.socket,
        })
    });
}

/// Poll `found` until it answers or [`STARTUP`] passes; an error once the
/// app is gone.
fn wait_for<T>(mut found: impl FnMut() -> Option<T>) -> Result<Option<T>, String> {
    let deadline = Instant::now() + STARTUP;
    loop {
        if let Some(value) = found() {
            return Ok(Some(value));
        }
        let alive = LAUNCHED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
            .is_some_and(Launched::alive);
        if !alive {
            return Err("the app quit while starting".to_owned());
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(POLL);
    }
}

fn fail(message: &str, hint: &str, code: u8) -> ExitCode {
    json_err::emit(
        false,
        &json_err::CliError::new(json_err::codes::WORKLOAD, message, hint),
        code,
    )
}
