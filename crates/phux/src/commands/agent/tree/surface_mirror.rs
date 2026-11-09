//! `phux agent surface mirror WINDOW`: capture a window or a display and
//! publish the image whenever it changes.
//!
//! On macOS the system's `screencapture` writes a PNG; JPEG frames and
//! `--max-width` re-encode it with the system's `sips`. The capture needs the
//! Screen Recording grant, which macOS ties to the application that launched
//! this process (the terminal). Without it, macOS
//! hands over an image with no window content rather than an error.
//!
//! Viewers' input intents (`phux agent page click|type|key`, or any `SEND_DOCUMENT_INPUT`) are
//! posted as input events on macOS (`surface_input`) and run through
//! `hyprctl`, `wlrctl`, `wtype` or `ydotool` on Linux
//! (`surface_grim_input`); `--dry-run-input` logs them instead. While input
//! arrives the host shows who is in control (`surface_indicator`).
//!
//! On Linux, `grim` captures from a wlroots compositor (Hyprland, Sway); see
//! `surface_grim`.

use std::path::PathBuf;
use std::process::ExitCode;

/// How the mirror was asked to run.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    expect(
        dead_code,
        reason = "only the macOS and Linux producers read the arguments"
    )
)]
pub(super) struct MirrorArgs<'a> {
    /// The window, as `App` or `App:title fragment`; `display` for the main
    /// display (on Linux every output, or `display:NAME` for one).
    pub(super) window: &'a str,
    /// The CoreGraphics window id, when known.
    pub(super) window_id: Option<u32>,
    /// The pane the surface binds to; this process's own pane when absent.
    pub(super) pane: Option<&'a str>,
    /// Milliseconds between captures.
    pub(super) interval_ms: u64,
    /// How each capture is encoded.
    pub(super) frames: super::surface::Frames,
    /// Log the input events viewers' intents would post instead of posting.
    pub(super) dry_run_input: bool,
    pub(super) json: bool,
    pub(super) socket: Option<PathBuf>,
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[expect(
    clippy::needless_pass_by_value,
    reason = "the stub keeps the producers' signature"
)]
pub(super) fn run_mirror(args: MirrorArgs<'_>) -> ExitCode {
    use crate::commands::json_err;
    use crate::exit_codes::EXIT_USAGE;
    json_err::emit(
        args.json,
        &json_err::CliError::new(
            json_err::codes::USAGE_ERROR,
            "`phux agent surface mirror` captures on macOS or a wlroots Wayland compositor; this build is neither",
            "run the mirror on the machine that shows the window",
        ),
        EXIT_USAGE,
    )
}

/// What input to the mirror controls: `display` or `display:OUTPUT` is a
/// whole screen, anything else one window
/// (`docs/resource-kinds.md#authority`).
fn control_class(window: &str, window_id: Option<u32>) -> phux_protocol::wire::frame::ControlClass {
    use phux_protocol::wire::frame::ControlClass;
    let display = window == "display" || window.starts_with("display:");
    if window_id.is_none() && display {
        ControlClass::Display
    } else {
        ControlClass::Window
    }
}

#[cfg(target_os = "linux")]
use super::surface_grim as platform;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn run_mirror(args: MirrorArgs<'_>) -> ExitCode {
    use phux_client::tree::{self, Kind};
    use phux_server::runtime::default_socket_path;
    use std::time::Duration;

    use crate::commands::agent::tree::{dial, mirror_pane, report};
    use crate::commands::{cli_runtime, json_err};
    use crate::exit_codes::EXIT_USAGE;

    let MirrorArgs {
        window,
        window_id,
        pane,
        interval_ms,
        frames,
        dry_run_input,
        json,
        socket,
    } = args;
    let target = match platform::target(window, window_id) {
        Ok(target) => target,
        Err(message) => {
            return json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::NO_SUCH_TARGET,
                    message,
                    platform::TARGET_HINT,
                ),
                EXIT_USAGE,
            );
        }
    };
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let socket_path = socket.unwrap_or_else(default_socket_path);
        let pane = match mirror_pane(&socket_path, pane, "agent surface mirror", json).await {
            Ok(pane) => pane,
            Err(code) => return code,
        };
        let label = format!("{target:?}");
        #[cfg(target_os = "macos")]
        let mut injector = super::surface_input::Injector::new(
            match target {
                macos::Target::Window(id) => Some(id),
                macos::Target::MainDisplay => None,
            },
            dry_run_input,
        );
        #[cfg(target_os = "macos")]
        let act = move |input: &tree::DocumentInput, frame| injector.act(input, frame);
        #[cfg(target_os = "linux")]
        let act = platform::injector(&target, dry_run_input);
        let mut platform_capture = platform::capturer(target, frames);
        let mut faked = false;
        // A dry run goes on without a capture grant, on a blank frame.
        let mut capture = move || match platform_capture() {
            Err(message) if dry_run_input => {
                if !std::mem::replace(&mut faked, true) {
                    eprintln!("dry-run: capture failed ({message}); publishing a blank frame");
                }
                Ok(super::surface_input::DRY_RUN_FRAME.to_vec())
            }
            other => other,
        };
        // A refused capture must not leave an empty surface behind.
        let first = match capture() {
            Ok(bytes) => bytes,
            Err(message) => return refused(json, &message),
        };
        let mut conn = match dial(&socket_path, json).await {
            Ok(conn) => conn,
            Err(code) => return code,
        };
        let control = Some(control_class(window, window_id));
        let opened = match tree::open_kind(&mut conn, Kind::Surface, &pane, window, control).await {
            Ok(opened) => opened,
            Err(err) => return report(json, &err),
        };
        let resource = phux_client::selector::format_terminal_id(&opened.resource);
        if json {
            outln!(
                "{}",
                serde_json::json!({ "schema_version": 1, "resource": resource, "target": label })
            );
        } else {
            outln!("{resource}");
        }
        let display = control == Some(phux_protocol::wire::frame::ControlClass::Display);
        let indicator = super::surface_indicator::Indicator::new(display, dry_run_input);
        let clipboard = super::clipboard::Clipboard::host(dry_run_input);
        publish_until_gone(
            &mut conn,
            &socket_path,
            &opened.resource,
            (capture, act, indicator, &clipboard),
            first,
            Duration::from_millis(interval_ms.max(100)),
            json,
        )
        .await
    })
}

/// Publish every changed capture until Ctrl-C, and `act` on every intent
/// viewers send between captures with the newest frame's size. Input brings
/// captures forward (`surface_cadence`). A capture that gives no frame (the
/// display asleep, the session locked) keeps the surface and its last frame
/// and backs off; a lost connection (a server upgrade) is redialed and the
/// last frame published again, which re-adopts the same surface.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[allow(
    clippy::future_not_send,
    reason = "the CLI runtime is single-threaded; the connection is not shared"
)]
async fn publish_until_gone(
    conn: &mut phux_client::attach::connection::Connection,
    socket: &std::path::Path,
    resource: &phux_protocol::ids::ResourceId,
    (mut capture, mut act, mut indicator, clipboard): (
        impl FnMut() -> Result<Vec<u8>, String>,
        impl FnMut(&phux_client::tree::DocumentInput, (u32, u32)),
        super::surface_indicator::Indicator,
        &super::clipboard::Clipboard,
    ),
    first: Vec<u8>,
    interval: std::time::Duration,
    json: bool,
) -> ExitCode {
    use super::redial;
    use crate::commands::agent::tree::report;
    use phux_client::tree;

    let size = |image: &[u8]| tree::image_size(image).map_or((0, 0), |(_, w, h)| (w, h));
    let mut intents = Vec::new();
    let mut frame = size(&first);
    if let Err(err) = redial::publish(conn, socket, resource, &first, &mut intents).await {
        return report(json, &err);
    }
    let mut last = first;
    let mut misses: u32 = 0;
    let mut input = None;
    let mut origin = super::surface_indicator::UNNAMED.to_owned();
    loop {
        match capture() {
            Ok(bytes) => {
                if misses > 0 {
                    eprintln!("capturing again after {misses} empty captures");
                }
                misses = 0;
                frame = size(&bytes);
                if last != bytes {
                    if let Err(err) =
                        redial::publish(conn, socket, resource, &bytes, &mut intents).await
                    {
                        return report(json, &err);
                    }
                    last = bytes;
                }
            }
            Err(message) => {
                if misses == 0 {
                    eprintln!("{message}; keeping the last frame and retrying");
                }
                misses = misses.saturating_add(1);
            }
        }
        let captured = std::time::Instant::now();
        let idle = super::surface_cadence::backoff(interval, misses);
        loop {
            for intent in std::mem::take(&mut intents) {
                if intent.resource != *resource {
                    continue;
                }
                clipboard
                    .carry_out(conn, &intent, &mut intents, |input| act(input, frame))
                    .await;
                input = Some(std::time::Instant::now());
                indicator.input(&origin).await;
            }
            indicator.tick().await;
            let next = super::surface_cadence::next_capture(captured, input, idle);
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
                () = tokio::time::sleep_until(next.into()) => break,
                received = conn.recv() => match received {
                    Ok(received) => {
                        if let Some(by) = tree::activity(&received, resource) {
                            origin = by.to_owned();
                        }
                        intents.extend(tree::intent(&received));
                    }
                    Err(err) if err.is_transport_loss() => {
                        let Some(redialed) = redial::redial(socket).await else {
                            return report(json, &err.into());
                        };
                        *conn = redialed;
                        if let Err(err) =
                            redial::publish(conn, socket, resource, &last, &mut intents).await
                        {
                            return report(json, &err);
                        }
                    }
                    Err(err) => return report(json, &err.into()),
                },
            }
        }
    }
}

/// A capture the platform refused or that failed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn refused(json: bool, message: &str) -> ExitCode {
    use crate::commands::json_err;
    json_err::emit(
        json,
        &json_err::CliError::new(json_err::codes::WORKLOAD, message, platform::REFUSED_HINT),
        crate::exit_codes::EXIT_FAILURE,
    )
}

#[cfg(target_os = "macos")]
mod macos {
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

    pub(super) fn capturer(
        target: Target,
        frames: Frames,
    ) -> impl FnMut() -> Result<Vec<u8>, String> {
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
        let output = Command::new("osascript")
            .args(["-l", "JavaScript", "-e", WINDOW_LIST])
            .output()
            .map_err(|err| format!("could not list windows: {err}"))?;
        pick_window(spec, &String::from_utf8_lossy(&output.stdout))
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
        let png =
            std::fs::read(&path).map_err(|err| format!("could not read the capture: {err}"))?;
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
}

#[cfg(test)]
mod control_tests {
    use phux_protocol::wire::frame::ControlClass::{Display, Window};

    #[test]
    fn a_display_mirror_controls_the_display_and_anything_else_one_window() {
        assert_eq!(super::control_class("display", None), Display);
        assert_eq!(super::control_class("display:DP-1", None), Display);
        assert_eq!(super::control_class("Safari", None), Window);
        assert_eq!(super::control_class("displayport:x", None), Window);
        assert_eq!(super::control_class("display", Some(7)), Window);
    }
}
