//! `TRANSCRIBE` (phux-ypsa): run the operator's transcriber on a finished
//! `PUT_FILE` upload and paste the text into a Terminal.
//!
//! The server embeds no speech model. `[voice] transcriber` is an argv the
//! server runs with the clip's sandboxed path substituted for `{path}`; its
//! stdout is the transcript. That is deliberately the same shape as a plugin
//! action (`phux_plugin::run_command_spec`: `kill_on_drop`, a deadline, no
//! shell), so wrapping a local ASR server is one `curl` line and no HTTP
//! client enters the server.
//!
//! The transcript is delivered as one acknowledged `APPLY_INPUT` paste under
//! a fresh operation id, so a reconnect between the reply and the paste can
//! not double it, and it inserts without submitting: the client shows the
//! text it got back and the user decides whether to press Enter.

use phux_protocol::ids::{FileUploadId, InputOperationId, TerminalId};
use phux_protocol::input::InputEvent;
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{CommandResult, CommandValue, ErrorCode};
use tracing::{debug, warn};

use super::commands::handle_route_input;
use super::input_lane::InputLaneHandle;
use crate::state::{ClientId, ServerState, SharedState};

/// Largest transcript the server will paste, in bytes. A transcriber that
/// prints a whole log file by mistake must not become a pane's input.
const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024;

pub(super) async fn handle_transcribe(
    state: &SharedState,
    client_id: ClientId,
    upload_id: FileUploadId,
    terminal_id: &TerminalId,
    input_lane: Option<&InputLaneHandle>,
) -> CommandResult {
    if terminal_id.host().is_some() {
        return refuse(
            ErrorCode::UnsupportedSatelliteRoute,
            "TRANSCRIBE pastes locally; a satellite-routed Terminal has no route",
        );
    }
    let voice = state.with(ServerState::voice);
    let started = std::time::Instant::now();
    let (text, transcribe_ms) = match run_transcriber(&voice, upload_id, client_id).await {
        Ok(result) => result,
        Err(refusal) => return refusal,
    };
    if text.is_empty() {
        debug!(?client_id, transcribe_ms, "transcriber returned no speech");
        return ok(&text, false, transcribe_ms, started);
    }
    match paste_transcript(state, client_id, terminal_id, input_lane, &text).await {
        CommandResult::Ok | CommandResult::OkWith(_) => ok(&text, true, transcribe_ms, started),
        error @ CommandResult::Error { .. } => error,
        _ => refuse(
            ErrorCode::InternalError,
            "paste returned an unexpected result",
        ),
    }
}

/// Resolve the upload, run the configured transcriber on it, and return the
/// trimmed transcript with the run's duration; every failure is already a
/// `CommandResult` refusal with its remedy.
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "`{path}` names the config token verbatim; these are plain messages, not format strings"
)]
async fn run_transcriber(
    voice: &phux_config::VoiceCfg,
    upload_id: FileUploadId,
    client_id: ClientId,
) -> Result<(String, u64), CommandResult> {
    if !voice.is_configured() {
        return Err(refuse(
            ErrorCode::InvalidCommand,
            "no transcriber configured on this server: set `[voice] transcriber = [...]` \
             in the server's config.toml (an argv; `{path}` is replaced by the clip)",
        ));
    }
    let clip = match super::upload::completed_upload_path(upload_id) {
        Ok(Some(path)) => path,
        Ok(None) => {
            return Err(refuse(
                ErrorCode::InvalidCommand,
                "no completed upload with that id: send the clip with PUT_FILE \
                 (final_chunk = true) first",
            ));
        }
        Err(message) => return Err(refuse(ErrorCode::InternalError, &message)),
    };
    let Some(argv) = voice.transcriber_argv(&clip) else {
        return Err(refuse(
            ErrorCode::InvalidCommand,
            "transcriber argv is empty",
        ));
    };
    let output = phux_plugin::run_command_spec(phux_plugin::CommandSpec {
        argv,
        cwd: None,
        env: Vec::new(),
        timeout: Some(voice.timeout()),
    })
    .await
    .map_err(|err| {
        refuse(
            ErrorCode::InternalError,
            &format!("transcriber could not be started: {err}"),
        )
    })?;
    let duration_ms = u64::try_from(output.duration_ms).unwrap_or(u64::MAX);
    if output.outcome == phux_plugin::PluginActionOutcome::TimedOut {
        warn!(?client_id, duration_ms, "transcriber timed out");
        return Err(refuse(
            ErrorCode::ResourceExhausted,
            &format!(
                "transcriber exceeded its {}s deadline (`[voice] timeout-secs`)",
                voice.timeout().as_secs()
            ),
        ));
    }
    if output.exit_code != Some(0) {
        let stderr = output.stderr.trim();
        warn!(?client_id, exit = ?output.exit_code, stderr, "transcriber failed");
        let exit = output
            .exit_code
            .map_or_else(|| "signal".to_owned(), |code| code.to_string());
        let detail = if stderr.is_empty() {
            "(no stderr)"
        } else {
            stderr
        };
        return Err(refuse(
            ErrorCode::InternalError,
            &format!("transcriber exited with {exit}: {detail}"),
        ));
    }
    let text = output.stdout.trim().to_owned();
    if text.len() > MAX_TRANSCRIPT_BYTES {
        return Err(refuse(
            ErrorCode::ResourceExhausted,
            "transcript exceeds the 64 KiB paste ceiling",
        ));
    }
    Ok((text, duration_ms))
}

/// Deliver the transcript as one acknowledged paste (or a fire-and-forget
/// paste when the server runs without an input lane, as some tests do).
async fn paste_transcript(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &TerminalId,
    input_lane: Option<&InputLaneHandle>,
    text: &str,
) -> CommandResult {
    let event = InputEvent::Paste(PasteEvent {
        trust: PasteTrust::Trusted,
        data: text.as_bytes().to_vec(),
    });
    match (input_lane, fresh_operation_id()) {
        (Some(lane), Some(operation_id)) => {
            lane.apply_input(client_id, operation_id, terminal_id.clone(), vec![event])
                .await
        }
        _ => handle_route_input(state, client_id, terminal_id, event),
    }
}

/// A fresh, non-zero `InputOperationId` for the acknowledged paste: the
/// dedupe cache only needs the id to be unique across this server's ten
/// minute retention, so a hash of the moment, the pid, and a counter is
/// enough and costs no new dependency.
fn fresh_operation_id() -> Option<InputOperationId> {
    use sha2::Digest as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"phux-voice-op");
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(SEQ.fetch_add(1, Ordering::Relaxed).to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    InputOperationId::new(bytes)
}

fn ok(text: &str, pasted: bool, transcribe_ms: u64, started: std::time::Instant) -> CommandResult {
    let body = serde_json::json!({
        "schema_version": 1,
        "text": text,
        "pasted": pasted,
        "transcribe_ms": transcribe_ms,
        "total_ms": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    });
    CommandResult::OkWith(CommandValue::Json(body.to_string()))
}

fn refuse(code: ErrorCode, message: &str) -> CommandResult {
    CommandResult::Error {
        code,
        message: message.to_owned(),
    }
}
