//! `phux agent tree open|publish|snapshot|follow`: the CLI over the
//! `AccessibilityTree` resource (`docs/resource-kinds.md`).
//!
//! - `open` takes the **parent pane**, the Terminal the producer runs in,
//!   and names the application the tree mirrors.
//! - `publish` replaces the tree's document from a file or stdin.
//! - `snapshot` prints the newest document and exits.
//! - `follow` prints every document as it arrives; `--until TEXT` exits as
//!   soon as a document contains `TEXT`, which is `phux wait` for an app.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use phux_client::attach::connection::Connection;
use phux_client::tree::{self, Document, FollowEnd, Kind, TreeError};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

const SCHEMA_VERSION: u8 = 1;

mod mirror;
mod surface_mirror;

#[derive(Debug, usage::Subcommands)]
pub(crate) enum TreeAction {
    /// Open a tree under the pane its producer runs in.
    Open {
        /// The parent pane.
        target: String,
        /// The application the tree mirrors.
        #[usage(long, value_name = "APP")]
        app: String,
        #[usage(long)]
        json: bool,
    },
    /// Replace the tree's document from a file, or stdin.
    Publish {
        /// The tree.
        target: String,
        /// The document; stdin when absent.
        #[usage(long, value_name = "PATH")]
        file: Option<PathBuf>,
        #[usage(long)]
        json: bool,
    },
    /// Print the tree's newest document.
    Snapshot {
        /// The tree.
        target: String,
        #[usage(long)]
        json: bool,
    },
    /// Mirror a running macOS application into a tree, until Ctrl-C.
    Mirror {
        /// The application's process name, e.g. `Safari`.
        app: String,
        /// The application's process id, instead of its name.
        #[usage(long, value_name = "PID")]
        pid: Option<u32>,
        /// The pane the tree binds to; this process's own pane when absent.
        #[usage(long, value_name = "PANE")]
        pane: Option<String>,
        /// Milliseconds between reads (default 500).
        #[usage(long, value_name = "MS")]
        interval: Option<u64>,
        /// Deepest nesting to read (default 24).
        #[usage(long, value_name = "N")]
        depth: Option<usize>,
        #[usage(long)]
        json: bool,
    },
    /// Print every document as it arrives.
    Follow {
        /// The tree.
        target: String,
        /// Stop as soon as a document contains this text.
        #[usage(long, value_name = "TEXT")]
        until: Option<String>,
        /// Give up after this many seconds.
        #[usage(long, value_name = "SECS")]
        timeout: Option<u64>,
        #[usage(long)]
        json: bool,
    },
}

/// `phux agent surface open|publish|snapshot|follow`: the same verbs over a
/// `PixelSurface`, whose documents are PNG images and go to files.
#[derive(Debug, usage::Subcommands)]
pub(crate) enum SurfaceAction {
    /// Open a surface under the pane its producer runs in.
    Open {
        /// The parent pane.
        target: String,
        /// The window or display the surface mirrors.
        #[usage(long, value_name = "WINDOW")]
        window: String,
        #[usage(long)]
        json: bool,
    },
    /// Replace the surface's image from a PNG file, or stdin.
    Publish {
        /// The surface.
        target: String,
        /// The PNG; stdin when absent.
        #[usage(long, value_name = "PATH")]
        file: Option<PathBuf>,
        #[usage(long)]
        json: bool,
    },
    /// Write the surface's newest image to a file.
    Snapshot {
        /// The surface.
        target: String,
        /// Where the PNG goes; stdout when absent.
        #[usage(long, value_name = "PATH")]
        out: Option<PathBuf>,
        #[usage(long)]
        json: bool,
    },
    /// Mirror a macOS window, or the main display, into a surface until Ctrl-C.
    Mirror {
        /// The window as `App` or `App:title`, or `display` for the main display.
        window: String,
        /// The CoreGraphics window id, instead of a name.
        #[usage(long, value_name = "ID")]
        window_id: Option<u32>,
        /// The pane the surface binds to; this process's own pane when absent.
        #[usage(long, value_name = "PANE")]
        pane: Option<String>,
        /// Milliseconds between captures (default 1000).
        #[usage(long, value_name = "MS")]
        interval: Option<u64>,
        #[usage(long)]
        json: bool,
    },
    /// Write every image as it arrives, each replacing the file.
    Follow {
        /// The surface.
        target: String,
        /// Where each PNG goes; stdout when absent.
        #[usage(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Give up after this many seconds.
        #[usage(long, value_name = "SECS")]
        timeout: Option<u64>,
        #[usage(long)]
        json: bool,
    },
}

pub(super) fn run_surface(action: &SurfaceAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        SurfaceAction::Open {
            target,
            window,
            json,
        } => run_open(Kind::Surface, target, window, *json, socket),
        SurfaceAction::Publish { target, file, json } => {
            run_publish(Kind::Surface, target, file.as_deref(), *json, socket)
        }
        SurfaceAction::Snapshot { target, out, json } => {
            run_snapshot(Kind::Surface, target, out.as_deref(), *json, socket)
        }
        SurfaceAction::Mirror {
            window,
            window_id,
            pane,
            interval,
            json,
        } => surface_mirror::run_mirror(surface_mirror::MirrorArgs {
            window,
            window_id: *window_id,
            pane: pane.as_deref(),
            interval_ms: interval.unwrap_or(1000),
            json: *json,
            socket,
        }),
        SurfaceAction::Follow {
            target,
            out,
            timeout,
            json,
        } => run_follow(
            Kind::Surface,
            target,
            Sink {
                until: None,
                out: out.as_deref(),
            },
            *timeout,
            *json,
            socket,
        ),
    }
}

pub(super) fn run_tree(action: &TreeAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        TreeAction::Open { target, app, json } => run_open(Kind::Tree, target, app, *json, socket),
        TreeAction::Publish { target, file, json } => {
            run_publish(Kind::Tree, target, file.as_deref(), *json, socket)
        }
        TreeAction::Snapshot { target, json } => {
            run_snapshot(Kind::Tree, target, None, *json, socket)
        }
        TreeAction::Mirror {
            app,
            pid,
            pane,
            interval,
            depth,
            json,
        } => mirror::run_mirror(mirror::MirrorArgs {
            app,
            pid: *pid,
            pane: pane.as_deref(),
            interval_ms: interval.unwrap_or(500),
            depth: depth.unwrap_or(24),
            json: *json,
            socket,
        }),
        TreeAction::Follow {
            target,
            until,
            timeout,
            json,
        } => run_follow(
            Kind::Tree,
            target,
            Sink {
                until: until.as_deref(),
                out: None,
            },
            *timeout,
            *json,
            socket,
        ),
    }
}

/// Resolve `target` to a resource, or report why not.
pub(super) async fn resolve(
    socket_path: &std::path::Path,
    target: &str,
    verb: &str,
    json: bool,
) -> Result<phux_protocol::ids::ResourceId, ExitCode> {
    let selector = parse_selector(Some(target))?;
    resolve_target(socket_path, &selector, verb, json).await
}

/// Connect, or report why not.
pub(super) async fn dial(
    socket_path: &std::path::Path,
    json: bool,
) -> Result<Connection, ExitCode> {
    Connection::connect(socket_path).await.map_err(|err| {
        json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::NO_SERVER,
                err.to_string(),
                "start a server with `phux server --ensure`",
            ),
            EXIT_FAILURE,
        )
    })
}

/// Resolve `target` and connect, or report why not.
async fn connect(
    target: &str,
    verb: &str,
    json: bool,
    socket: Option<PathBuf>,
) -> Result<(Connection, phux_protocol::ids::ResourceId), ExitCode> {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let resource = resolve(&socket_path, target, verb, json).await?;
    let conn = dial(&socket_path, json).await?;
    Ok((conn, resource))
}

pub(super) fn report(json: bool, err: &TreeError) -> ExitCode {
    let (code, hint) = match err {
        TreeError::Unsupported => (
            json_err::codes::SERVER_TOO_OLD,
            "update the phux server; it does not serve accessibility trees",
        ),
        TreeError::WrongKind { .. }
        | TreeError::ParentNotFound { .. }
        | TreeError::ParentKindMismatch { .. } => (
            json_err::codes::NO_SUCH_TARGET,
            "name a tree by its @N id; `open` names the pane the producer runs in",
        ),
        TreeError::Invalid(_) => (json_err::codes::RECORD_INVALID, "publish one JSON document"),
        TreeError::NotProducer(_) => (
            json_err::codes::WORKLOAD,
            "publish from the local owner connection",
        ),
        TreeError::Refused(_) => (json_err::codes::WORKLOAD, "see the message"),
        TreeError::Protocol(_) => (json_err::codes::TRANSPORT, "retry"),
    };
    json_err::emit(
        json,
        &json_err::CliError::new(code, err.to_string(), hint),
        EXIT_FAILURE,
    )
}

/// Where a followed document goes, and what ends the follow.
#[derive(Clone, Copy)]
struct Sink<'a> {
    /// Stop as soon as a document contains this text (trees).
    until: Option<&'a str>,
    /// Write each document to this file instead of stdout (surfaces).
    out: Option<&'a std::path::Path>,
}

/// The width and height a PNG's `IHDR` names, when `bytes` starts like one.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Some((be(16), be(20)))
}

/// Deliver one document: a tree as text or JSON on stdout, a surface as
/// bytes to `out` (or stdout) with a JSON line about it when asked.
fn print_document(
    kind: Kind,
    json: bool,
    out: Option<&std::path::Path>,
    document: &Document,
) -> ExitCode {
    match kind {
        Kind::Tree if json => {
            let value: serde_json::Value = if document.bytes.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice(&document.bytes).unwrap_or(serde_json::Value::Null)
            };
            outln!(
                "{}",
                serde_json::json!({
                    "schema_version": SCHEMA_VERSION,
                    "seq": document.seq,
                    "document": value,
                })
            );
        }
        Kind::Tree => {
            if !document.bytes.is_empty() {
                outln!("{}", String::from_utf8_lossy(&document.bytes));
            }
        }
        Kind::Surface => {
            if !document.bytes.is_empty() {
                let written = out.map_or_else(
                    || {
                        use std::io::Write as _;
                        std::io::stdout().write_all(&document.bytes)
                    },
                    |path| std::fs::write(path, &document.bytes),
                );
                if let Err(err) = written {
                    return json_err::emit(
                        json,
                        &json_err::CliError::new(
                            json_err::codes::USAGE_ERROR,
                            format!("could not write the image: {err}"),
                            "pass a writable `--out PATH`",
                        ),
                        EXIT_FAILURE,
                    );
                }
            }
            if json {
                let size = png_size(&document.bytes);
                outln!(
                    "{}",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "seq": document.seq,
                        "bytes": document.bytes.len(),
                        "width": size.map(|s| s.0),
                        "height": size.map(|s| s.1),
                        "out": out.map(|p| p.display().to_string()),
                    })
                );
            }
        }
    }
    ExitCode::SUCCESS
}

fn run_open(kind: Kind, target: &str, app: &str, json: bool, socket: Option<PathBuf>) -> ExitCode {
    if app.trim().is_empty() {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::RECORD_INVALID,
                format!("what the {} mirrors must be named", kind.label()),
                "e.g. `--app Safari` for a tree, `--window Safari` for a surface",
            ),
            EXIT_USAGE,
        );
    }
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let (mut conn, parent) = match connect(target, "agent tree open", json, socket).await {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        match tree::open_kind(&mut conn, kind, &parent, app).await {
            Ok(opened) => {
                let resource = phux_client::selector::format_terminal_id(&opened.resource);
                if json {
                    outln!(
                        "{}",
                        serde_json::json!({
                            "schema_version": SCHEMA_VERSION,
                            "resource": resource,
                            "parent": phux_client::selector::format_terminal_id(&opened.parent),
                            "app": app,
                        })
                    );
                } else {
                    outln!("{resource}");
                }
                ExitCode::SUCCESS
            }
            Err(err) => report(json, &err),
        }
    })
}

fn run_publish(
    kind: Kind,
    target: &str,
    file: Option<&std::path::Path>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let mut bytes = Vec::new();
    let read = match file {
        Some(path) => std::fs::read(path).map(|content| bytes = content),
        None => std::io::stdin().read_to_end(&mut bytes).map(|_| ()),
    };
    if let Err(err) = read {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::USAGE_ERROR,
                format!("could not read the document: {err}"),
                "pass `--file PATH` or pipe the document to stdin",
            ),
            EXIT_USAGE,
        );
    }
    let shape = match kind {
        Kind::Tree => serde_json::from_slice::<serde_json::Value>(&bytes)
            .map(|_| ())
            .map_err(|err| format!("the document is not JSON: {err}")),
        Kind::Surface => png_size(&bytes)
            .map(|_| ())
            .ok_or_else(|| "the document is not a PNG".to_owned()),
    };
    if let Err(message) = shape {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::RECORD_INVALID,
                message,
                "publish one JSON document to a tree, one PNG image to a surface",
            ),
            EXIT_USAGE,
        );
    }
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let (mut conn, resource) = match connect(target, "agent tree publish", json, socket).await {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        match tree::publish(&mut conn, &resource, bytes).await {
            Ok(published) => {
                if json {
                    outln!(
                        "{}",
                        serde_json::json!({
                            "schema_version": SCHEMA_VERSION,
                            "seq": published.seq,
                            "ts_ms": published.ts_ms,
                        })
                    );
                }
                ExitCode::SUCCESS
            }
            Err(err) => report(json, &err),
        }
    })
}

fn run_snapshot(
    kind: Kind,
    target: &str,
    out: Option<&std::path::Path>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let (mut conn, resource) = match connect(target, "agent tree snapshot", json, socket).await
        {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        match tree::snapshot(&mut conn, &resource).await {
            Ok(document) => print_document(kind, json, out, &document),
            Err(err) => report(json, &err),
        }
    })
}

fn run_follow(
    kind: Kind,
    target: &str,
    sink: Sink<'_>,
    timeout: Option<u64>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let Sink { until, out } = sink;
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let (mut conn, resource) = match connect(target, "agent tree follow", json, socket).await {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        let mut matched = false;
        let sink = |document: Document| {
            let hit =
                until.is_some_and(|text| String::from_utf8_lossy(&document.bytes).contains(text));
            if until.is_none() || hit {
                print_document(kind, json, out, &document);
            }
            matched = hit;
            !hit
        };
        let followed = match timeout {
            Some(secs) => {
                match tokio::time::timeout(
                    Duration::from_secs(secs),
                    tree::follow(&mut conn, &resource, sink),
                )
                .await
                {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        return json_err::emit(
                            json,
                            &json_err::CliError::new(
                                json_err::codes::WORKLOAD,
                                format!("no document matched within {secs}s"),
                                "raise `--timeout` or loosen `--until`",
                            ),
                            EXIT_FAILURE,
                        );
                    }
                }
            }
            None => tree::follow(&mut conn, &resource, sink).await,
        };
        match followed {
            Ok(FollowEnd::Stopped) => ExitCode::SUCCESS,
            Ok(FollowEnd::Closed | FollowEnd::Disconnected) if until.is_none() || matched => {
                ExitCode::SUCCESS
            }
            Ok(end) => json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::WORKLOAD,
                    format!("the tree ended ({end:?}) before a document matched"),
                    "the producer closed or the connection dropped",
                ),
                EXIT_FAILURE,
            ),
            Err(err) => report(json, &err),
        }
    })
}
