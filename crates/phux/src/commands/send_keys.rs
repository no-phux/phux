use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::read::Verb;
use phux_client::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};
use phux_protocol::ids::ResourceId;

use crate::commands::ctx::{self, Ctx};
use crate::commands::json_err::Refusal;
use crate::commands::{parse_target, resolve_target_for_input};

/// Version of the `send-keys --json` and `paste --json` receipts.
pub(crate) const RECEIPT_SCHEMA_VERSION: u8 = 1;

/// `phux send-keys TARGET KEYS...` — send input to a pane via the
/// side-effect-free `ROUTE_INPUT` route.
pub(crate) fn run_send_keys(
    target: &str,
    keys: &[String],
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    ctx::run_local(socket, json, |ctx| {
        let selector = parse_target(Some(target))?;
        ctx.rt.block_on(async {
            // Keys reach a Terminal only; a document's input is typed intents.
            let pane = resolve_target_for_input(ctx.socket(), &selector, Verb::SendKeys).await?;
            let events = phux_client::send_keys::send_counted(ctx.socket(), &pane, keys)
                .await
                .map_err(|failure| {
                    input_failure(ctx, target, "send-keys", failure.delivered, &failure.error)
                })?;
            Ok(report_receipt(json, &pane, events))
        })
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

/// A failed `ROUTE_INPUT`, exit 1. Under `--json`, the error carries what
/// reached the pane: nothing after a refusal of the first event, an unknown
/// part once any event was acknowledged.
pub(crate) fn input_failure(
    ctx: &Ctx<PathBuf>,
    target: &str,
    verb: &str,
    delivered: usize,
    err: &AttachError,
) -> Refusal {
    let refusal = ctx
        .unreachable(err, verb)
        .with_recovery(input_recovery(delivered, err));
    match err {
        AttachError::Io(_) => refusal,
        AttachError::Refused(msg) | AttachError::WrongResourceKind(msg) => {
            refusal.with_human(vec![format!(
                "{verb} refused for '{target}': {msg} (try `phux ls`)"
            )])
        }
        other => refusal.with_human(vec![format!("{verb} failed: {other}")]),
    }
}

/// What a retry of a failed send may assume. A refusal or a lost
/// connection: the server's own reason is in the message; the stable part
/// is what reached the pane.
const fn input_recovery(delivered: usize, err: &AttachError) -> Recovery {
    let (certainty, next) = if delivered == 0 && !matches!(err, AttachError::Disconnected) {
        (EffectCertainty::NotApplied, NextAction::RetryNewOperation)
    } else {
        (EffectCertainty::Unknown, NextAction::Reconcile)
    };
    Recovery::new(ScopeKind::InputServerIncarnation, None, certainty, next)
}

/// Run `phux send-keys` as parsed.
pub(crate) fn run(
    args: crate::commands::SendKeysArgs,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    let crate::commands::SendKeysArgs { target, keys, json } = args;
    run_send_keys(&target, &keys, json.json, socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What reached the pane decides the recovery: a refusal before any
    /// event is a safe retry; anything acknowledged, or a dropped
    /// connection, has to be reconciled by reading the pane.
    #[test]
    fn a_failed_send_says_what_reached_the_pane() {
        let ctx = Ctx::local(Some(PathBuf::from("/tmp/phux-test.sock")), true).unwrap();
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
            let refusal = input_failure(&ctx, "@1", "send-keys", delivered, &err);
            let doc = crate::commands::json_err::error_document(&refusal.err, refusal.exit);
            assert_eq!(
                doc["error"]["recovery"]["effect_certainty"],
                serde_json::json!(certainty),
                "{delivered} {err:?}",
            );
        }
    }
}
