//! Rename a session on the connected server (`phux.session.name/v1`,
//! `docs/protocol.md#commands-events-resources`), with the shared `phux_client_core::rename`
//! policy: judge against the latest session list, send `SET_METADATA`
//! (`current\0new`), then a `GET_STATE` barrier whose answer is the outcome.
//!
//! The first rename subscribes to the key so this client applies every
//! later rename to its session list; `phux_client_follow_session_names`
//! subscribes at `HELLO_OK` instead. No attach is needed for either.
//!
//! Against a server that advertises `ORGANIZATION` none of that metadata
//! surface is used (ADR-0161 section 9): a rename is one typed
//! `SESSION_RENAME`, and every rename reaches the session list as a
//! journal `SessionRenamed` the organization replica applies
//! (`organization::changed`). An attach subscribes the event stream that
//! carries it; following without one subscribes it once the replica is live.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared by the bridge dispatcher and Client"
)]

use std::mem;

use crate::c::client::Client;
use crate::c::error::{BridgeError, bytes_in, check_struct};
use crate::c::types::{PhuxBytes, bytes_out};
use crate::c::{PhuxClient, PhuxClientResult, with_client_mut, with_client_ref};
use phux_client_core::org_ops::OrgOutcome;
use phux_client_core::rename::{
    BarrierVerdict, NamedSession, RenamePlan, barrier_verdict, check_session_name_text,
    plan_rename, rename_frames,
};
use phux_protocol::ids::{OperationId, SessionId};
use phux_protocol::wire::frame::{
    CommandResult, CommandValue, ErrorCode, FrameKind, SESSION_NAME_KEY, Scope,
};

/// No rename has been asked on this client.
const STATUS_NONE: u32 = 0;
/// The write is queued or on the wire; its confirmation read is not answered.
const STATUS_PENDING: u32 = 1;
/// The server applied it (or the name was already the session's).
const STATUS_RENAMED: u32 = 2;
/// Refused, here or by the server; `message` says why.
const STATUS_REFUSED: u32 = 3;
/// The connection ended before the confirmation read was answered.
const STATUS_UNKNOWN_OUTCOME: u32 = 4;

mod abi;
pub use abi::PhuxSessionRenameInfo;
pub use abi::*;

/// The one rename this client may have outstanding, and what it has heard.
pub(crate) struct SessionRename {
    request_id: u32,
    status: u32,
    session_id: u32,
    new_name: Vec<u8>,
    current: Vec<u8>,
    /// The confirmation `GET_STATE`, kept until answered even once the
    /// rename settled, so its reply is consumed here and nowhere else.
    barrier: Option<u32>,
    message: Vec<u8>,
    /// Whether this client has subscribed to the rename key.
    subscribed: bool,
    /// Subscribe as soon as the connection is negotiated, not at the first
    /// rename (`phux_client_follow_session_names`).
    follow: bool,
    /// Bumped whenever a rename changed the session list in place.
    sessions_revision: u64,
    /// The typed `SESSION_RENAME` this rename submitted, against a server
    /// that advertises `ORGANIZATION`.
    operation: Option<OperationId>,
}

impl Default for SessionRename {
    fn default() -> Self {
        Self {
            request_id: 0,
            status: STATUS_NONE,
            session_id: 0,
            new_name: Vec::new(),
            current: Vec::new(),
            barrier: None,
            message: Vec::new(),
            subscribed: false,
            follow: false,
            sessions_revision: 0,
            operation: None,
        }
    }
}

impl SessionRename {
    const fn pending(&self) -> bool {
        self.status == STATUS_PENDING
    }

    fn settle(&mut self, status: u32, message: &str) {
        self.status = status;
        self.message = message.as_bytes().to_vec();
    }

    /// A pending rename can no longer be confirmed on this connection.
    pub(crate) fn disconnect(&mut self) {
        self.barrier = None;
        self.operation = None;
        if self.pending() {
            self.settle(
                STATUS_UNKNOWN_OUTCOME,
                "the connection ended before the rename was confirmed",
            );
        }
    }
}

/// Consume a rename's frames: every `METADATA_CHANGED` of the rename key, a
/// correlated refusal of the write, and the confirmation read's reply. Runs
/// ahead of the arrangement and operations dispatchers, which would read the
/// first as a stray frame and the others as uncorrelated.
pub(crate) fn dispatch(
    client: &mut Client,
    frame: FrameKind,
) -> Result<Option<FrameKind>, BridgeError> {
    match frame {
        FrameKind::MetadataChanged {
            scope: Scope::Global,
            key,
            value,
            ..
        } if key == SESSION_NAME_KEY => {
            if let Some(value) = value {
                applied(client, &value);
            }
            Ok(None)
        }
        FrameKind::Error {
            request_id: Some(request_id),
            code,
            message,
        } if awaits(client, request_id) => {
            if client.session_rename.barrier == Some(request_id) {
                client.session_rename.barrier = None;
            }
            if client.session_rename.pending() {
                client.session_rename.settle(error_status(code), &message);
            }
            Ok(None)
        }
        FrameKind::CommandResult { request_id, result }
            if client.session_rename.barrier == Some(request_id) =>
        {
            client.session_rename.barrier = None;
            confirmed(client, result)?;
            Ok(None)
        }
        other => Ok(Some(other)),
    }
}

/// How an error answer settles a rename: refused, except a code this build
/// does not know (`UNREADABLE_REPLY` included), which does not say whether
/// the rename happened.
const fn error_status(code: ErrorCode) -> u32 {
    if matches!(code, ErrorCode::Unknown(_)) {
        STATUS_UNKNOWN_OUTCOME
    } else {
        STATUS_REFUSED
    }
}

fn awaits(client: &Client, request_id: u32) -> bool {
    let rename = &client.session_rename;
    rename.barrier == Some(request_id) || (rename.pending() && rename.request_id == request_id)
}

/// The server broadcast an applied rename: `current\0new`. Whoever asked for
/// it, the list follows; a malformed value changes nothing.
fn applied(client: &mut Client, value: &[u8]) {
    let Some(split) = value.iter().position(|&byte| byte == 0) else {
        return;
    };
    let (current, new_name) = (&value[..split], &value[split + 1..]);
    if valid_name(new_name).is_err() {
        return;
    }
    let current_owned = current.to_vec();
    if crate::c::arrangement::rename_sessions(client, |s| s.name == current_owned, new_name) {
        client.session_rename.sessions_revision += 1;
    }
    let rename = &mut client.session_rename;
    if rename.pending() && rename.current == current && rename.new_name == new_name {
        rename.settle(STATUS_RENAMED, "");
    }
}

/// The confirmation read: the session carries the new name or it does not.
fn confirmed(client: &mut Client, result: CommandResult) -> Result<(), BridgeError> {
    let snapshot = match result {
        CommandResult::OkWith(CommandValue::State(snapshot)) => snapshot,
        CommandResult::Error { code, message } => {
            if client.session_rename.pending() {
                client.session_rename.settle(error_status(code), &message);
            }
            return Ok(());
        }
        _ => {
            return Err(BridgeError::protocol(
                "a rename's GET_STATE was answered with an unexpected value",
            ));
        }
    };
    let id = client.session_rename.session_id;
    let roster: Vec<NamedSession<'_>> = snapshot
        .sessions
        .iter()
        .map(|session| NamedSession {
            id: session.id,
            name: session.name.as_str(),
        })
        .collect();
    let verdict = barrier_verdict(&roster, SessionId::new(id), {
        std::str::from_utf8(&client.session_rename.new_name).unwrap_or("")
    });
    let Some(session) = snapshot.sessions.iter().find(|s| s.id.get() == id) else {
        if client.session_rename.pending() {
            client.session_rename.settle(
                STATUS_REFUSED,
                BarrierVerdict::Gone
                    .refusal_reason()
                    .unwrap_or("the session no longer exists"),
            );
        }
        return Ok(());
    };
    let name = session.name.as_bytes().to_vec();
    // The server's name for it wins, whether or not a METADATA_CHANGED said so.
    if valid_name(&name).is_ok()
        && crate::c::arrangement::rename_sessions(client, |s| s.session_id == id, &name)
    {
        client.session_rename.sessions_revision += 1;
    }
    let rename = &mut client.session_rename;
    if !rename.pending() {
        return Ok(());
    }
    match verdict.refusal_reason() {
        None => rename.settle(STATUS_RENAMED, ""),
        Some(reason) => rename.settle(STATUS_REFUSED, reason),
    }
    Ok(())
}

/// A session name as the server stores one: UTF-8 that
/// `check_session_name_text` accepts.
///
/// This judges an existing name (the current one, or one the server
/// reported). The new name of a rename is judged by `check_session_name` in
/// `plan_rename`, which settles a selector-grammar refusal REFUSED.
pub(crate) fn valid_name(name: &[u8]) -> Result<(), BridgeError> {
    let name = std::str::from_utf8(name)
        .map_err(|_| BridgeError::invalid("a session name must be UTF-8"))?;
    check_session_name_text(name).map_err(|invalid| BridgeError::invalid(invalid.to_string()))
}

/// The shared rename plan against this client's session list.
///
/// `current` and `new_name` are already validated UTF-8. A catalog name that
/// is not UTF-8 cannot match either side and is ignored.
fn plan(client: &Client, current: &str, new_name: &str) -> RenamePlan {
    let roster: Vec<NamedSession<'_>> = client
        .sessions
        .iter()
        .filter_map(|session| {
            Some(NamedSession {
                id: SessionId::new(session.session_id),
                name: std::str::from_utf8(&session.name).ok()?,
            })
        })
        .collect();
    plan_rename(&roster, current, new_name)
}

/// Whether the negotiated server advertises `ORGANIZATION`, whose renames
/// arrive through the organization replica rather than the metadata key.
fn organization_advertised(client: &Client) -> bool {
    client.control().organization_supported()
}

/// Subscribe once per client to what carries renames. Read-only: it
/// attaches nothing and sizes nothing.
///
/// Against a server that advertises `ORGANIZATION` that is the event stream,
/// whose `SessionRenamed` the replica applies to the session list. An attach
/// already subscribes it; a client that never attaches subscribes here, once
/// the replica is live, from the seed's journal cut so nothing between the
/// seed and the subscription is missed. Otherwise it is the rename key.
fn subscribe(client: &mut Client) -> Result<(), BridgeError> {
    if client.session_rename.subscribed {
        return Ok(());
    }
    if organization_advertised(client) {
        return subscribe_events(client);
    }
    client.queue_frame(&FrameKind::SubscribeMetadata {
        scope: Scope::Global,
        key: SESSION_NAME_KEY.to_owned(),
    })?;
    client.session_rename.subscribed = true;
    Ok(())
}

/// The `ORGANIZATION` half of [`subscribe`]: wait for the replica's cut, then
/// ask for the server-wide event stream unless an attach already did.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the control guard lives exactly as long as the replica borrow it lends"
)]
fn subscribe_events(client: &mut Client) -> Result<(), BridgeError> {
    if client.attached {
        client.session_rename.subscribed = true;
        return Ok(());
    }
    let cut = {
        let control = client.control();
        let replica = control.organization();
        replica.is_live().then(|| replica.journal_seq())
    };
    let Some(cut) = cut else {
        // `negotiated` retries on every frame until the seed lands.
        return Ok(());
    };
    client.queue_frame(&FrameKind::SubscribeEvents {
        terminal: None,
        after_seq: client.event_journal.then_some(cut),
    })?;
    client.session_rename.subscribed = true;
    Ok(())
}

/// A frame was applied on the negotiated connection: a client following
/// renames subscribes as soon as it can (at `HELLO_OK`, or against an
/// `ORGANIZATION` server once the replica is live).
pub(crate) fn negotiated(client: &mut Client) -> Result<(), BridgeError> {
    if client.session_rename.follow {
        subscribe(client)?;
    }
    Ok(())
}

fn queue_rename(
    client: &mut Client,
    request_id: u32,
    current: &str,
    new_name: &str,
) -> Result<(), BridgeError> {
    subscribe(client)?;
    let barrier = client.arrangement.reserve_internal()?;
    let (write, barrier_frame) = rename_frames(request_id, barrier, current, new_name);
    client.queue_frame(&write)?;
    client.queue_frame(&barrier_frame)?;
    client.session_rename.barrier = Some(barrier);
    Ok(())
}

/// Against a server that advertises `ORGANIZATION`, rename by the typed
/// `SESSION_RENAME` (the last writer wins); the runtime's journal holds it
/// until a seed names the authority. False only against a
/// server that does not advertise it, where the legacy metadata path carries
/// the rename; an `ORGANIZATION` server is never sent the legacy key.
fn submit_typed(client: &mut Client, session_id: u32, new_name: &str) -> Result<bool, BridgeError> {
    if !organization_advertised(client) {
        return Ok(false);
    }
    let key = client
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .and_then(|session| session.key)
        .ok_or_else(|| BridgeError::state("the session list carries no key for that session"))?;
    let operation = client
        .control()
        .rename_session(key, new_name)
        .map_err(|error| crate::c::organization::submit_error(&error))?;
    client.session_rename.operation = Some(operation);
    client.process_runtime_events()?;
    Ok(true)
}

/// A typed rename resolved. True when `operation` was this client's rename.
pub(crate) fn organization_resolved(
    client: &mut Client,
    operation: OperationId,
    outcome: &OrgOutcome,
) -> bool {
    if client.session_rename.operation != Some(operation) {
        return false;
    }
    client.session_rename.operation = None;
    let resolution = crate::projection::organization::resolution(outcome);
    match resolution.status {
        crate::projection::organization::Status::Committed
        | crate::projection::organization::Status::Accepted => {
            let id = client.session_rename.session_id;
            let name = client.session_rename.new_name.clone();
            if crate::c::arrangement::rename_sessions(client, |s| s.session_id == id, &name) {
                bump_sessions_revision(client);
            }
            client.session_rename.settle(STATUS_RENAMED, "");
        }
        crate::projection::organization::Status::Unknown => {
            client
                .session_rename
                .settle(STATUS_UNKNOWN_OUTCOME, &resolution.message);
        }
        _ => client
            .session_rename
            .settle(STATUS_REFUSED, &resolution.message),
    }
    true
}

/// A rename, from this client or another, changed the session list in place.
pub(crate) const fn bump_sessions_revision(client: &mut Client) {
    client.session_rename.sessions_revision += 1;
}

#[cfg(test)]
mod tests;
