//! How one document reaches the reader, for `phux agent tree|surface|doc
//! snapshot|follow` and `phux snapshot`.
//!
//! `--out` and `--json` keep the document's bytes exact. Text mode prints a
//! tree's JSON or a text document's text made safe for a terminal
//! ([`phux_client::read::terminal_safe`]) and ending in a newline; other
//! content goes to stdout as raw bytes only when stdout is not a terminal.

use std::io::IsTerminal as _;
use std::process::ExitCode;

use phux_client::tree::{Document, Kind, image_size};

use super::SCHEMA_VERSION;
use crate::commands::json_err;
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// Deliver one document: a tree as text or JSON on stdout, a surface or a
/// typed document as bytes to `out` (or stdout) with a JSON line about it
/// when asked.
pub(crate) fn print_document(
    kind: Kind,
    json: bool,
    out: Option<&std::path::Path>,
    document: &Document,
) -> ExitCode {
    if out.is_none() {
        if json && kind == Kind::Tree {
            outln!("{}", tree_json(document));
            return ExitCode::SUCCESS;
        }
        if !json {
            return print_text(kind, document);
        }
    }
    write_bytes(json, out, document)
}

/// `{ schema_version, seq, document }`: a tree's JSON, null before its
/// first publish.
fn tree_json(document: &Document) -> serde_json::Value {
    let value: serde_json::Value = if document.bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&document.bytes).unwrap_or(serde_json::Value::Null)
    };
    serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "seq": document.seq,
        "document": value,
    })
}

/// Text mode on stdout: the safe text, or raw bytes into a pipe. Binary
/// bytes are refused on a terminal, which they could drive.
fn print_text(kind: Kind, document: &Document) -> ExitCode {
    if let Some(text) = phux_client::read::terminal_text(kind, document) {
        out!("{text}");
        return ExitCode::SUCCESS;
    }
    if document.bytes.is_empty() || !std::io::stdout().is_terminal() {
        return write_bytes(false, None, document);
    }
    json_err::emit(
        false,
        &json_err::CliError::new(
            json_err::codes::USAGE_ERROR,
            format!(
                "the document is {} bytes of binary, which would drive this terminal",
                document.bytes.len()
            ),
            "pass `--out FILE`, or redirect stdout to a file",
        ),
        EXIT_USAGE,
    )
}

/// The bytes to `out`, else stdout, and with `json` a line about them.
fn write_bytes(json: bool, out: Option<&std::path::Path>, document: &Document) -> ExitCode {
    if !document.bytes.is_empty() {
        let written = out.map_or_else(
            || {
                // Flushed: stdout holds binary back until a newline byte.
                use std::io::Write as _;
                let mut stdout = std::io::stdout().lock();
                stdout
                    .write_all(&document.bytes)
                    .and_then(|()| stdout.flush())
            },
            |path| std::fs::write(path, &document.bytes),
        );
        if let Err(err) = written {
            return json_err::emit(
                json,
                &json_err::CliError::new(
                    json_err::codes::USAGE_ERROR,
                    format!("could not write the document: {err}"),
                    "pass a writable `--out PATH`",
                ),
                EXIT_FAILURE,
            );
        }
    }
    if json {
        let size = image_size(&document.bytes).ok();
        outln!(
            "{}",
            serde_json::json!({
                "schema_version": SCHEMA_VERSION,
                "seq": document.seq,
                "bytes": document.bytes.len(),
                "format": size.map(|s| s.0.name()),
                "width": size.map(|s| s.1),
                "height": size.map(|s| s.2),
                "content_type": document.content_type,
                "out": out.map(|p| p.display().to_string()),
            })
        );
    }
    ExitCode::SUCCESS
}
