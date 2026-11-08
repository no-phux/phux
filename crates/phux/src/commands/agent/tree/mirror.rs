//! `phux agent tree mirror APP`: read an application's accessibility tree
//! and publish the whole tree as one JSON document whenever it changes.
//! Pixels are never read.
//!
//! On macOS the tree comes from the Accessibility API. The process needs the
//! Accessibility grant, which macOS ties to the application that launched it
//! (the terminal). On Linux it comes from AT-SPI
//! over D-Bus (`mirror_atspi`), in the same JSON shape.

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
    /// The application: its process name (`pgrep -x`) on macOS, its AT-SPI
    /// name on Linux; unless `pid` is given.
    pub(super) app: &'a str,
    /// The application's process id, when known.
    pub(super) pid: Option<u32>,
    /// The pane the tree binds to; this process's own pane when absent.
    pub(super) pane: Option<&'a str>,
    /// Milliseconds between reads.
    pub(super) interval_ms: u64,
    /// Deepest nesting to read.
    pub(super) depth: usize,
    pub(super) json: bool,
    pub(super) socket: Option<PathBuf>,
}

/// The ceiling on nodes one document carries; a larger tree is cut off at
/// the deepest levels first, by the walk's budget.
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) const MAX_NODES: usize = 20_000;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[expect(
    clippy::needless_pass_by_value,
    reason = "one signature for every platform; the producers consume the arguments"
)]
pub(super) fn run_mirror(args: MirrorArgs<'_>) -> ExitCode {
    use crate::commands::json_err;
    use crate::exit_codes::EXIT_USAGE;
    json_err::emit(
        args.json,
        &json_err::CliError::new(
            json_err::codes::USAGE_ERROR,
            "`phux agent tree mirror` reads the macOS Accessibility API or Linux AT-SPI; this build is neither",
            "run the mirror on the machine that runs the application",
        ),
        EXIT_USAGE,
    )
}

#[cfg(target_os = "linux")]
use super::mirror_atspi::App;
#[cfg(target_os = "macos")]
use macos::App;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn run_mirror(args: MirrorArgs<'_>) -> ExitCode {
    use phux_client::tree;
    use phux_server::runtime::default_socket_path;
    use std::time::Duration;

    use crate::commands::agent::tree::{dial, mirror_pane, report};
    use crate::commands::cli_runtime;

    let MirrorArgs {
        app,
        pid,
        pane,
        interval_ms,
        depth,
        json,
        socket,
    } = args;
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let socket_path = socket.unwrap_or_else(default_socket_path);
        let pane = match mirror_pane(&socket_path, pane, "agent tree mirror", json).await {
            Ok(pane) => pane,
            Err(code) => return code,
        };
        // An application that cannot be read must not leave an empty tree.
        let source = match App::find(app, pid, json).await {
            Ok(source) => source,
            Err(code) => return code,
        };
        let mut conn = match dial(&socket_path, json).await {
            Ok(conn) => conn,
            Err(code) => return code,
        };
        let opened = match tree::open(&mut conn, &pane, app).await {
            Ok(opened) => opened,
            Err(err) => return report(json, &err),
        };
        let resource = phux_client::selector::format_terminal_id(&opened.resource);
        let pid = source.pid();
        if json {
            outln!(
                "{}",
                serde_json::json!({ "schema_version": 1, "resource": resource, "pid": pid, "app": app })
            );
        } else {
            outln!("{resource}");
        }
        let interval = Duration::from_millis(interval_ms.max(50));
        let label = format!("{app} (pid {pid})");
        match publish_until_gone(&source, depth, interval, &label, &mut conn, &opened.resource).await
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => report(json, &err),
        }
    })
}

/// Publish every change of the tree until Ctrl-C, or until the application
/// stops answering for three reads in a row.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[allow(
    clippy::future_not_send,
    reason = "an AXUIElement belongs to the thread that made it; the CLI runtime is single-threaded"
)]
async fn publish_until_gone(
    source: &App,
    depth: usize,
    interval: std::time::Duration,
    label: &str,
    conn: &mut phux_client::attach::connection::Connection,
    resource: &phux_protocol::ids::ResourceId,
) -> Result<(), phux_client::tree::TreeError> {
    let mut last: Option<Vec<u8>> = None;
    let mut misses = 0;
    loop {
        let document = source.read(depth).await;
        if document.is_null() {
            misses += 1;
            if misses >= 3 {
                outln!("{label} is gone; the tree keeps its last document");
                return Ok(());
            }
        } else {
            misses = 0;
            let bytes = serde_json::to_vec(&document).unwrap_or_default();
            if last.as_deref() != Some(bytes.as_slice()) {
                phux_client::tree::publish(conn, resource, bytes.clone()).await?;
                last = Some(bytes);
            }
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            () = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::process::ExitCode;

    use accessibility::{AXUIElement, AXUIElementAttributes as _};
    use core_foundation::base::CFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFString;

    use super::MAX_NODES;
    use crate::commands::json_err;
    use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

    /// A running application, read through the Accessibility API.
    pub(super) struct App {
        root: AXUIElement,
        pid: u32,
    }

    impl App {
        /// The application `app` (or `pid`), once the API answers for it.
        /// Ready at once; the Linux reader awaits D-Bus, so callers await both.
        pub(super) fn find(
            app: &str,
            pid: Option<u32>,
            json: bool,
        ) -> std::future::Ready<Result<Self, ExitCode>> {
            std::future::ready(Self::find_now(app, pid, json))
        }

        fn find_now(app: &str, pid: Option<u32>, json: bool) -> Result<Self, ExitCode> {
            let pid = pid.map_or_else(|| pid_of(app), Ok).map_err(|message| {
                json_err::emit(
                    json,
                    &json_err::CliError::new(
                        json_err::codes::NO_SUCH_TARGET,
                        message,
                        "name a running application's process, or pass `--pid`",
                    ),
                    EXIT_USAGE,
                )
            })?;
            let root = AXUIElement::application(pid.cast_signed());
            // Trust is learned from the first read: an untrusted process gets
            // `kAXErrorAPIDisabled` for every element.
            if let Err(error) = root.role() {
                return Err(json_err::emit(
                    json,
                    &json_err::CliError::new(
                        json_err::codes::WORKLOAD,
                        format!("cannot read {app} (pid {pid}): {error}"),
                        "grant the terminal (or phux) Accessibility in System Settings, Privacy & Security, then run again",
                    ),
                    EXIT_FAILURE,
                ));
            }
            Ok(Self { root, pid })
        }

        pub(super) const fn pid(&self) -> u32 {
            self.pid
        }

        /// The whole tree, or `Null` once the application stops answering.
        pub(super) fn read(&self, depth: usize) -> std::future::Ready<serde_json::Value> {
            let mut budget = MAX_NODES;
            std::future::ready(node(&self.root, depth, &mut budget))
        }
    }

    /// The first process named exactly `app`, via `pgrep -x`.
    fn pid_of(app: &str) -> Result<u32, String> {
        let output = std::process::Command::new("pgrep")
            .args(["-x", app])
            .output()
            .map_err(|err| format!("could not run pgrep: {err}"))?;
        first_pid(&String::from_utf8_lossy(&output.stdout))
            .ok_or_else(|| format!("no running process named {app:?}"))
    }

    pub(super) fn first_pid(pgrep: &str) -> Option<u32> {
        pgrep.lines().find_map(|line| line.trim().parse().ok())
    }

    /// One element as JSON: its role, title, value and description when it
    /// has them, and its children while `depth` and `budget` allow. `Null`
    /// for an element the API no longer answers for.
    fn node(element: &AXUIElement, depth: usize, budget: &mut usize) -> serde_json::Value {
        let Ok(role) = element.role() else {
            return serde_json::Value::Null;
        };
        *budget = budget.saturating_sub(1);
        let mut object = serde_json::Map::new();
        object.insert("role".into(), role.to_string().into());
        for (key, text) in [
            ("title", element.title().ok().map(|s| s.to_string())),
            (
                "description",
                element.description().ok().map(|s| s.to_string()),
            ),
            ("value", element.value().ok().and_then(|v| value_text(&v))),
        ] {
            if let Some(text) = text.filter(|text| !text.is_empty()) {
                object.insert(key.into(), text.into());
            }
        }
        if depth > 0
            && *budget > 0
            && let Ok(children) = element.children()
        {
            let mut nested = Vec::new();
            for child in children.iter() {
                if *budget == 0 {
                    break;
                }
                let child = node(&child, depth - 1, budget);
                if !child.is_null() {
                    nested.push(child);
                }
            }
            if !nested.is_empty() {
                object.insert("children".into(), nested.into());
            }
        }
        serde_json::Value::Object(object)
    }

    /// A value attribute as text: strings, numbers and booleans; nothing else.
    fn value_text(value: &CFType) -> Option<String> {
        if let Some(text) = value.downcast::<CFString>() {
            return Some(text.to_string());
        }
        if let Some(number) = value.downcast::<CFNumber>() {
            return number.to_f64().map(|n| n.to_string());
        }
        value
            .downcast::<CFBoolean>()
            .map(|flag| bool::from(flag).to_string())
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn the_first_pid_line_wins_and_noise_is_skipped() {
            assert_eq!(super::first_pid("\n 4242\n99\n"), Some(4242));
            assert_eq!(super::first_pid(""), None);
        }
    }
}
