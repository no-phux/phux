//! Input for the Linux surface mirror: a viewer's intents (`phux agent page
//! click|type|key|scroll`) carried out on the desktop through command-line
//! tools that speak the wlroots virtual-pointer and virtual-keyboard
//! protocols, so no `unsafe` and no Wayland bindings.
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
use super::surface_input::to_screen;

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
}

/// Carry out each intent on the desktop `target` shows, or with `dry_run`
/// only log the commands. A failure is reported and skipped; it never ends
/// the mirror.
pub(super) fn injector(
    target: &Target,
    dry_run: bool,
) -> impl FnMut(&DocumentInput, (u32, u32)) + use<> {
    let env = session_env();
    let tools = Tools::detect();
    if dry_run {
        eprintln!("dry-run: input tools on PATH: {tools:?}");
    }
    let injector = Injector {
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
            return;
        }
        if let Err(message) = injector.act(input, image) {
            eprintln!("phux: input skipped: {message}");
        }
    }
}

struct Injector {
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
            target,
            env,
            tools,
            hyprland,
            dry_run,
        } = self;
        let pointer = matches!(
            input,
            DocumentInput::Click { .. } | DocumentInput::Scroll { .. }
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
        for argv in plan(input, image, place.as_ref(), *tools)? {
            if *dry_run {
                eprintln!("dry-run: would run: {}", shown(&argv));
                continue;
            }
            let mut command = Command::new(&argv[0]);
            command.args(&argv[1..]).envs(env.iter().cloned());
            run(command, "the input tool did not finish")?;
        }
        Ok(())
    }
}

/// `target`'s rectangle now (a window may have moved since the frame).
fn place(target: &Target, env: &[(&str, String)]) -> Result<Place, String> {
    let monitors = hyprctl("monitors", env)?;
    match target {
        Target::Display(name) => Ok(Place {
            frame: monitors_rect(&monitors, name.as_deref())?,
            window: None,
        }),
        Target::Window(spec) => {
            let window = find_window(spec, &hyprctl("clients", env)?, &monitors)?;
            Ok(Place {
                frame: window.rect,
                window: Some(window.address),
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
fn to_layout(point: (u32, u32), image: (u32, u32), frame: Rect) -> Result<(i64, i64), String> {
    to_screen(image, frame, point.0, point.1)
        .map(|(x, y)| (x.floor() as i64, y.floor() as i64))
        .ok_or_else(|| "no frame has been published yet".to_owned())
}

/// The commands that carry out `input`, in order, or why none can here.
pub(super) fn plan(
    input: &DocumentInput,
    image: (u32, u32),
    place: Option<&Place>,
    tools: Tools,
) -> Result<Vec<Vec<String>>, String> {
    let argv = |words: &[&str]| words.iter().map(|&w| w.to_owned()).collect::<Vec<_>>();
    let mut out = Vec::new();
    if let Some(address) = place.and_then(|p| p.window.as_deref()) {
        out.push(argv(&[
            "hyprctl",
            "dispatch",
            "focuswindow",
            &format!("address:{address}"),
        ]));
    }
    let movecursor = |(x, y): (i64, i64)| {
        argv(&[
            "hyprctl",
            "dispatch",
            "movecursor",
            &x.to_string(),
            &y.to_string(),
        ])
    };
    match input {
        DocumentInput::Navigate { .. } => return Ok(Vec::new()),
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

/// The XKB keysym `wtype -k` names.
const fn keysym(key: DocumentKey) -> &'static str {
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
mod tests {
    use super::*;

    /// A 4K panel at 2x left of a 1080p panel at 1x rotated upright, as
    /// `hyprctl -j monitors` lists them.
    const MONITORS: &str = r#"[
        {"name":"DP-1","x":0,"y":0,"width":3840,"height":2160,"scale":2.0,"transform":0},
        {"name":"HDMI-A-1","x":1920,"y":0,"width":1920,"height":1080,"scale":1.0,"transform":1}
    ]"#;

    const fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn outputs_are_logical_rectangles_and_display_is_their_union() {
        assert_eq!(
            monitors_rect(MONITORS, Some("DP-1")),
            Ok(rect(0.0, 0.0, 1920.0, 1080.0))
        );
        assert_eq!(
            monitors_rect(MONITORS, Some("HDMI-A-1")),
            Ok(rect(1920.0, 0.0, 1080.0, 1920.0))
        );
        assert_eq!(
            monitors_rect(MONITORS, None),
            Ok(rect(0.0, 0.0, 3000.0, 1920.0))
        );
        assert!(monitors_rect(MONITORS, Some("eDP-1")).is_err());
    }

    #[test]
    fn image_pixels_map_to_layout_points_and_clamp_to_the_frame() {
        // grim renders `display` at the highest scale: 6000x3840 for 3000x1920.
        let all = rect(0.0, 0.0, 3000.0, 1920.0);
        assert_eq!(to_layout((4000, 200), (6000, 3840), all), Ok((2000, 100)));
        // The same layout under `--max-width 1500`: a quarter-size frame.
        assert_eq!(to_layout((1000, 50), (1500, 960), all), Ok((2000, 100)));
        // One output: its origin is added.
        let hdmi = rect(1920.0, 0.0, 1080.0, 1920.0);
        assert_eq!(to_layout((10, 20), (1080, 1920), hdmi), Ok((1930, 20)));
        // A window at (12,34), captured natively on a 2x output.
        let window = rect(12.0, 34.0, 800.0, 600.0);
        assert_eq!(to_layout((100, 50), (1600, 1200), window), Ok((62, 59)));
        // Outside the image lands on the far edge, still inside the window.
        assert_eq!(
            to_layout((9999, 9999), (1600, 1200), window),
            Ok((811, 633))
        );
        assert!(to_layout((0, 0), (0, 0), window).is_err());
    }

    const ALL: Tools = Tools {
        hyprctl: true,
        wlrctl: true,
        ydotool: true,
        wtype: true,
    };
    const YDOTOOL: Tools = Tools {
        hyprctl: true,
        wlrctl: false,
        ydotool: true,
        wtype: false,
    };

    fn cmds(
        input: &DocumentInput,
        place: Option<&Place>,
        tools: Tools,
    ) -> Result<Vec<String>, String> {
        plan(input, (1600, 1200), place, tools).map(|out| out.iter().map(|a| shown(a)).collect())
    }

    #[test]
    fn a_window_click_focuses_moves_then_presses() {
        let place = Place {
            frame: rect(12.0, 34.0, 800.0, 600.0),
            window: Some("0xa1".into()),
        };
        let click = DocumentInput::Click { x: 100, y: 50 };
        assert_eq!(
            cmds(&click, Some(&place), ALL).unwrap(),
            [
                "hyprctl dispatch focuswindow address:0xa1",
                "hyprctl dispatch movecursor 62 59",
                "wlrctl pointer click left",
            ]
        );
        assert_eq!(
            cmds(&click, Some(&place), YDOTOOL).unwrap()[2],
            "ydotool click 0xC0"
        );
        let none = Tools {
            hyprctl: true,
            ..Tools::default()
        };
        assert!(
            cmds(&click, Some(&place), none)
                .unwrap_err()
                .contains("install wlrctl")
        );
        assert!(cmds(&click, None, ALL).unwrap_err().contains("Hyprland"));
    }

    #[test]
    fn keys_text_and_scroll_pick_the_first_tool_present() {
        let display = Place {
            frame: rect(0.0, 0.0, 800.0, 600.0),
            window: None,
        };
        let enter = DocumentInput::Key {
            key: DocumentKey::Enter,
        };
        assert_eq!(cmds(&enter, None, ALL).unwrap(), ["wtype -k Return"]);
        assert_eq!(
            cmds(&enter, None, YDOTOOL).unwrap(),
            ["ydotool key 28:1 28:0"]
        );
        let wlrctl_only = Tools {
            wlrctl: true,
            ..Tools::default()
        };
        assert!(
            cmds(&enter, None, wlrctl_only)
                .unwrap_err()
                .contains("install wtype")
        );
        let text = DocumentInput::Type {
            text: "it's -x".into(),
        };
        assert_eq!(cmds(&text, None, ALL).unwrap(), [r"wtype -- 'it'\''s -x'"]);
        assert_eq!(
            cmds(&text, None, wlrctl_only).unwrap(),
            [r"wlrctl keyboard type 'it'\''s -x'"]
        );
        // A 2x frame halves the delta; a display scrolls where the pointer is.
        let scroll = DocumentInput::Scroll { dx: 0, dy: 200 };
        assert_eq!(
            cmds(&scroll, Some(&display), ALL).unwrap(),
            ["wlrctl pointer scroll 100 0"]
        );
        assert_eq!(
            cmds(&scroll, Some(&display), YDOTOOL).unwrap(),
            ["ydotool mousemove --wheel -x 0 -y -2"]
        );
        let window = Place {
            window: Some("0xb2".into()),
            ..display
        };
        assert_eq!(
            cmds(&scroll, Some(&window), ALL).unwrap()[1],
            "hyprctl dispatch movecursor 400 300"
        );
        let nav = DocumentInput::Navigate {
            url: "https://phux.sh/".into(),
        };
        assert_eq!(
            cmds(&nav, Some(&window), ALL).unwrap(),
            Vec::<String>::new()
        );
    }
}
