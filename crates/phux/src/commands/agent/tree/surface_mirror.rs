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
        let mut platform_capture = capturer(target, frames);
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

/// The platform capturer, switching to JPEG for good once a PNG frame is
/// over the publish limit (a big or many-output display), which the server
/// would refuse.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn capturer(
    target: platform::Target,
    frames: super::surface::Frames,
) -> impl FnMut() -> Result<Vec<u8>, String> {
    #[allow(
        clippy::clone_on_copy,
        reason = "the macOS target is Copy, the Linux one is not"
    )]
    let mut jpeg_target = frames.jpeg.is_none().then(|| target.clone());
    let mut capture = platform::capturer(target, frames);
    move || {
        let frame = capture()?;
        if frame.len() <= phux_protocol::wire::frame::MAX_PUBLISH_BYTES {
            return Ok(frame);
        }
        let Some(target) = jpeg_target.take() else {
            return Ok(frame);
        };
        eprintln!(
            "a PNG frame is over the publish limit; sending JPEG frames instead \
             (pass --format jpeg, and --max-width if JPEG is still too big)"
        );
        let jpeg = super::surface::Frames {
            jpeg: Some(70),
            ..frames
        };
        capture = platform::capturer(target, jpeg);
        capture()
    }
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
        impl FnMut(&phux_client::tree::DocumentInput, (u32, u32)) -> Result<(), String>,
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
pub(super) mod macos;

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
