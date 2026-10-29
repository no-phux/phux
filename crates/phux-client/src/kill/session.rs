use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::{IdempotencyKey, SessionKey};
use phux_protocol::wire::frame::{Command, Refusal};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::org::{self, OrgError, OrgOutcome};
use crate::selector;
use crate::state::Degradation;

use super::selection::{KillError, Selected};
use super::{
    KeyedError, KillOutcome, kill_resource, kill_resource_keyed, kill_resources,
    kill_resources_keyed,
};

/// Whether the server behind `conn` takes `SESSION_KILL`.
#[must_use]
pub fn session_kill_supported(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features_ext
            .contains(ServerFeatureExt::SessionKill)
    })
}

/// Kill the whole session `session` with one `SESSION_KILL`.
///
/// The server ends every member, work that joined meanwhile included, and
/// deletes the session. A disconnect right after is the server exiting with
/// its last session: success.
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
    let label = format!("session {label:?}");
    let command = Command::SessionKill {
        operation: org::mint_operation(),
        session,
    };
    match org::request(conn, 2, command).await {
        Ok((OrgOutcome::Committed(receipt), degradation)) => Ok(Selected {
            interleaved: degradation.notices().to_vec(),
            killed: receipt.members,
        }),
        Ok((OrgOutcome::Refused(Refusal::NotFound), _)) => {
            Err(KillError::NoSuchTarget { target: label })
        }
        Ok((outcome, degradation)) => Err(KillError::Refused {
            label,
            message: match outcome {
                OrgOutcome::Refused(refusal) => org::refusal_sentence(&refusal),
                OrgOutcome::Error { message, .. } => message,
                other => format!("{other:?}"),
            },
            interleaved: degradation.notices().to_vec(),
        }),
        Err(OrgError::OutcomeUnknown { .. }) => Ok(Selected::default()),
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
        None => kill_resources(conn, 1, ids).await.map_err(KeyedError::from),
    };
    batch_outcome(reply, &format!("session {session:?}"))
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
    batch_outcome(reply, &format!("{target:?}"))
}

/// The result of one kill round trip that covered a whole target. A
/// disconnect is the server self-exiting after its last session was reaped,
/// which is success.
fn batch_outcome(
    reply: Result<(KillOutcome, Degradation), KeyedError>,
    label: &str,
) -> Result<Selected, KillError> {
    match reply {
        Ok((KillOutcome::Killed, degradation)) => Ok(Selected {
            interleaved: degradation.notices().to_vec(),
            killed: None,
        }),
        Err(KeyedError::Attach(AttachError::Disconnected)) => Ok(Selected::default()),
        Err(KeyedError::Unsupported) => Err(KillError::UnsupportedKeyedSignal),
        Ok((KillOutcome::Refused(message), degradation)) => Err(KillError::Refused {
            label: label.to_owned(),
            message,
            interleaved: degradation.notices().to_vec(),
        }),
        Ok((KillOutcome::Unexpected(other), degradation)) => Err(KillError::Unexpected {
            label: label.to_owned(),
            message: crate::explain::explain_unexpected("kill", &other),
            interleaved: degradation.notices().to_vec(),
        }),
        Err(KeyedError::Attach(err)) => Err(err.into()),
    }
}

/// Kill each Terminal in turn: every refusal is collected rather than
/// stopping the loop, and a disconnect means the remaining targets are
/// already gone.
pub(super) async fn kill_each_terminal(
    conn: &mut Connection,
    terminals: Vec<ResourceId>,
) -> Result<Selected, KillError> {
    let mut refusals = Vec::new();
    let mut interleaved = Vec::new();
    for (i, terminal) in terminals.into_iter().enumerate() {
        let request_id = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
        match kill_one(conn, request_id, terminal).await {
            KillStep::Killed(notices) => interleaved.extend(notices),
            KillStep::Refused { message, notices } => {
                interleaved.extend(notices);
                refusals.push(message);
            }
            KillStep::ServerGone => break,
        }
    }
    if refusals.is_empty() {
        Ok(Selected {
            interleaved,
            killed: None,
        })
    } else {
        Err(KillError::PaneRefusals {
            messages: refusals,
            interleaved,
        })
    }
}

/// How one `KILL_RESOURCE` ended.
enum KillStep {
    Killed(Vec<String>),
    Refused {
        message: String,
        notices: Vec<String>,
    },
    ServerGone,
}

async fn kill_one(conn: &mut Connection, request_id: u32, terminal: ResourceId) -> KillStep {
    let label = selector::format_terminal_id(&terminal);
    match kill_resource(conn, request_id, terminal).await {
        Ok((KillOutcome::Killed, degradation)) => KillStep::Killed(degradation.notices().to_vec()),
        Ok((KillOutcome::Refused(message), degradation)) => KillStep::Refused {
            message: format!("kill refused for {label}: {message}"),
            notices: degradation.notices().to_vec(),
        },
        Ok((KillOutcome::Unexpected(other), degradation)) => KillStep::Refused {
            message: format!(
                "{label}: {}",
                crate::explain::explain_unexpected("kill", &other)
            ),
            notices: degradation.notices().to_vec(),
        },
        Err(AttachError::Disconnected) => KillStep::ServerGone,
        Err(err) => KillStep::Refused {
            message: format!("kill failed for {label}: {err}"),
            notices: Vec::new(),
        },
    }
}
