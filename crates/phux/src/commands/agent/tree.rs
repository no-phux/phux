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
use phux_client::tree::{self, Document, FollowEnd, TreeError};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

const SCHEMA_VERSION: u8 = 1;

mod mirror;

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

pub(super) fn run_tree(action: &TreeAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        TreeAction::Open { target, app, json } => run_open(target, app, *json, socket),
        TreeAction::Publish { target, file, json } => {
            run_publish(target, file.as_deref(), *json, socket)
        }
        TreeAction::Snapshot { target, json } => run_snapshot(target, *json, socket),
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
        } => run_follow(target, until.as_deref(), *timeout, *json, socket),
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

fn print_document(json: bool, document: &Document) -> ExitCode {
    if json {
        let value: serde_json::Value = if document.bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&document.bytes).unwrap_or(serde_json::Value::Null)
        };
        let line = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "seq": document.seq,
            "document": value,
        });
        outln!("{line}");
    } else if !document.bytes.is_empty() {
        outln!("{}", String::from_utf8_lossy(&document.bytes));
    }
    ExitCode::SUCCESS
}

fn run_open(target: &str, app: &str, json: bool, socket: Option<PathBuf>) -> ExitCode {
    if app.trim().is_empty() {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::RECORD_INVALID,
                "--app must not be empty",
                "name the application the tree mirrors, e.g. `--app Safari`",
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
        match tree::open(&mut conn, &parent, app).await {
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
    if let Err(err) = serde_json::from_slice::<serde_json::Value>(&bytes) {
        return json_err::emit(
            json,
            &json_err::CliError::new(
                json_err::codes::RECORD_INVALID,
                format!("the document is not JSON: {err}"),
                "publish one JSON document",
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

fn run_snapshot(target: &str, json: bool, socket: Option<PathBuf>) -> ExitCode {
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
            Ok(document) => print_document(json, &document),
            Err(err) => report(json, &err),
        }
    })
}

fn run_follow(
    target: &str,
    until: Option<&str>,
    timeout: Option<u64>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
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
                print_document(json, &document);
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
