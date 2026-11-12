//! Input for the Linux surface mirror: a viewer's intents (`phux agent page
//! click|type|key|scroll`) carried out on the desktop. The wlroots
//! virtual-pointer and virtual-keyboard protocols are spoken natively
//! (`surface_wayland`) when the compositor offers them; otherwise through
//! command-line tools:
//!
//! - Pointer: Hyprland places the cursor (`hyprctl dispatch movecursor`), in
//!   layout coordinates, then `wlrctl pointer click` (or `ydotool click`)
//!   presses. `hyprctl dispatch` alone cannot click, and `wlrctl` moves only
//!   relative to where the pointer is.
//! - Keyboard: `wtype`, else `wlrctl keyboard type` (text only), else
//!   `ydotool` (which needs `ydotoold` and uinput).
//! - A window target is focused first (`hyprctl dispatch focuswindow`).
//!
//! A click is in the published image's pixels. The image shows a rectangle
//! of the layout (every output, one output, or a window, re-read at each
//! intent since windows move), so the point maps through
//! [`to_screen`](super::surface_input::to_screen), which covers per-output
//! scale and `--max-width` alike. `Navigate` has no meaning for a desktop.

#![cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "macOS builds this module only to test it")
)]

use std::process::Command;

use phux_client::tree::{DocumentInput, DocumentKey};

use super::surface_grim::{Rect, Target, find_window, hyprctl, run, session_env};
use super::surface_input::{chord_letter, to_screen};

/// The input tools found on `PATH` at startup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "one flag per tool on PATH")]
pub(super) struct Tools {
    pub(super) hyprctl: bool,
    pub(super) wlrctl: bool,
    pub(super) ydotool: bool,
    pub(super) wtype: bool,
}

impl Tools {
    fn detect() -> Self {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let has = |name: &str| std::env::split_paths(&path).any(|dir| dir.join(name).is_file());
        Self {
            hyprctl: has("hyprctl"),
            wlrctl: has("wlrctl"),
            ydotool: has("ydotool"),
            wtype: has("wtype"),
        }
    }
}

/// Where the published image sits in the layout, and the window to focus
/// before acting.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Place {
    pub(super) frame: Rect,
    pub(super) window: Option<String>,
    /// Every output together, which the native pointer moves within.
    pub(super) layout: Rect,
}

/// Carry out each intent on the desktop `target` shows, or with `dry_run`
/// only log the commands. A failure is returned for the mirror to report;
/// it never ends the mirror.
pub(super) fn injector(
    target: &Target,
    dry_run: bool,
) -> impl FnMut(&DocumentInput, (u32, u32)) -> Result<(), String> + use<> {
    let env = session_env();
    let tools = Tools::detect();
    let native = native_protocols(&env);
    if dry_run {
        eprintln!("dry-run: input tools on PATH: {tools:?}; native pointer, keyboard: {native:?}");
    }
    let injector = Injector {
        native,
        hyprland: tools.hyprctl && env.iter().any(|(k, _)| *k == "HYPRLAND_INSTANCE_SIGNATURE"),
        target: target.clone(),
        env,
        tools,
        dry_run,
    };
    move |input, image| {
        if let DocumentInput::Navigate { url } = input {
            tracing::debug!(%url, "a desktop has no address; navigate ignored");
            if dry_run {
                eprintln!("dry-run: navigate {url:?} ignored: a desktop has no address");
            }
            return Ok(());
        }
        injector.act(input, image)
    }
}

/// Window classes (`hyprctl` `class`, any case) that paste on Ctrl+Shift+V.
const TERMINALS: [&str; 14] = [
    "kitty",
    "alacritty",
    "foot",
    "footclient",
    "ghostty",
    "com.mitchellh.ghostty",
    "wezterm",
    "org.wezfurlong.wezterm",
    "xterm",
    "konsole",
    "org.kde.konsole",
    "gnome-terminal",
    "gnome-terminal-server",
    "org.gnome.terminal",
];

/// Whether the window `hyprctl -j` describes is a terminal: `json` is
/// `activewindow`'s object, or `clients`' array searched for `address`.
pub(super) fn is_terminal(json: &str, address: Option<&str>) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json.trim()) else {
        return false;
    };
    let window = match (&value, address) {
        (serde_json::Value::Array(all), Some(address)) => all
            .iter()
            .find(|w| w["address"].as_str() == Some(address))
            .unwrap_or(&serde_json::Value::Null),
        _ => &value,
    };
    let class = window["class"].as_str().unwrap_or_default();
    TERMINALS.iter().any(|t| t.eq_ignore_ascii_case(class))
}

/// Whether the compositor offers the virtual pointer and keyboard.
#[cfg(target_os = "linux")]
fn native_protocols(env: &[(&str, String)]) -> (bool, bool) {
    use super::surface_wayland::{KEYBOARD, POINTER, Session};
    Session::connect(env).map_or((false, false), |s| (s.has(POINTER), s.has(KEYBOARD)))
}

#[cfg(not(target_os = "linux"))]
const fn native_protocols(_env: &[(&str, String)]) -> (bool, bool) {
    (false, false)
}

struct Injector {
    native: (bool, bool),
    target: Target,
    env: Vec<(&'static str, String)>,
    tools: Tools,
    hyprland: bool,
    dry_run: bool,
}

impl Injector {
    /// Carry out (or log) one intent on an `image`-sized frame.
    fn act(&self, input: &DocumentInput, image: (u32, u32)) -> Result<(), String> {
        let Self {
            native,
            target,
            env,
            tools,
            hyprland,
            dry_run,
        } = self;
        let pointer = matches!(
            input,
            DocumentInput::Click { .. } | DocumentInput::Scroll { .. } | DocumentInput::Drag { .. }
        );
        let place = if *hyprland && (pointer || matches!(target, Target::Window(_))) {
            Some(place(target, env)?)
        } else {
            None
        };
        if *dry_run && let Some(place) = &place {
            eprintln!(
                "dry-run: {input:?} on a {}x{} frame of {target:?} at {}",
                image.0,
                image.1,
                super::surface_grim::geometry(place.frame)
            );
        }
        let chord = chord_letter(input, false).is_some();
        // A terminal takes Ctrl+Shift chords; the window that takes the
        // chord is the target, or whatever has focus on a display.
        let shift = chord && *hyprland && {
            let window = place.as_ref().and_then(|p| p.window.as_deref());
            let what = if window.is_some() {
                "clients"
            } else {
                "activewindow"
            };
            hyprctl(what, env).is_ok_and(|json| is_terminal(&json, window))
        };
        let native = if pointer { native.0 } else { native.1 };
        let (argvs, ops) = if native {
            let focus = place.as_ref().and_then(|p| p.window.as_deref()).map(focus);
            let ops = if chord {
                let letter = chord_letter(input, shift).ok_or(NO_SELECT_ALL)?;
                vec![super::surface_wayland::chord(letter, shift)]
            } else {
                super::surface_wayland::ops(input, image, place.as_ref())?
            };
            (focus.into_iter().collect(), ops)
        } else {
            (
                plan(input, image, place.as_ref(), *tools, shift)?,
                Vec::new(),
            )
        };
        for argv in argvs {
            if *dry_run {
                eprintln!("dry-run: would run: {}", shown(&argv));
                continue;
            }
            let mut command = Command::new(&argv[0]);
            command.args(&argv[1..]).envs(env.iter().cloned());
            run(command, "the input tool did not finish")?;
        }
        if *dry_run {
            for op in &ops {
                eprintln!("dry-run: would send natively: {op:?}");
            }
        } else if !ops.is_empty() {
            #[cfg(target_os = "linux")]
            super::surface_wayland::Session::connect(env)?.run(&ops)?;
        }
        Ok(())
    }
}

/// The command that focuses window `address`.
fn focus(address: &str) -> Vec<String> {
    [
        "hyprctl",
        "dispatch",
        "focuswindow",
        &format!("address:{address}"),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// `target`'s rectangle now (a window may have moved since the frame).
fn place(target: &Target, env: &[(&str, String)]) -> Result<Place, String> {
    let monitors = hyprctl("monitors", env)?;
    let layout = monitors_rect(&monitors, None)?;
    match target {
        Target::Display(name) => Ok(Place {
            frame: monitors_rect(&monitors, name.as_deref())?,
            window: None,
            layout,
        }),
        Target::Window(spec) => {
            let window = find_window(spec, &hyprctl("clients", env)?, &monitors)?;
            Ok(Place {
                frame: window.rect,
                window: Some(window.address),
                layout,
            })
        }
    }
}

/// The layout rectangle of output `name`, or of all outputs together, from
/// `hyprctl -j monitors` (whose `width`/`height` are physical pixels).
pub(super) fn monitors_rect(monitors: &str, name: Option<&str>) -> Result<Rect, String> {
    let monitors: Vec<serde_json::Value> = serde_json::from_str(monitors.trim())
        .map_err(|err| format!("hyprctl's monitors were not readable: {err}"))?;
    let rect = |m: &serde_json::Value| {
        let n = |key: &str| m[key].as_f64().unwrap_or(0.0);
        let scale = m["scale"].as_f64().filter(|&s| s > 0.0).unwrap_or(1.0);
        let (w, h) = (n("width") / scale, n("height") / scale);
        // Odd transforms turn the output a quarter.
        let (w, h) = if m["transform"].as_i64().unwrap_or(0) % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        };
        Rect {
            x: n("x"),
            y: n("y"),
            w,
            h,
        }
    };
    if let Some(name) = name {
        return monitors
            .iter()
            .find(|m| m["name"].as_str() == Some(name))
            .map(rect)
            .ok_or_else(|| format!("no output is named {name:?}"));
    }
    monitors
        .iter()
        .map(rect)
        .reduce(|a, b| {
            let (x, y) = (a.x.min(b.x), a.y.min(b.y));
            Rect {
                x,
                y,
                w: (a.x + a.w).max(b.x + b.w) - x,
                h: (a.y + a.h).max(b.y + b.h) - y,
            }
        })
        .ok_or_else(|| "hyprctl lists no monitors".to_owned())
}

/// Image pixel `point` of an `image`-sized frame showing `frame`, as whole
/// layout coordinates, clamped to the frame.
#[expect(
    clippy::cast_possible_truncation,
    reason = "layout coordinates are far inside i64"
)]
pub(super) fn to_layout(
    point: (u32, u32),
    image: (u32, u32),
    frame: Rect,
) -> Result<(i64, i64), String> {
    to_screen(image, frame, point.0, point.1)
        .map(|(x, y)| (x.floor() as i64, y.floor() as i64))
        .ok_or_else(|| "no frame has been published yet".to_owned())
}

/// The commands that carry out `input`, in order, or why none can here;
/// `shift` when the window that takes a chord is a terminal.
pub(super) fn plan(
    input: &DocumentInput,
    image: (u32, u32),
    place: Option<&Place>,
    tools: Tools,
    shift: bool,
) -> Result<Vec<Vec<String>>, String> {
    let argv = |words: &[&str]| words.iter().map(|&w| w.to_owned()).collect::<Vec<_>>();
    let mut out = Vec::new();
    out.extend(place.and_then(|p| p.window.as_deref()).map(focus));
    match input {
        // The clipboard is the mirror's (`clipboard`); only chords are input.
        DocumentInput::Navigate { .. }
        | DocumentInput::CopyClipboard { .. }
        | DocumentInput::ClipboardText { .. }
        | DocumentInput::Activate { .. }
        | DocumentInput::Paste { chord: false, .. } => return Ok(Vec::new()),
        DocumentInput::Paste { .. }
        | DocumentInput::SelectAll
        | DocumentInput::CopySelection { .. } => {
            out.push(chord_argv(tools, chord_letter(input, shift).ok_or(NO_SELECT_ALL)?, shift)?);
        }
        DocumentInput::Drag { from, to } => {
            let place =
                place.ok_or("a drag needs Hyprland (hyprctl) to place the pointer; run under Hyprland")?;
            if !tools.ydotool {
                return Err("nothing here can drag: the compositor offers no virtual pointer; install ydotool with ydotoold running".into());
            }
            out.push(movecursor(to_layout(*from, image, place.frame)?));
            out.push(argv(&["ydotool", "click", "0x40"]));
            out.push(movecursor(to_layout(*to, image, place.frame)?));
            out.push(argv(&["ydotool", "click", "0x80"]));
        }
        DocumentInput::Click { x, y } => {
            let place =
                place.ok_or("a click needs Hyprland (hyprctl) to place the pointer; run under Hyprland")?;
            let press = if tools.wlrctl {
                argv(&["wlrctl", "pointer", "click", "left"])
            } else if tools.ydotool {
                argv(&["ydotool", "click", "0xC0"])
            } else {
                return Err("nothing here can click: install wlrctl, or ydotool with ydotoold running".into());
            };
            out.push(movecursor(to_layout((*x, *y), image, place.frame)?));
            out.push(press);
        }
        DocumentInput::Scroll { dx, dy } => {
            // Surface pixels to layout pixels, on the frame's own ratio.
            let ratio = place.map_or(1.0, |p| p.frame.w / f64::from(image.0.max(1)));
            let (dx, dy) = (f64::from(*dx) * ratio, f64::from(*dy) * ratio);
            // A window scrolls under the pointer, so put it there.
            if let Some(place) = place.filter(|p| p.window.is_some()) {
                let f = place.frame;
                out.push(movecursor(to_layout((1, 1), (2, 2), f)?));
            }
            if tools.wlrctl {
                out.push(argv(&[
                    "wlrctl",
                    "pointer",
                    "scroll",
                    &format!("{dy:.0}"),
                    &format!("{dx:.0}"),
                ]));
            } else if tools.ydotool {
                // ponytail: one wheel notch per 50px and evdev's up-positive
                // wheel; tune once tried against a real ydotoold.
                let notches = |d: f64| {
                    if d == 0.0 {
                        0.0
                    } else {
                        (d / 50.0).round().abs().max(1.0).copysign(d)
                    }
                };
                out.push(argv(&[
                    "ydotool",
                    "mousemove",
                    "--wheel",
                    "-x",
                    &format!("{:.0}", notches(dx)),
                    "-y",
                    &format!("{:.0}", -notches(dy)),
                ]));
            } else {
                return Err("nothing here can scroll: install wlrctl, or ydotool with ydotoold running".into());
            }
        }
        DocumentInput::Type { text } => out.push(if tools.wtype {
            argv(&["wtype", "--", text])
        } else if tools.wlrctl {
            argv(&["wlrctl", "keyboard", "type", text])
        } else if tools.ydotool {
            argv(&["ydotool", "type", "--", text])
        } else {
            return Err("nothing here can type: install wtype (or wlrctl, or ydotool with ydotoold running)".into());
        }),
        DocumentInput::Key { key } => out.push(if tools.wtype {
            argv(&["wtype", "-k", keysym(*key)])
        } else if tools.ydotool {
            let code = evdev(*key);
            argv(&["ydotool", "key", &format!("{code}:1"), &format!("{code}:0")])
        } else {
            return Err("nothing here can press keys: install wtype, or ydotool with ydotoold running".into());
        }),
    }
    Ok(out)
}

/// The command that puts the pointer at layout point `(x, y)`.
fn movecursor((x, y): (i64, i64)) -> Vec<String> {
    let (x, y) = (x.to_string(), y.to_string());
    ["hyprctl", "dispatch", "movecursor", &x, &y]
        .map(str::to_owned)
        .to_vec()
}

/// Why a terminal skips a select-all.
const NO_SELECT_ALL: &str = "a terminal has no select-all; skipped";

/// The command that presses Ctrl and `letter`, with `shift` Ctrl+Shift.
fn chord_argv(tools: Tools, letter: char, shift: bool) -> Result<Vec<String>, String> {
    let (l, code) = (
        letter.to_string(),
        super::surface_input::letter_code(letter),
    );
    let words: Vec<String> = if tools.wtype {
        let (hold, free): (&[&str], &[&str]) = if shift {
            (
                &["-M", "ctrl", "-M", "shift"],
                &["-m", "shift", "-m", "ctrl"],
            )
        } else {
            (&["-M", "ctrl"], &["-m", "ctrl"])
        };
        let mut words = vec!["wtype".to_owned()];
        words.extend(hold.iter().map(|&w| w.to_owned()));
        words.push(l);
        words.extend(free.iter().map(|&w| w.to_owned()));
        words
    } else if tools.ydotool {
        let (down, up) = (format!("{code}:1"), format!("{code}:0"));
        let keys: &[&str] = if shift {
            &["29:1", "42:1", &down, &up, "42:0", "29:0"]
        } else {
            &["29:1", &down, &up, "29:0"]
        };
        ["ydotool", "key"]
            .iter()
            .chain(keys)
            .map(|&w| w.to_owned())
            .collect()
    } else {
        return Err("nothing here can press the chord: the compositor offers no virtual keyboard; install wtype, or ydotool with ydotoold running".into());
    };
    Ok(words)
}

/// The XKB keysym `wtype -k` names, and the native keymap binds.
pub(super) const fn keysym(key: DocumentKey) -> &'static str {
    match key {
        DocumentKey::Enter => "Return",
        DocumentKey::Tab => "Tab",
        DocumentKey::Backspace => "BackSpace",
        DocumentKey::Escape => "Escape",
        DocumentKey::ArrowLeft => "Left",
        DocumentKey::ArrowUp => "Up",
        DocumentKey::ArrowRight => "Right",
        DocumentKey::ArrowDown => "Down",
    }
}

/// The Linux input event code `ydotool key` presses.
const fn evdev(key: DocumentKey) -> u16 {
    match key {
        DocumentKey::Enter => 28,
        DocumentKey::Tab => 15,
        DocumentKey::Backspace => 14,
        DocumentKey::Escape => 1,
        DocumentKey::ArrowLeft => 105,
        DocumentKey::ArrowUp => 103,
        DocumentKey::ArrowRight => 106,
        DocumentKey::ArrowDown => 108,
    }
}

/// `argv` as a shell would read it, for the dry-run log.
fn shown(argv: &[String]) -> String {
    let word = |w: &String| {
        if !w.is_empty()
            && w.bytes()
                .all(|b| b.is_ascii_graphic() && b != b'\'' && b != b'"')
        {
            w.clone()
        } else {
            format!("'{}'", w.replace('\'', r"'\''"))
        }
    };
    argv.iter().map(word).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[path = "surface_grim_input_tests.rs"]
mod tests;
