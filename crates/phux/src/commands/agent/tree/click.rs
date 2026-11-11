//! `phux agent tree click TREE --node ID | --name TEXT [--role ROLE]`:
//! activate one element of an accessibility tree by node instead of pixel
//! (`docs/applications.md#agents-clicking-elements`). The finding, the
//! semantic intent and the pixel fallback live in
//! `phux_client::tree::element`, shared with the MCP `phux_click_element`.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::tree::Kind;
use phux_client::tree::element::{self, ClickError, Clicked, FindError, Selector};
use phux_server::runtime::default_socket_path;

use super::{SCHEMA_VERSION, Sink, report, resolve};
use crate::commands::{cli_runtime, json_err};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// `tree click`'s arguments.
#[derive(Debug, usage::Args)]
pub(crate) struct ClickArgs {
    /// The tree.
    target: String,
    /// The node's `id`, as the tree document gives it.
    #[usage(long, value_name = "ID")]
    node: Option<String>,
    /// The node whose name (title or description) is TEXT, ignoring case;
    /// an exact name beats a substring.
    #[usage(long, value_name = "TEXT")]
    name: Option<String>,
    /// With `--name`, only nodes of this role (`AXButton`, `push button`).
    #[usage(long, value_name = "ROLE", requires("--name"))]
    role: Option<String>,
    /// Click the node's centre on the window surface beside the tree,
    /// skipping the producer's semantic action.
    #[usage(long)]
    pixel: bool,
    /// After the click, wait until a tree document contains this text.
    #[usage(long, value_name = "TEXT")]
    wait_until: Option<String>,
    /// Give up waiting after this many seconds.
    #[usage(long, value_name = "SECS", requires("--wait-until"))]
    timeout: Option<u64>,
    #[usage(long)]
    json: bool,
}

pub(super) fn run_click(args: &ClickArgs, socket: Option<PathBuf>) -> ExitCode {
    let json = args.json;
    let selector = match (&args.node, &args.name) {
        (Some(id), None) => Selector::Id(id),
        (None, Some(name)) => Selector::Name {
            name,
            role: args.role.as_deref(),
        },
        _ => {
            return json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::USAGE_ERROR,
                    "name exactly one of `--node ID` or `--name TEXT`",
                    "`phux agent tree snapshot TREE` shows each node's id and title",
                ),
                EXIT_USAGE,
            );
        }
    };
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let clicked = rt.block_on(async {
        let socket_path = socket.clone().unwrap_or_else(default_socket_path);
        let tree = resolve(&socket_path, &args.target, "agent tree click", json).await?;
        element::click(&socket_path, &tree, selector, args.pixel)
            .await
            .map_err(|err| click_error(json, &err))
    });
    drop(rt);
    let (node, how) = match clicked {
        Ok(done) => done,
        Err(code) => return code,
    };
    let (path, surface, pixel) = match &how {
        Clicked::Semantic => ("semantic", None, None),
        Clicked::Pixel { surface, x, y } => (
            "pixel",
            Some(phux_client::selector::format_terminal_id(surface)),
            Some([*x, *y]),
        ),
    };
    if json {
        outln!(
            "{}",
            serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "node": node.id,
                "role": node.role,
                "name": node.name,
                "path": path,
                "surface": surface,
                "pixel": pixel,
            })
        );
    } else {
        outln!(
            "clicked [{}] {} {:?} ({path})",
            node.id,
            node.role,
            node.name
        );
    }
    args.wait_until
        .as_deref()
        .map_or(ExitCode::SUCCESS, |until| {
            let sink = Sink {
                until: Some(until),
                out: None,
            };
            super::run_follow(Kind::Tree, &args.target, sink, args.timeout, json, socket)
        })
}

fn click_error(json: bool, err: &ClickError) -> ExitCode {
    let (code, hint) = match err {
        ClickError::Tree(err) => return report(json, err),
        ClickError::NoDocument => (
            json_err::codes::WORKLOAD,
            "wait for the tree's producer to publish",
        ),
        ClickError::Find(FindError::Ambiguous(_)) => (
            json_err::codes::USAGE_ERROR,
            "pick one with `--node ID`, or narrow with `--role`",
        ),
        ClickError::Find(FindError::NoMatch { .. }) => (
            json_err::codes::NO_SUCH_TARGET,
            "`phux agent tree snapshot TREE` shows each node's id and title",
        ),
        ClickError::NoPixel { .. } => (
            json_err::codes::WORKLOAD,
            "mirror the app with `phux agent app run` so a producer serves the tree and a window surface sits beside it",
        ),
    };
    json_err::emit(
        json,
        &json_err::CliError::new(code, err.to_string(), hint),
        EXIT_FAILURE,
    )
}
