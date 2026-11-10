//! The CLI side of kind dispatch ([`phux_client::read`]): the refusals, as
//! usage errors, and the document branches of `phux snapshot` and
//! `phux wait`. A tree prints what `phux agent tree snapshot` prints, and a
//! surface or a document written to `--out` what `phux agent surface|doc
//! snapshot --out` prints.

use std::path::Path;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::deadline::Deadline;
use phux_client::read::{
    self, DocumentWait, Resolved, TerminalOnly, TextMatch, WaitError, WrongKind,
};
use phux_client::selector::format_terminal_id;
use phux_client::tree::{Document, Kind, TreeError};
use phux_client::wait::Condition;

use crate::commands::agent::tree::{print_document, report};
use crate::commands::json_err::{self, CliError, codes};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE, EXIT_WAIT_TIMEOUT};

/// A verb refused for the resource's kind: `wrong_resource_kind`, exit 2.
pub(crate) fn wrong_kind(json: bool, err: &WrongKind) -> ExitCode {
    json_err::emit(
        json,
        &CliError::new(codes::WRONG_RESOURCE_KIND, err.to_string(), err.remedy()),
        EXIT_USAGE,
    )
}

/// A Terminal option given for another kind: a usage error, exit 2.
pub(crate) fn terminal_only(json: bool, err: &TerminalOnly) -> ExitCode {
    usage(
        json,
        err.to_string(),
        format!("drop `{}`, or name a terminal", err.option),
    )
}

/// `--out` given for a Terminal, whose screen prints on stdout.
pub(crate) fn out_on_terminal(json: bool, resolved: &Resolved) -> ExitCode {
    usage(
        json,
        format!(
            "`--out` writes a document's content to a file, and {} is a terminal",
            format_terminal_id(&resolved.id)
        ),
        "drop `--out`; a terminal's screen prints on stdout",
    )
}

/// A document wait with no text to match: `--idle` reads a screen.
fn needs_text(json: bool, resolved: &Resolved) -> ExitCode {
    usage(
        json,
        format!(
            "{} is {}, which has no idle: a wait on it matches text",
            format_terminal_id(&resolved.id),
            read::kind_label(resolved.kind)
        ),
        "pass `--until TEXT` or `--regex PATTERN`",
    )
}

fn usage(json: bool, message: String, remedy: impl Into<String>) -> ExitCode {
    json_err::emit(
        json,
        &CliError::new(codes::USAGE_ERROR, message, remedy),
        EXIT_USAGE,
    )
}

fn report_read(json: bool, socket: &Path, verb: &str, err: &TreeError) -> ExitCode {
    match err {
        TreeError::Protocol(err @ AttachError::Io(_)) => {
            json_err::report_no_server(json, err, socket, verb)
        }
        other => report(json, other),
    }
}

/// `phux snapshot` of a document resource. A surface needs `--out`; a
/// binary document without one prints its size and type.
pub(crate) async fn snapshot_document(
    socket: &Path,
    resolved: &Resolved,
    kind: Kind,
    out: Option<&Path>,
    json: bool,
) -> ExitCode {
    if kind == Kind::Surface && out.is_none() {
        return usage(
            json,
            format!(
                "{} is a pixel surface: its image goes to a file, not the terminal",
                format_terminal_id(&resolved.id)
            ),
            "pass `--out FILE`",
        );
    }
    let document = match read::document(socket, &resolved.id).await {
        Ok(document) => document,
        Err(err) => return report_read(json, socket, "snapshot", &err),
    };
    match (kind, out) {
        // A tree's JSON goes to the file as bytes, as a document's would.
        (Kind::Tree, Some(path)) => print_document(Kind::Document, json, Some(path), &document),
        (_, Some(path)) => print_document(kind, json, Some(path), &document),
        (Kind::Tree, None) => print_document(Kind::Tree, json, None, &document),
        (_, None) => print_unfiled(json, &document),
    }
}

/// A typed document with no `--out`: its text, or its size and type.
fn print_unfiled(json: bool, document: &Document) -> ExitCode {
    if json {
        outln!("{}", read::document_json(document));
        return ExitCode::SUCCESS;
    }
    if read::text_of(Kind::Document, document).is_some() {
        return print_document(Kind::Document, false, None, document);
    }
    outln!(
        "{} bytes of {} (seq {}); pass `--out FILE` to write it",
        document.bytes.len(),
        document.content_type.as_deref().unwrap_or("unknown type"),
        document.seq
    );
    ExitCode::SUCCESS
}

/// `phux wait` on a tree or a text document: exit 0 once a document's text
/// meets `condition`, 124 when `deadline` passes, 1 when it closes first,
/// and 2 for an idle wait, which reads a screen.
pub(crate) async fn wait_document(
    socket: &Path,
    resolved: &Resolved,
    kind: Kind,
    condition: &Condition,
    deadline: Deadline,
    json: bool,
) -> ExitCode {
    let Some(matcher) = TextMatch::of(condition) else {
        return needs_text(json, resolved);
    };
    let waited = read::wait_document(socket, &resolved.id, kind, matcher, deadline).await;
    let id = format_terminal_id(&resolved.id);
    match waited {
        Ok(DocumentWait::Matched { document, .. }) => {
            if !json {
                return ExitCode::SUCCESS;
            }
            if kind == Kind::Tree {
                return print_document(Kind::Tree, true, None, &document);
            }
            outln!("{}", read::document_json(&document));
            ExitCode::SUCCESS
        }
        Ok(DocumentWait::TimedOut { reads }) => json_err::emit(
            json,
            &CliError::new(
                codes::TIMED_OUT,
                format!(
                    "wait timed out after {reads} {} of {id}",
                    if reads == 1 { "document" } else { "documents" }
                ),
                "raise `--timeout`, or read it with `phux snapshot` to see why the condition \
                 never held",
            ),
            EXIT_WAIT_TIMEOUT,
        ),
        Ok(DocumentWait::Closed { .. }) => json_err::emit(
            json,
            &CliError::new(
                codes::WORKLOAD,
                format!("{id} closed before a document matched"),
                "its producer closed it",
            ),
            EXIT_FAILURE,
        ),
        Err(err @ WaitError::NotText { .. }) => {
            usage(json, err.to_string(), "wait on a text document or a tree")
        }
        Err(WaitError::Tree(err)) => report_read(json, socket, "wait", &err),
    }
}
