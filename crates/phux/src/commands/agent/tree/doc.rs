//! `phux agent doc open|publish|snapshot|follow`: the verbs over a typed
//! `Document`, whose documents are of the content type it was opened with.
//! `serve` is the producer contract any language speaks over a pipe;
//! `folder` and `notifications` are producers built on it
//! (`docs/applications.md#publish-your-own-kind`).

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::tree::{self, Kind, TreeError};

use super::doc_producer::{Producer, intent_json};
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
    /// Write the newest document to a file, or stdout: text with control
    /// characters shown as signs, binary only into a pipe.
    Snapshot {
        /// The document resource.
        target: String,
        /// Where the document goes; stdout when absent.
        #[usage(long, value_name = "PATH")]
        out: Option<PathBuf>,
        #[usage(long)]
        json: bool,
    },
    /// Produce a document over a pipe until stdin ends: each stdin line
    /// `{"text": ...}` or `{"base64": ...}` replaces it; stdout prints an
    /// `opened` line, then one `input` line per viewer intent.
    Serve {
        /// The content type, a MIME type such as `text/markdown`.
        #[usage(long = "type", value_name = "MIME")]
        content_type: String,
        /// A name for the document.
        #[usage(long, value_name = "NAME")]
        name: Option<String>,
        /// The parent pane; this process's own pane when absent.
        #[usage(long, value_name = "PANE")]
        pane: Option<String>,
    },
    /// Keep a directory listed as a Markdown document, refreshed on change;
    /// a `navigate` intent opens a subdirectory.
    Folder {
        /// The directory.
        path: PathBuf,
        /// A name for the document; the directory's name when absent.
        #[usage(long, value_name = "NAME")]
        name: Option<String>,
        /// The parent pane; this process's own pane when absent.
        #[usage(long, value_name = "PANE")]
        pane: Option<String>,
        /// Seconds between rereads (default 2).
        #[usage(long, value_name = "SECS")]
        interval: Option<u64>,
    },
    /// Keep this host's desktop notifications as a Markdown document
    /// (Linux, freedesktop Notifications on the session bus).
    Notifications {
        /// A name for the document (default `notifications`).
        #[usage(long, value_name = "NAME")]
        name: Option<String>,
        /// The parent pane; this process's own pane when absent.
        #[usage(long, value_name = "PANE")]
        pane: Option<String>,
        /// How many to keep, newest first (default 50).
        #[usage(long, value_name = "N")]
        keep: Option<usize>,
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
        DocAction::Serve {
            content_type,
            name,
            pane,
        } => run_serve(content_type, name.as_deref(), pane.as_deref(), socket),
        DocAction::Folder {
            path,
            name,
            pane,
            interval,
        } => super::doc_folder::run(path, name.as_deref(), pane.as_deref(), *interval, socket),
        DocAction::Notifications { name, pane, keep } => {
            super::doc_notifications::run(name.as_deref(), pane.as_deref(), *keep, socket)
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

fn run_serve(
    content_type: &str,
    name: Option<&str>,
    pane: Option<&str>,
    socket: Option<PathBuf>,
) -> ExitCode {
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let verb = "agent doc serve";
        let mut producer = match Producer::open(socket, pane, content_type, name, verb).await {
            Ok(producer) => producer,
            Err(code) => return code,
        };
        // A plain thread reads stdin: a blocked read on the runtime's pool
        // would hold the process open after the loop returns.
        let (tx, mut lines) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return ExitCode::SUCCESS,
                woke = producer.recv() => match producer.settle(woke).await {
                    Ok(input) => {
                        if let Some(line) = input.as_ref().and_then(intent_json) {
                            outln!("{line}");
                        }
                    }
                    Err(err) => return report(false, &err),
                },
                line = lines.recv() => {
                    let Some(line) = line else { return ExitCode::SUCCESS };
                    let bytes = match serve_line(&line) {
                        Ok(Some(bytes)) => bytes,
                        Ok(None) => continue,
                        Err(why) => {
                            eprintln!("phux agent doc serve: skipped a line: {why}");
                            continue;
                        }
                    };
                    match producer.publish(bytes).await {
                        Ok(()) => {}
                        Err(err @ TreeError::Invalid(_)) => {
                            eprintln!("phux agent doc serve: refused: {err}");
                        }
                        Err(err) => return report(false, &err),
                    }
                }
            }
        }
    })
}

/// The document one stdin line carries: `{"text": ...}` or
/// `{"base64": ...}`; a blank line carries none.
fn serve_line(line: &str) -> Result<Option<Vec<u8>>, String> {
    use base64::Engine as _;

    if line.trim().is_empty() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_str(line).map_err(|err| err.to_string())?;
    if let Some(text) = value["text"].as_str() {
        return Ok(Some(text.as_bytes().to_vec()));
    }
    if let Some(b64) = value["base64"].as_str() {
        let engine = base64::engine::general_purpose::STANDARD;
        return engine.decode(b64).map(Some).map_err(|err| err.to_string());
    }
    Err("expected {\"text\": ...} or {\"base64\": ...}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::serve_line;

    #[test]
    fn a_serve_line_is_text_or_base64() {
        assert_eq!(
            serve_line(r##"{"text":"# hi"}"##),
            Ok(Some(b"# hi".to_vec()))
        );
        assert_eq!(serve_line(r#"{"base64":"aGk="}"#), Ok(Some(b"hi".to_vec())));
        assert_eq!(serve_line("  "), Ok(None));
        assert!(serve_line(r#"{"other":1}"#).is_err());
        assert!(serve_line("not json").is_err());
    }
}
