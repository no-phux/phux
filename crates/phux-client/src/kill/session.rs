use phux_protocol::ResourceId;
use phux_protocol::ids::{IdempotencyKey, SessionKey};
use phux_protocol::wire::frame::{Command, Refusal};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::org::{self, OrgError, OrgOutcome};
use crate::selector;
use crate::state::Degradation;

use super::selection::{KillError, KillIdentity, Selected};
use super::{
    KillOutcome, kill_resource, kill_resource_keyed, kill_resources, kill_resources_keyed,
};

/// Kill the whole session `session` with one `SESSION_KILL`.
///
/// A receipt acknowledges close intent; a disconnect cannot prove an end.
///
/// # Errors
///
/// [`KillError::NoSuchTarget`] when the session is gone,
/// [`KillError::Refused`] for any other refusal, [`KillError::Org`] for a
/// transport failure.
pub async fn kill_session(
    conn: &mut Connection,
    session: SessionKey,
    label: &str,
) -> Result<Selected, KillError> {
    kill_session_keyed(conn, session, label, org::mint_operation()).await
}

pub(super) async fn kill_session_keyed(
    conn: &mut Connection,
    session: SessionKey,
    label: &str,
    operation: IdempotencyKey,
) -> Result<Selected, KillError> {
    let label = format!("session {label:?}");
    let command = Command::SessionKill { operation, session };
    match org::request(conn, 2, command).await {
        Ok((OrgOutcome::Committed(receipt), degradation)) => Ok(Selected {
            recovery: Some(KillIdentity::new(Some(operation)).recovery(crate::recovery::EffectCertainty::Applied)),
            interleaved: degradation.notices().to_vec(),
            killed: receipt.members,
        }),
        Ok((OrgOutcome::Refused(Refusal::NotFound), _)) => {
            Err(KillError::NoSuchTarget { target: label })
        }
        Ok((OrgOutcome::NotSent { reason }, degradation)) => Err(KillError::NotSent {
            target: label,
            identity: KillIdentity::new(Some(operation)),
            message: reason,
            interleaved: degradation.notices().to_vec(),
        }),
        Ok((OrgOutcome::Accepted, degradation)) => Ok(Selected {
            recovery: Some(KillIdentity::new(Some(operation)).recovery(crate::recovery::EffectCertainty::Applied)),
            interleaved: degradation.notices().to_vec(),
            killed: None,
        }),
        Ok((OrgOutcome::Unknown { reason }, degradation)) => Err(unknown(
            &label,
            Some(operation),
            reason,
            degradation.notices().to_vec(),
        )),
        Ok((OrgOutcome::Value(value), degradation)) => Err(unknown(
            &label,
            Some(operation),
            format!("unexpected reply: {value:?}"),
            degradation.notices().to_vec(),
        )),
        Ok((outcome, degradation)) => Err(KillError::Refused {
            identity: KillIdentity::new(Some(operation)),
            label,
            message: match outcome {
                OrgOutcome::Refused(refusal) => org::refusal_sentence(&refusal),
                OrgOutcome::Error { message, .. } => message,
                other => format!("{other:?}"),
            },
            interleaved: degradation.notices().to_vec(),
        }),
        Err(err @ OrgError::OutcomeUnknown { .. }) => Err(KillError::OutcomeUnknown {
            results: None,
            target: label,
            identity: KillIdentity::new(Some(operation)),
            message: err.to_string(),
            interleaved: Vec::new(),
        }),
        Err(err) => Err(KillError::Org(err)),
    }
}

/// One `KILL_RESOURCES` for a whole session, keyed when `key` is set.
pub(super) async fn kill_whole_session(
    conn: &mut Connection,
    session: &str,
    ids: Vec<ResourceId>,
    key: Option<IdempotencyKey>,
) -> Result<Selected, KillError> {
    let reply = match key {
        Some(key) => kill_resources_keyed(conn, 1, ids, key).await,
        None => kill_resources(conn, 1, ids).await,
    };
    batch_outcome(reply, &format!("session {session:?}"), key)
}

/// Kill `terminals` as one keyed command: `KILL_RESOURCE` for one,
/// `KILL_RESOURCES` for several, so the key names the whole operation.
pub(super) async fn kill_keyed(
    conn: &mut Connection,
    target: &str,
    mut terminals: Vec<ResourceId>,
    key: IdempotencyKey,
) -> Result<Selected, KillError> {
    let reply = if terminals.len() == 1 {
        let terminal = terminals.remove(0);
        kill_resource_keyed(conn, 1, terminal, key).await
    } else {
        kill_resources_keyed(conn, 1, terminals, key).await
    };
    batch_outcome(reply, target, Some(key))
}

/// Classify a whole-target close without inferring destruction from EOF.
fn batch_outcome(
    reply: Result<(KillOutcome, Degradation), AttachError>,
    label: &str,
    key: Option<IdempotencyKey>,
) -> Result<Selected, KillError> {
    match reply {
        Ok((KillOutcome::Killed, degradation)) => Ok(Selected {
            recovery: key.map(|key| KillIdentity::new(Some(key)).recovery(crate::recovery::EffectCertainty::Applied)),
            interleaved: degradation.notices().to_vec(),
            killed: None,
        }),
        Ok((KillOutcome::Partial(results), degradation)) => Err(KillError::Partial {
            target: label.to_owned(),
            results: Box::new(results),
            identity: KillIdentity::new(key),
            interleaved: degradation.notices().to_vec(),
        }),
        Ok((KillOutcome::Refused(message), degradation)) => Err(KillError::Refused {
            label: label.to_owned(),
            identity: KillIdentity::new(key),
            message,
            interleaved: degradation.notices().to_vec(),
        }),
        Ok((KillOutcome::Unexpected(other), degradation)) => Err(KillError::Unexpected {
            label: label.to_owned(),
            message: crate::explain::explain_unexpected("kill", &other),
            interleaved: degradation.notices().to_vec(),
        }),
        Err(err) => Err(unknown(label, key, err.to_string(), Vec::new())),
    }
}

/// Stop at uncertainty; retain earlier receipts and every unattempted target.
pub(super) async fn kill_each_terminal(
    conn: &mut Connection,
    terminals: Vec<ResourceId>,
    target: &str,
) -> Result<Selected, KillError> {
    let mut interleaved = Vec::new();
    let mut failures = Vec::new();
    let mut accepted = Vec::new();
    let mut remaining = terminals.into_iter().enumerate();
    while let Some((i, terminal)) = remaining.next() {
        let label = selector::format_terminal_id(&terminal);
        let request_id = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
        let reply = kill_resource(conn, request_id, terminal).await;
        match batch_outcome(reply, &label, None) {
            Ok(done) => {
                interleaved.extend(done.interleaved);
                accepted.push(label);
            }
            Err(mut error) => {
                interleaved.extend(error.take_interleaved());
                let uncertain = matches!(
                    error,
                    KillError::OutcomeUnknown { .. } | KillError::Unexpected { .. }
                );
                failures.push(error.outcome_document().unwrap_or_else(|| serde_json::json!({
                    "error": {"target": label, "code": "outcome_unknown", "message": error.to_string()},
                })));
                if uncertain {
                    let not_sent = remaining
                        .map(|(_, id)| selector::format_terminal_id(&id))
                        .collect::<Vec<_>>();
                    let mut error = unknown(
                        target,
                        None,
                        "a close reply was not confirmed; earlier closes may have been accepted"
                            .into(),
                        interleaved,
                    );
                    if let KillError::OutcomeUnknown { results, .. } = &mut error {
                        *results = Some(Box::new(serde_json::json!({
                            "failed": failures, "not_sent": not_sent, "accepted": accepted,
                        })));
                    }
                    return Err(error);
                }
            }
        }
    }
    if failures.is_empty() {
        Ok(Selected {
            recovery: None,
            interleaved,
            killed: None,
        })
    } else {
        Err(KillError::Partial {
            target: target.to_owned(),
            results: Box::new(serde_json::json!({"failed": failures, "accepted": accepted})),
            identity: KillIdentity::new(None),
            interleaved,
        })
    }
}

fn unknown(
    target: &str,
    key: Option<IdempotencyKey>,
    message: String,
    interleaved: Vec<String>,
) -> KillError {
    KillError::OutcomeUnknown {
        target: target.to_owned(),
        identity: KillIdentity::new(key),
        message,
        results: None,
        interleaved,
    }
}

