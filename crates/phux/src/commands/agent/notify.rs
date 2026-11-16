//! `phux notify` — post a note for a pane: the `phux.note/v1` record, which
//! clients show as a toast, and as a system notification when the pane is
//! out of sight (`docs/scripting.md#notes`).

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client_core::note::{MAX_NOTE_BYTES, Note, NoteLevel, parse_note};
use phux_protocol::wire::frame::{FrameKind, RESOURCE_NOTE_KEY, Scope};

use super::record::with_target_pane;
use crate::commands::json_err::{self, CliError, codes};
use crate::commands::{NoteLevelArg, warn_interleaved_degradation};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

/// Write the note to TARGET (default: the pane this runs in, else the
/// focused pane) and confirm it with a read-back.
pub(crate) fn run_notify(
    title: &str,
    body: Option<&str>,
    level: NoteLevelArg,
    pane: Option<&str>,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let note = Note {
        level: match level {
            NoteLevelArg::Info => NoteLevel::Info,
            NoteLevelArg::Warn => NoteLevel::Warn,
            NoteLevelArg::Error => NoteLevel::Error,
        },
        title: title.to_owned(),
        body: body.map(str::to_owned),
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
    };
    let value = serde_json::to_vec(&note).unwrap_or_default();
    if value.len() > MAX_NOTE_BYTES {
        return usage_error(json, &format!("the note is over {MAX_NOTE_BYTES} bytes"));
    }
    if parse_note(&value).is_none() {
        return usage_error(json, "the note needs a title");
    }
    // Inside a pane the note is about that pane.
    let own = crate::environment::own_terminal_id().map(|id| format!("@{id}"));
    let target = pane.map(str::to_owned).or(own);
    with_target_pane(
        target.as_deref(),
        socket,
        "notify",
        None,
        json,
        move |conn, pane| {
            Box::pin(async move {
                let scope = Scope::Resource(pane.clone());
                conn.send(&FrameKind::SetMetadata {
                    request_id: 100,
                    scope: scope.clone(),
                    key: RESOURCE_NOTE_KEY.to_owned(),
                    value: value.clone(),
                })
                .await?;
                let (answer, interleaved) = conn
                    .request_metadata(101, scope, RESOURCE_NOTE_KEY.to_owned())
                    .await?
                    .into_parts();
                warn_interleaved_degradation(&phux_client::state::Degradation::from_interleaved(
                    &interleaved,
                ));
                let selector = phux_client::selector::format_terminal_id(&pane);
                if answer.ok().flatten().as_deref() != Some(value.as_slice()) {
                    return Ok(json_err::emit(
                        json,
                        &CliError::new(
                            codes::AGENT_RECORD_UNCONFIRMED,
                            format!("the note for {selector} could not be confirmed"),
                            "retry; run `phux doctor` if notes keep failing",
                        ),
                        EXIT_FAILURE,
                    ));
                }
                if json {
                    return Ok(crate::output::json(&serde_json::json!({
                        "schema_version": 1,
                        "terminal": selector,
                        "note": note,
                    })));
                }
                Ok(ExitCode::SUCCESS)
            })
        },
    )
}

fn usage_error(json: bool, message: &str) -> ExitCode {
    json_err::emit(
        json,
        &CliError::new(codes::USAGE_ERROR, message, "see `phux notify --help`"),
        EXIT_USAGE,
    )
}
