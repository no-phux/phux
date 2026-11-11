//! The Linux side of `phux agent surface mirror`: `grim` captures from a
//! wlroots compositor with screencopy (Hyprland, Sway). `display` is every
//! output and `display:NAME` one (`grim -o`); a window `App` or `App:title`
//! is found through `hyprctl clients` and captured as its rectangle
//! (`grim -g`), so window targeting needs Hyprland.
//!
//! A pane started by a server outside the graphical session has no
//! `WAYLAND_DISPLAY` or `HYPRLAND_INSTANCE_SIGNATURE`; [`session_env`] finds
//! them under the runtime directory. A compositor that withholds frames (a
//! display asleep, a locked session) leaves grim waiting forever, so every
//! command runs under a timeout.

#![cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "macOS builds this module only to test it")
)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::surface::Frames;
#[cfg(target_os = "linux")]
pub(super) use super::surface_grim_input::injector;
pub(super) use super::surface_input::Rect;

/// How long grim or hyprctl may take before the capture counts as stalled.
const TIMEOUT: Duration = Duration::from_secs(10);

const STALLED: &str = "the compositor gave no frame: is the display asleep or the session locked?";

/// What grim is pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Target {
    /// Every output, or the one named.
    Display(Option<String>),
    /// A Hyprland window, `App` or `App:title`, looked up at every capture
    /// so the mirror follows it as it moves.
    Window(String),
}

pub(super) const TARGET_HINT: &str = "name `display`, `display:OUTPUT`, or on Hyprland a visible window as `App` or `App:title` (its class, from `hyprctl clients`)";

pub(super) const REFUSED_HINT: &str = "grim needs a wlroots compositor with screencopy (Hyprland, Sway), an awake display and an unlocked session";

pub(super) fn target(window: &str, window_id: Option<u32>) -> Result<Target, String> {
    if window_id.is_some() {
        return Err("`--window-id` names a macOS window; on Linux name the window".into());
    }
    match window.split_once(':') {
        _ if window == "display" => Ok(Target::Display(None)),
        Some(("display", name)) => Ok(Target::Display(Some(name.to_owned()))),
        _ => {
            let env = session_env();
            if !env.iter().any(|(k, _)| *k == "HYPRLAND_INSTANCE_SIGNATURE") {
                return Err(format!(
                    "{window:?} names a window, and only Hyprland can find one; this session has no Hyprland instance"
                ));
            }
            // Found once up front, so a miss is a usage error, not a capture failure.
            let target = Target::Window(window.to_owned());
            region(&target, &env).map(|_| target)
        }
    }
}

/// Captures of `target` as `frames` asks. With `--max-width`, the first
/// capture learns the scale and later ones reuse it.
pub(super) fn capturer(target: Target, frames: Frames) -> impl FnMut() -> Result<Vec<u8>, String> {
    let env = session_env();
    // ponytail: the scale is fixed by the first frame; a window resized
    // later keeps it. Re-probe on a size change if that matters.
    let mut scale: Option<Option<f64>> = frames.max_width.is_none().then_some(None);
    move || {
        let region = region(&target, &env)?;
        let scale = if let Some(scale) = scale {
            scale
        } else {
            let max = frames.max_width.unwrap_or(u32::MAX);
            let native = grim(&region, frames, None, &env)?;
            if width(&native) <= max {
                scale = Some(None);
                return Ok(native);
            }
            let logical = width(&grim(&region, frames, Some(1.0), &env)?).max(1);
            *scale.insert(Some(f64::from(max) / f64::from(logical)))
        };
        grim(&region, frames, scale, &env)
    }
}

fn width(image: &[u8]) -> u32 {
    phux_client::tree::image_size(image).map_or(0, |size| size.1)
}

/// What one grim run captures.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Region {
    All,
    Output(String),
    Rect(Rect),
}

/// grim's `-g` geometry for a rectangle in the compositor's layout
/// (logical) coordinates, `x,y wxh`.
pub(super) fn geometry(rect: Rect) -> String {
    let Rect { x, y, w, h } = rect;
    format!("{x:.0},{y:.0} {w:.0}x{h:.0}")
}

/// A Hyprland window: its `address` (for `focuswindow`) and rectangle.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Window {
    pub(super) address: String,
    pub(super) rect: Rect,
}

fn region(target: &Target, env: &[(&str, String)]) -> Result<Region, String> {
    match target {
        Target::Display(None) => Ok(Region::All),
        Target::Display(Some(name)) => Ok(Region::Output(name.clone())),
        Target::Window(spec) => {
            let clients = hyprctl("clients", env)?;
            let monitors = hyprctl("monitors", env)?;
            find_window(spec, &clients, &monitors).map(|window| Region::Rect(window.rect))
        }
    }
}

pub(super) fn hyprctl(what: &str, env: &[(&str, String)]) -> Result<String, String> {
    let mut command = Command::new("hyprctl");
    command.args(["-j", what]).envs(env.iter().cloned());
    let out = run(command, "hyprctl did not answer: is Hyprland running?")?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn grim(
    region: &Region,
    frames: Frames,
    scale: Option<f64>,
    env: &[(&str, String)],
) -> Result<Vec<u8>, String> {
    let mut command = Command::new("grim");
    command
        .args(grim_args(region, frames, scale))
        .envs(env.iter().cloned());
    let image = run(command, STALLED)?;
    if image.is_empty() {
        return Err("grim wrote no image".into());
    }
    Ok(image)
}

/// grim's arguments for one capture written to stdout.
pub(super) fn grim_args(region: &Region, frames: Frames, scale: Option<f64>) -> Vec<String> {
    let format = if frames.jpeg.is_some() { "jpeg" } else { "png" };
    let mut args: Vec<String> = vec!["-t".into(), format.into()];
    if let Some(quality) = frames.jpeg {
        args.extend(["-q".into(), quality.to_string()]);
    }
    if let Some(scale) = scale {
        args.extend(["-s".into(), format!("{scale:.4}")]);
    }
    match region {
        Region::All => {}
        Region::Output(name) => args.extend(["-o".into(), name.clone()]),
        Region::Rect(rect) => args.extend(["-g".into(), geometry(*rect)]),
    }
    args.push("-".into());
    args
}

/// The rectangle of the first mapped window on a shown workspace whose
/// class is `App` (any case) and, for `App:title`, whose title contains
/// `title`, from `hyprctl -j clients` and `hyprctl -j monitors`.
pub(super) fn find_window(spec: &str, clients: &str, monitors: &str) -> Result<Window, String> {
    let (app, title) = spec.split_once(':').unwrap_or((spec, ""));
    let parse = |text: &str, what: &str| {
        serde_json::from_str::<Vec<serde_json::Value>>(text.trim())
            .map_err(|err| format!("hyprctl's {what} were not readable: {err}"))
    };
    let shown: Vec<i64> = parse(monitors, "monitors")?
        .iter()
        .flat_map(|m| [&m["activeWorkspace"]["id"], &m["specialWorkspace"]["id"]])
        .filter_map(serde_json::Value::as_i64)
        .filter(|&id| id != 0)
        .collect();
    let clients = parse(clients, "clients")?;
    let matching = |c: &&serde_json::Value| {
        ["class", "initialClass"]
            .iter()
            .any(|key| c[key].as_str().is_some_and(|s| s.eq_ignore_ascii_case(app)))
            && (title.is_empty() || c["title"].as_str().is_some_and(|t| t.contains(title)))
            && c["mapped"].as_bool() != Some(false)
            && c["hidden"].as_bool() != Some(true)
    };
    let mut found = clients.iter().filter(matching).peekable();
    if found.peek().is_none() {
        return Err(format!("no window matches {spec:?}"));
    }
    let client = found
        .find(|c| {
            c["workspace"]["id"]
                .as_i64()
                .is_some_and(|id| shown.contains(&id))
        })
        .ok_or_else(|| format!("{spec:?} is on a workspace no monitor shows"))?;
    let n = |key: &str, i: usize| client[key][i].as_f64().unwrap_or(0.0);
    Ok(Window {
        address: client["address"].as_str().unwrap_or_default().to_owned(),
        rect: Rect {
            x: n("at", 0),
            y: n("at", 1),
            w: n("size", 0),
            h: n("size", 1),
        },
    })
}

/// The session variables this process lacks, found under the runtime
/// directory: `XDG_RUNTIME_DIR` (else `/run/user/UID`), `WAYLAND_DISPLAY`
/// (the first `wayland-N` socket) and `HYPRLAND_INSTANCE_SIGNATURE` (the
/// newest instance under `hypr/`).
pub(super) fn session_env() -> Vec<(&'static str, String)> {
    let uid = rustix::process::getuid().as_raw();
    session_env_from(|key| std::env::var(key).ok().filter(|v| !v.is_empty()), uid)
}

pub(super) fn session_env_from(
    get: impl Fn(&str) -> Option<String>,
    uid: u32,
) -> Vec<(&'static str, String)> {
    let runtime = get("XDG_RUNTIME_DIR").unwrap_or_else(|| format!("/run/user/{uid}"));
    let dir = Path::new(&runtime);
    let mut env = Vec::new();
    if let Some(display) = get("WAYLAND_DISPLAY").or_else(|| first_wayland_socket(dir)) {
        env.push(("WAYLAND_DISPLAY", display));
    }
    if let Some(sig) = get("HYPRLAND_INSTANCE_SIGNATURE").or_else(|| newest_hypr(dir)) {
        env.push(("HYPRLAND_INSTANCE_SIGNATURE", sig));
    }
    env.push(("XDG_RUNTIME_DIR", runtime));
    env
}

fn first_wayland_socket(dir: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| {
            name.strip_prefix("wayland-")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    names.sort_by_key(|name| (name.len(), name.clone()));
    names.into_iter().next()
}

fn newest_hypr(dir: &Path) -> Option<String> {
    std::fs::read_dir(dir.join("hypr"))
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let meta = entry.metadata().ok().filter(std::fs::Metadata::is_dir)?;
            Some((meta.modified().ok()?, entry.file_name().into_string().ok()?))
        })
        .max()
        .map(|(_, name)| name)
}

/// Run `command` to completion and return its stdout, or `stalled` once
/// [`TIMEOUT`] passes. Stdout goes through a file so a large image cannot
/// fill a pipe while this waits.
pub(super) fn run(command: Command, stalled: &str) -> Result<Vec<u8>, String> {
    run_within(command, TIMEOUT, stalled)
}

pub(super) fn run_within(
    mut command: Command,
    timeout: Duration,
    stalled: &str,
) -> Result<Vec<u8>, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let dir = std::env::temp_dir().join(format!("phux-surface-{}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("could not make {}: {err}", dir.display()))?;
    let path = dir.join("stdout");
    let stdout = std::fs::File::create(&path)
        .map_err(|err| format!("could not make {}: {err}", path.display()))?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not run {program}: {err}"))?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{stalled} ({program} gave nothing in {}s)",
                    timeout.as_secs()
                ));
            }
            Err(err) => return Err(format!("could not wait for {program}: {err}")),
        }
    };
    if !status.success() {
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
        }
        return Err(format!("{program} exited with {status}: {}", stderr.trim()));
    }
    std::fs::read(&path).map_err(|err| format!("could not read {program}'s output: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITORS: &str =
        r#"[{"name":"DP-1","activeWorkspace":{"id":1},"specialWorkspace":{"id":0}}]"#;
    const CLIENTS: &str = r#"[
        {"class":"firefox","initialClass":"firefox","title":"Docs","mapped":true,"hidden":false,"workspace":{"id":2},"at":[0,0],"size":[10,10]},
        {"address":"0xa1","class":"firefox","initialClass":"firefox","title":"Mail","mapped":true,"hidden":false,"workspace":{"id":1},"at":[12,34],"size":[800,600]},
        {"class":"kitty","initialClass":"kitty","title":"zsh","mapped":true,"hidden":false,"workspace":{"id":3},"at":[0,0],"size":[1,1]}
    ]"#;

    #[test]
    fn a_window_is_its_rectangle_on_a_shown_workspace() {
        let mail = find_window("Firefox", CLIENTS, MONITORS).unwrap();
        assert_eq!(mail.address, "0xa1");
        assert_eq!(geometry(mail.rect), "12,34 800x600");
        assert_eq!(find_window("firefox:Mail", CLIENTS, MONITORS), Ok(mail));
        let hidden = find_window("firefox:Docs", CLIENTS, MONITORS).unwrap_err();
        assert!(hidden.contains("no monitor shows"), "{hidden}");
        assert!(find_window("kitty", CLIENTS, MONITORS).is_err());
        assert!(
            find_window("mpv", CLIENTS, MONITORS)
                .unwrap_err()
                .contains("no window")
        );
    }

    #[test]
    fn grim_args_follow_the_region_format_and_scale() {
        let png = Frames {
            jpeg: None,
            max_width: None,
        };
        let jpeg = Frames {
            jpeg: Some(70),
            max_width: Some(800),
        };
        assert_eq!(grim_args(&Region::All, png, None), ["-t", "png", "-"]);
        assert_eq!(
            grim_args(&Region::Output("DP-1".into()), jpeg, Some(0.5)),
            ["-t", "jpeg", "-q", "70", "-s", "0.5000", "-o", "DP-1", "-"]
        );
        assert_eq!(
            grim_args(
                &Region::Rect(Rect {
                    x: 1.0,
                    y: 2.0,
                    w: 3.0,
                    h: 4.0
                }),
                png,
                None
            ),
            ["-t", "png", "-g", "1,2 3x4", "-"]
        );
    }

    #[test]
    fn targets_read_display_and_reject_a_window_id() {
        assert_eq!(target("display", None), Ok(Target::Display(None)));
        assert_eq!(
            target("display:HDMI-A-1", None),
            Ok(Target::Display(Some("HDMI-A-1".into())))
        );
        assert!(target("display", Some(4)).is_err());
    }

    #[test]
    fn the_session_is_found_under_the_runtime_dir() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "wayland-1.lock",
            "wayland-1",
            "wayland-10",
            "wayland-",
            "bus",
        ] {
            std::fs::write(dir.path().join(name), "").unwrap();
        }
        let hypr = dir.path().join("hypr");
        for (name, age) in [("old", 100), ("new", 1)] {
            std::fs::create_dir_all(hypr.join(name)).unwrap();
            let when = std::time::SystemTime::now() - Duration::from_secs(age);
            std::fs::File::open(hypr.join(name))
                .unwrap()
                .set_modified(when)
                .unwrap();
        }
        let runtime = dir.path().to_string_lossy().into_owned();
        let env = session_env_from(|k| (k == "XDG_RUNTIME_DIR").then(|| runtime.clone()), 1000);
        assert_eq!(
            env,
            [
                ("WAYLAND_DISPLAY", "wayland-1".to_owned()),
                ("HYPRLAND_INSTANCE_SIGNATURE", "new".to_owned()),
                ("XDG_RUNTIME_DIR", runtime.clone()),
            ]
        );
        // What the environment already has wins, and the runtime dir falls
        // back to the uid's.
        let set = session_env_from(|k| (k == "WAYLAND_DISPLAY").then(|| "wayland-9".into()), 7);
        assert_eq!(set[0], ("WAYLAND_DISPLAY", "wayland-9".to_owned()));
        assert_eq!(
            set.last().unwrap(),
            &("XDG_RUNTIME_DIR", "/run/user/7".to_owned())
        );
    }

    #[test]
    fn a_capture_that_never_finishes_is_a_clear_error_not_a_hang() {
        let mut sleeper = Command::new("sleep");
        sleeper.arg("5");
        let started = Instant::now();
        let err = run_within(sleeper, Duration::from_millis(200), STALLED).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(err.starts_with(STALLED), "{err}");
        let mut fails = Command::new("sh");
        fails.args(["-c", "echo nope >&2; exit 3"]);
        assert!(
            run_within(fails, TIMEOUT, STALLED)
                .unwrap_err()
                .contains("nope")
        );
        let mut prints = Command::new("printf");
        prints.arg("img");
        assert_eq!(run_within(prints, TIMEOUT, STALLED).unwrap(), b"img");
    }
}
