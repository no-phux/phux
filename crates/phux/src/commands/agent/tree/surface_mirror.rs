//! `phux agent surface mirror WINDOW`: the macOS producer. It captures a
//! window (or the main display) with the system's `screencapture` and
//! publishes the PNG whenever it changes.
//!
//! The capture needs the Screen Recording grant, which macOS ties to the
//! application that launched this process (the terminal, or the phux
//! desktop app). Without it, macOS hands over an image with no window
//! content rather than an error.

use std::path::PathBuf;
use std::process::ExitCode;

/// How the mirror was asked to run.
pub(super) struct MirrorArgs<'a> {
    /// The window, as `App` or `App:title fragment`; `display` for the main
    /// display.
    pub(super) window: &'a str,
    /// The CoreGraphics window id, when known.
    pub(super) window_id: Option<u32>,
    /// The pane the surface binds to; this process's own pane when absent.
    pub(super) pane: Option<&'a str>,
    /// Milliseconds between captures.
    pub(super) interval_ms: u64,
    pub(super) json: bool,
    pub(super) socket: Option<PathBuf>,
}

#[cfg(not(target_os = "macos"))]
#[cfg_attr(
    not(target_os = "macos"),
    expect(dead_code, reason = "only the macOS producer reads the arguments")
)]
pub(super) fn run_mirror(args: MirrorArgs<'_>) -> ExitCode {
    use crate::commands::json_err;
    use crate::exit_codes::EXIT_USAGE;
    json_err::emit(
        args.json,
        &json_err::CliError::new(
            json_err::codes::USAGE_ERROR,
            "`phux agent surface mirror` captures with macOS's screencapture; this build is not macOS",
            "run the mirror on the Mac that shows the window",
        ),
        EXIT_USAGE,
    )
}

#[cfg(target_os = "macos")]
pub(super) fn run_mirror(args: MirrorArgs<'_>) -> ExitCode {
    macos::run(args)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::process::{Command, ExitCode};
    use std::time::Duration;

    use phux_client::tree::{self, Kind};
    use phux_protocol::ids::ResourceId;
    use phux_server::runtime::default_socket_path;

    use super::MirrorArgs;
    use crate::commands::agent::tree::{dial, report, resolve};
    use crate::commands::{cli_runtime, json_err};
    use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

    /// What `screencapture` is pointed at.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Target {
        Window(u32),
        MainDisplay,
    }

    pub(super) fn run(args: MirrorArgs<'_>) -> ExitCode {
        let MirrorArgs {
            window,
            window_id,
            pane,
            interval_ms,
            json,
            socket,
        } = args;
        let target = match (window_id, window) {
            (Some(id), _) => Ok(Target::Window(id)),
            (None, "display") => Ok(Target::MainDisplay),
            (None, name) => window_by_name(name).map(Target::Window),
        };
        let target = match target {
            Ok(target) => target,
            Err(message) => {
                return json_err::emit(
                    json,
                    &json_err::CliError::new(
                        json_err::codes::NO_SUCH_TARGET,
                        message,
                        "name a visible window as `App` or `App:title`, pass `--window-id`, or `display`",
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
            let pane = match pane {
                Some(selector) => {
                    match resolve(&socket_path, selector, "agent surface mirror", json).await {
                        Ok(pane) => pane,
                        Err(code) => return code,
                    }
                }
                None => match own_pane() {
                    Some(pane) => pane,
                    None => {
                        return json_err::emit(
                            json,
                            &json_err::CliError::new(
                                json_err::codes::USAGE_ERROR,
                                "not running in a phux pane and no `--pane` given",
                                "run the mirror inside a pane, or pass `--pane @N`",
                            ),
                            EXIT_USAGE,
                        );
                    }
                },
            };
            let mut conn = match dial(&socket_path, json).await {
                Ok(conn) => conn,
                Err(code) => return code,
            };
            let opened = match tree::open_kind(&mut conn, Kind::Surface, &pane, window).await {
                Ok(opened) => opened,
                Err(err) => return report(json, &err),
            };
            let resource = phux_client::selector::format_terminal_id(&opened.resource);
            if json {
                outln!(
                    "{}",
                    serde_json::json!({ "schema_version": 1, "resource": resource, "target": format!("{target:?}") })
                );
            } else {
                outln!("{resource}");
            }
            let interval = Duration::from_millis(interval_ms.max(100));
            publish_until_gone(&mut conn, &opened.resource, target, interval, json).await
        })
    }

    /// Publish every changed capture until Ctrl-C, or until three captures
    /// in a row fail.
    #[allow(
        clippy::future_not_send,
        reason = "the CLI runtime is single-threaded; the connection is not shared"
    )]
    async fn publish_until_gone(
        conn: &mut phux_client::attach::connection::Connection,
        resource: &ResourceId,
        target: Target,
        interval: Duration,
        json: bool,
    ) -> ExitCode {
        let mut last: Option<Vec<u8>> = None;
        let mut misses = 0;
        loop {
            match capture(target) {
                Ok(bytes) => {
                    misses = 0;
                    if last.as_deref() != Some(bytes.as_slice()) {
                        if let Err(err) = tree::publish(conn, resource, bytes.clone()).await {
                            return report(json, &err);
                        }
                        last = Some(bytes);
                    }
                }
                Err(message) => {
                    misses += 1;
                    if misses >= 3 {
                        return json_err::emit(
                            json,
                            &json_err::CliError::new(
                                json_err::codes::WORKLOAD,
                                format!("capture failed three times: {message}"),
                                "the window may be gone; grant Screen Recording to the terminal (or phux) in System Settings if images are blank",
                            ),
                            EXIT_FAILURE,
                        );
                    }
                }
            }
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
                () = tokio::time::sleep(interval) => {}
            }
        }
    }

    /// The pane this process runs in, from the injected `PHUX_TERMINAL_ID`.
    fn own_pane() -> Option<ResourceId> {
        let raw = std::env::var("PHUX_TERMINAL_ID").ok()?;
        raw.parse::<u32>().ok().map(ResourceId::local)
    }

    /// The on-screen window list as JSON, through the system's JavaScript
    /// automation bridge to CoreGraphics: no bindings, no `unsafe`.
    const WINDOW_LIST: &str = r#"ObjC.import("CoreGraphics");
const list = $.CGWindowListCopyWindowInfo($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, 0);
const windows = ObjC.deepUnwrap($.CFBridgingRelease(list)) || [];
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

    /// One PNG of `target`, through `screencapture`.
    fn capture(target: Target) -> Result<Vec<u8>, String> {
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
        std::fs::read(&path).map_err(|err| format!("could not read the capture: {err}"))
    }

    #[cfg(test)]
    mod tests {
        use super::pick_window;

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
