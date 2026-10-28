use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};
use phux_protocol::ids::ResourceId;
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, json_err, parse_selector, resolve_target_for_input};

/// Version of the `send-keys --json` and `paste --json` receipts.
pub(crate) const RECEIPT_SCHEMA_VERSION: u8 = 1;

/// `phux send-keys TARGET KEYS...` — send input to a pane via the
/// side-effect-free `ROUTE_INPUT` route.
///
/// `TARGET` is the full selector grammar (`docs/concepts.md#targets`):
/// `session`, `session:group`, `session:group.pane`, `@id`, `.`. It
/// is resolved client-side to a single pane (the selected one — the focused
/// pane when the selector spans several), then the events route to exactly
/// that pane by id, so this neither attaches nor resizes the live pane
/// (phux-n95; see [`phux_client::send_keys::send_to`]).
pub(crate) fn run_send_keys(
    target: &str,
    keys: &[String],
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let selector = match parse_selector(Some(target)) {
        Ok(sel) => sel,
        Err(code) => return code,
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    rt.block_on(async move {
        let pane = match resolve_target_for_input(&socket_path, &selector, "send-keys", json).await
        {
            Ok(id) => id,
            Err(code) => return code,
        };
        match phux_client::send_keys::send_counted(&socket_path, &pane, keys).await {
            Ok(events) => report_receipt(json, &pane, events),
            Err(failure) => report_input_failure(
                json,
                target,
                "send-keys",
                &socket_path,
                failure.delivered,
                &failure.error,
            ),
        }
    })
}

/// The receipt for input the server acknowledged: `{schema_version, sent,
/// terminal_id, events}` under `--json`, silence otherwise.
pub(crate) fn report_receipt(json: bool, pane: &ResourceId, events: usize) -> ExitCode {
    if !json {
        return ExitCode::SUCCESS;
    }
    crate::output::json(&serde_json::json!({
        "schema_version": RECEIPT_SCHEMA_VERSION,
        "sent": true,
        "terminal_id": pane.local_id(),
        "events": events,
    }))
}

/// A failed `ROUTE_INPUT`. Under `--json`, the error carries what reached
/// the pane: nothing after a refusal of the first event, an unknown part
/// once any event was acknowledged.
pub(crate) fn report_input_failure(
    json: bool,
    target: &str,
    verb: &str,
    socket_path: &Path,
    delivered: usize,
    err: &AttachError,
) -> ExitCode {
    if !json {
        return match err {
            AttachError::Io(_) => crate::commands::report_no_server(err, socket_path, verb),
            AttachError::Refused(msg) => {
                error!("{verb} refused for '{target}': {msg} (try `phux ls`)");
                ExitCode::FAILURE
            }
            other => {
                error!("{verb} failed: {other}");
                ExitCode::FAILURE
            }
        };
    }
    json_err::emit(
        true,
        &failure_error(verb, socket_path, delivered, err),
        crate::exit_codes::EXIT_FAILURE,
    )
}

/// The `--json` error for a failed send. A refusal or a lost connection:
/// the server's own reason is in the message; the stable part is what
/// reached the pane.
fn failure_error(
    verb: &str,
    socket_path: &Path,
    delivered: usize,
    err: &AttachError,
) -> json_err::CliError {
    let (certainty, next) = if delivered == 0 && !matches!(err, AttachError::Disconnected) {
        (EffectCertainty::NotApplied, NextAction::RetryNewOperation)
    } else {
        (EffectCertainty::Unknown, NextAction::Reconcile)
    };
    json_err::no_server_error(err, socket_path, verb).with_recovery(Recovery::new(
        ScopeKind::InputServerIncarnation,
        None,
        certainty,
        next,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What reached the pane decides the recovery: a refusal before any
    /// event is a safe retry; anything acknowledged, or a dropped
    /// connection, has to be reconciled by reading the pane.
    #[test]
    fn a_failed_send_says_what_reached_the_pane() {
        let socket = Path::new("/tmp/phux-test.sock");
        for (delivered, err, certainty) in [
            (
                0,
                AttachError::Refused("no".to_owned()),
                EffectCertainty::NotApplied,
            ),
            (
                1,
                AttachError::Refused("no".to_owned()),
                EffectCertainty::Unknown,
            ),
            (0, AttachError::Disconnected, EffectCertainty::Unknown),
        ] {
            let doc =
                json_err::error_document(&failure_error("send-keys", socket, delivered, &err), 1);
            assert_eq!(
                doc["error"]["recovery"]["effect_certainty"],
                serde_json::json!(certainty),
                "{delivered} {err:?}",
            );
        }
    }
}
