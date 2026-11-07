//! `phux agent doc open|publish|snapshot|follow`: the verbs over a typed
//! `Document`, whose documents are of the content type it was opened with.

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::tree::{self, Kind};

use super::{SCHEMA_VERSION, Sink, connect, report, run_follow, run_publish, run_snapshot};
use crate::commands::cli_runtime;

/// `phux agent doc open|publish|snapshot|follow`.
#[derive(Debug, usage::Subcommands)]
pub(crate) enum DocAction {
    /// Open a document under the pane its producer runs in.
    Open {
        /// The parent pane.
        target: String,
        /// The content type, a MIME type such as `text/markdown`.
        #[usage(long = "type", value_name = "MIME")]
        content_type: String,
        /// A name for the document.
        #[usage(long, value_name = "NAME")]
        name: Option<String>,
        #[usage(long)]
        json: bool,
    },
    /// Replace the document from a file, or stdin.
    Publish {
        /// The document resource.
        target: String,
        /// The document; stdin when absent.
        #[usage(long, value_name = "PATH")]
        file: Option<PathBuf>,
        #[usage(long)]
        json: bool,
    },
    /// Write the newest document to a file, or stdout.
    Snapshot {
        /// The document resource.
        target: String,
        /// Where the document goes; stdout when absent.
        #[usage(long, value_name = "PATH")]
        out: Option<PathBuf>,
        #[usage(long)]
        json: bool,
    },
    /// Write every document as it arrives.
    Follow {
        /// The document resource.
        target: String,
        /// Where each document goes, replacing the file; stdout when absent.
        #[usage(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Give up after this many seconds.
        #[usage(long, value_name = "SECS")]
        timeout: Option<u64>,
        #[usage(long)]
        json: bool,
    },
}

pub(crate) fn run_doc(action: &DocAction, socket: Option<PathBuf>) -> ExitCode {
    match action {
        DocAction::Open {
            target,
            content_type,
            name,
            json,
        } => run_open(target, content_type, name.as_deref(), *json, socket),
        DocAction::Publish { target, file, json } => {
            run_publish(Kind::Document, target, file.as_deref(), *json, socket)
        }
        DocAction::Snapshot { target, out, json } => {
            run_snapshot(Kind::Document, target, out.as_deref(), *json, socket)
        }
        DocAction::Follow {
            target,
            out,
            timeout,
            json,
        } => run_follow(
            Kind::Document,
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

fn run_open(
    target: &str,
    content_type: &str,
    name: Option<&str>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let (mut conn, parent) = match connect(target, "agent doc open", json, socket).await {
            Ok(pair) => pair,
            Err(code) => return code,
        };
        match tree::open_document(&mut conn, &parent, content_type, name).await {
            Ok(opened) => {
                let resource = phux_client::selector::format_terminal_id(&opened.resource);
                if json {
                    outln!(
                        "{}",
                        serde_json::json!({
                            "schema_version": SCHEMA_VERSION,
                            "resource": resource,
                            "parent": phux_client::selector::format_terminal_id(&opened.parent),
                            "content_type": content_type,
                            "name": name,
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
