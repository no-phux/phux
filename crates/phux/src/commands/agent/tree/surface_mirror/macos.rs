//! The macOS window and display capture behind `agent surface mirror`.

use std::process::Command;

use crate::commands::agent::tree::surface::Frames;

/// What `screencapture` is pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target {
    Window(u32),
    MainDisplay,
}

pub(super) const TARGET_HINT: &str =
    "name a visible window as `App` or `App:title`, pass `--window-id`, or `display`";

/// macOS grants Screen Recording to the app that owns the process, and a
/// phux pane's processes belong to the server, which no grant reaches.
pub(super) const REFUSED_HINT: &str = "start the mirror from a terminal app such as Terminal, and grant that app Screen Recording; a phux pane cannot hold the grant";

pub(super) fn target(window: &str, window_id: Option<u32>) -> Result<Target, String> {
    match (window_id, window) {
        (Some(id), _) => Ok(Target::Window(id)),
        (None, "display") => Ok(Target::MainDisplay),
        (None, name) => window_by_name(name).map(Target::Window),
    }
}

pub(super) fn capturer(target: Target, frames: Frames) -> impl FnMut() -> Result<Vec<u8>, String> {
    // The last capture and its re-encoding: an unchanged screen skips `sips`.
    let mut last = None;
    move || capture(target, frames, &mut last)
}

/// The on-screen window list as JSON, through the system's JavaScript
/// automation bridge to CoreGraphics: no bindings, no `unsafe`.
const WINDOW_LIST: &str = r#"ObjC.import("CoreGraphics");
const list = $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, 0);
const windows = ObjC.deepUnwrap(ObjC.castRefToObject(list)) || [];
JSON.stringify(windows.map(w => ({ id: w.kCGWindowNumber, owner: w.kCGWindowOwnerName, name: w.kCGWindowName || "", layer: w.kCGWindowLayer })));"#;

/// The first normal-layer on-screen window of `App`, or of `App:title`
/// whose title contains `title`.
fn window_by_name(spec: &str) -> Result<u32, String> {
    pick_window(spec, &window_list()?)
}

/// The on-screen windows as `[{id, owner, name, layer}]` JSON. Without
/// Screen Recording access every `name` is empty.
pub(in crate::commands::agent::tree) fn window_list() -> Result<String, String> {
    let output = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", WINDOW_LIST])
        .output()
        .map_err(|err| format!("could not list windows: {err}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(super) fn pick_window(spec: &str, listing: &str) -> Result<u32, String> {
    let (owner, title) = spec.split_once(':').unwrap_or((spec, ""));
    let windows: Vec<serde_json::Value> = serde_json::from_str(listing.trim())
        .map_err(|err| format!("the window list was not readable: {err}"))?;
    windows
        .iter()
        .filter(|w| w["layer"].as_i64().unwrap_or(0) == 0)
        .filter(|w| {
            w["owner"]
                .as_str()
                .is_some_and(|o| o.eq_ignore_ascii_case(owner))
        })
        .find(|w| title.is_empty() || w["name"].as_str().is_some_and(|n| n.contains(title)))
        .and_then(|w| w["id"].as_u64())
        .and_then(|id| u32::try_from(id).ok())
        .ok_or_else(|| format!("no on-screen window matches {spec:?}"))
}

/// One image of `target`, through `screencapture`, then `sips` when
/// `frames` asks for JPEG or a narrower image.
fn capture(
    target: Target,
    frames: Frames,
    last: &mut Option<(Vec<u8>, Vec<u8>)>,
) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir().join(format!("phux-surface-{}", std::process::id()));
    std::fs::create_dir_all(&dir)
        .map_err(|err| format!("could not make {}: {err}", dir.display()))?;
    let path = dir.join("frame.png");
    let mut command = Command::new("/usr/sbin/screencapture");
    command.args(["-x", "-o", "-t", "png"]);
    if let Target::Window(id) = target {
        command.arg(format!("-l{id}"));
    }
    command.arg(&path);
    let status = command
        .status()
        .map_err(|err| format!("could not run screencapture: {err}"))?;
    if !status.success() {
        return Err(format!("screencapture exited with {status}"));
    }
    let png = std::fs::read(&path).map_err(|err| format!("could not read the capture: {err}"))?;
    let width = phux_client::tree::image_size(&png).map_or(0, |size| size.1);
    let Some(args) = sips_args(frames, width) else {
        return Ok(png);
    };
    if let Some((_, frame)) = last.as_ref().filter(|(raw, _)| *raw == png) {
        return Ok(frame.clone());
    }
    let out = dir.join("frame.out");
    let status = Command::new("/usr/bin/sips")
        .args(args)
        .arg(&path)
        .arg("--out")
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|err| format!("could not run sips: {err}"))?;
    if !status.success() {
        return Err(format!("sips exited with {status}"));
    }
    let frame = std::fs::read(&out)
        .map_err(|err| format!("could not read the re-encoded capture: {err}"))?;
    *last = Some((png, frame.clone()));
    Ok(frame)
}

/// The `sips` arguments that turn a `width`-pixel PNG into the frame
/// `frames` asks for, or `None` when the PNG already is that frame.
pub(super) fn sips_args(frames: Frames, width: u32) -> Option<Vec<String>> {
    let mut args = Vec::new();
    if let Some(quality) = frames.jpeg {
        args.extend(["-s", "format", "jpeg", "-s", "formatOptions"].map(String::from));
        args.push(quality.to_string());
    }
    if let Some(max) = frames.max_width.filter(|&max| max < width) {
        args.extend(["--resampleWidth".to_owned(), max.to_string()]);
    }
    (!args.is_empty()).then_some(args)
}

#[cfg(test)]
mod tests {
    use super::{Frames, pick_window, sips_args};

    #[test]
    fn sips_runs_only_for_jpeg_or_a_frame_wider_than_the_limit() {
        let frames = |jpeg, max_width| Frames { jpeg, max_width };
        assert_eq!(sips_args(frames(None, None), 1920), None);
        assert_eq!(sips_args(frames(None, Some(1920)), 1920), None);
        assert_eq!(
            sips_args(frames(None, Some(800)), 1920).unwrap(),
            ["--resampleWidth", "800"]
        );
        assert_eq!(
            sips_args(frames(Some(70), Some(800)), 640).unwrap(),
            ["-s", "format", "jpeg", "-s", "formatOptions", "70"]
        );
    }

    const LISTING: &str = r#"[{"id":7,"owner":"Finder","name":"Desktop","layer":0},{"id":9,"owner":"Safari","name":"Docs","layer":0},{"id":11,"owner":"Safari","name":"Menu","layer":25}]"#;

    #[test]
    fn an_app_names_its_first_normal_window_and_a_title_narrows_it() {
        assert_eq!(pick_window("safari", LISTING), Ok(9));
        assert_eq!(pick_window("Safari:Doc", LISTING), Ok(9));
        assert!(pick_window("Safari:Menu", LISTING).is_err());
        assert!(pick_window("Mail", LISTING).is_err());
    }
}
