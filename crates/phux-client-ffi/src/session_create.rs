//! Empty, keep-empty session creation over the CLI's owning-connection metadata
//! operation. A nonce result binds the server-owned ID; an ordered state read
//! verifies that exact session. No attach, shell, or second coordinator is involved.
#![allow(clippy::redundant_pub_crate, reason = "private bridge module")]

use std::collections::BTreeMap;
use std::mem;

use crate::client::Client;
use crate::error::{BridgeError, bytes_in, check_struct};
use crate::types::{PhuxBytes, bytes_out};
use crate::{PhuxClient, PhuxClientResult, with_client_mut, with_client_ref};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, SESSION_CREATE_KEY,
    SESSION_CREATE_RESULT_KEY_PREFIX, Scope, StateScope,
};

const PENDING: u32 = 1;
const CREATED: u32 = 2;
const REFUSED: u32 = 3;
const UNKNOWN: u32 = 4;

#[derive(Default)]
pub(crate) struct SessionCreates {
    entries: BTreeMap<u32, Creation>,
}

struct Creation {
    name: String,
    token: String,
    result_read: u32,
    state_read: u32,
    confirmed: bool,
    receipt_id: Option<u32>,
    state_finished: bool,
    released: bool,
    status: u32,
    session_id: u32,
    message: Vec<u8>,
}

impl Creation {
    fn unknown(&mut self, message: &str) {
        self.status = UNKNOWN;
        self.message = message.as_bytes().to_vec();
    }

    fn refuse(&mut self, message: &str) {
        if self.status == UNKNOWN {
            return;
        }
        if self.confirmed {
            self.unknown("Session creation was confirmed but its current state is unavailable. Refresh Sessions to check the outcome.");
            return;
        }
        self.status = REFUSED;
        self.message = message.as_bytes().to_vec();
    }

    fn confirm(&mut self, value: Option<Vec<u8>>) {
        let Some(value) = value else {
            self.refuse("the server did not create the session; the name may already exist");
            return;
        };
        if matches_result(&value, &self.name, &self.token) {
            self.confirmed = true;
            if let Some(id) = receipt_session_id(&value) {
                self.receipt_id = Some(id);
            } else {
                self.unknown("The server confirmed creation without an exact session identity. Refresh Sessions to find it.");
            }
        } else {
            self.unknown("The server returned an invalid session creation receipt. Refresh Sessions to check the outcome.");
        }
    }
}

impl SessionCreates {
    pub(crate) fn disconnect(&mut self) {
        self.entries.retain(|_, entry| !entry.released);
        for entry in self.entries.values_mut() {
            entry.state_finished = true;
            if entry.status == PENDING {
                entry.status = UNKNOWN;
                entry.message = b"connection ended before session creation was confirmed".to_vec();
            }
        }
    }

    fn owner(&self, id: u32) -> Option<u32> {
        self.entries.iter().find_map(|(&request, entry)| {
            (request == id || entry.result_read == id || entry.state_read == id).then_some(request)
        })
    }
}

fn receipt_session_id(value: &[u8]) -> Option<u32> {
    let doc: serde_json::Value = serde_json::from_slice(value).ok()?;
    u32::try_from(doc["session_id"].as_u64()?)
        .ok()
        .filter(|id| *id != 0)
}

fn matches_result(value: &[u8], name: &str, token: &str) -> bool {
    let Ok(doc) = serde_json::from_slice::<serde_json::Value>(value) else {
        return false;
    };
    doc["name"].as_str() == Some(name)
        && doc["request_token"].as_str() == Some(token)
        && doc["empty"].as_bool() == Some(true)
        && doc.get("terminal_id") == Some(&serde_json::Value::Null)
}

/// Consume only the exact operation's replies, before workspace dispatch.
pub(crate) fn dispatch(
    client: &mut Client,
    frame: FrameKind,
) -> Result<Option<FrameKind>, BridgeError> {
    let Some(id) = response_id(&frame) else {
        return Ok(Some(frame));
    };
    let Some(owner) = client.session_creates.owner(id) else {
        return Ok(Some(frame));
    };
    let mut entry = client
        .session_creates
        .entries
        .remove(&owner)
        .ok_or_else(|| BridgeError::state("session creation correlation disappeared"))?;
    let result = receive(client, &mut entry, id, frame);
    if !entry.released || !entry.state_finished {
        client.session_creates.entries.insert(owner, entry);
    }
    result.map(|()| None)
}

fn receive(
    client: &mut Client,
    entry: &mut Creation,
    id: u32,
    frame: FrameKind,
) -> Result<(), BridgeError> {
    match frame {
        FrameKind::MetadataValue { value, .. } => {
            if id == entry.result_read {
                entry.confirm(value);
            } else {
                entry.refuse("unexpected metadata reply to session creation");
            }
        }
        FrameKind::CommandResult { result, .. } => receive_state(client, entry, id, result)?,
        FrameKind::Error { message, .. } => {
            entry.refuse(&message);
            if id == entry.state_read {
                entry.state_finished = true;
            }
        }
        _ => unreachable!("response_id accepts only reply frames"),
    }
    Ok(())
}

const fn response_id(frame: &FrameKind) -> Option<u32> {
    match frame {
        FrameKind::MetadataValue { request_id, .. }
        | FrameKind::CommandResult { request_id, .. } => Some(*request_id),
        FrameKind::Error { request_id, .. } => *request_id,
        _ => None,
    }
}

fn receive_state(
    client: &mut Client,
    entry: &mut Creation,
    id: u32,
    result: CommandResult,
) -> Result<(), BridgeError> {
    if id != entry.state_read {
        entry.refuse("unexpected command reply to session creation");
        return Ok(());
    }
    entry.state_finished = true;
    let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
        entry.refuse("could not read the created session's identity");
        return Ok(());
    };
    if entry.status != PENDING {
        return Ok(());
    }
    if !entry.confirmed {
        entry.refuse("session identity arrived without a confirmed creation receipt");
        return Ok(());
    }
    let sessions = crate::workspace::session_summaries(snapshot).inspect_err(|_| {
        entry.refuse("could not decode the created session's state");
    })?;
    let created = sessions
        .iter()
        .find(|session| Some(session.session_id) == entry.receipt_id && session.keep_empty);
    let Some(created) = created else {
        entry.refuse("created session is no longer available as a keep-empty session");
        return Ok(());
    };
    entry.session_id = created.session_id;
    entry.status = CREATED;
    client.sessions = sessions;
    Ok(())
}

fn valid_name(name: &[u8]) -> Result<&str, BridgeError> {
    if name.is_empty() || name.len() > 240 {
        return Err(BridgeError::invalid(
            "session name must contain 1 to 240 bytes",
        ));
    }
    if name.iter().any(|byte| *byte < 0x20 || *byte == 0x7f) {
        return Err(BridgeError::invalid(
            "session name contains control characters",
        ));
    }
    std::str::from_utf8(name).map_err(|_| BridgeError::invalid("session name must be UTF-8"))
}

fn queue(client: &mut Client, id: u32, name: &str) -> Result<Creation, BridgeError> {
    let token = uuid::Uuid::new_v4().to_string();
    let result_read = client.workspace.reserve_internal()?;
    let state_read = client.workspace.reserve_internal()?;
    let value = serde_json::to_vec(&serde_json::json!({
        "name": name, "empty": true, "keep_empty": true, "request_token": token,
    }))
    .map_err(|error| BridgeError::invalid(error.to_string()))?;
    let frames = [
        FrameKind::SetMetadata {
            request_id: id,
            scope: Scope::Global,
            key: SESSION_CREATE_KEY.to_owned(),
            value,
        },
        FrameKind::GetMetadata {
            request_id: result_read,
            scope: Scope::Global,
            key: format!("{SESSION_CREATE_RESULT_KEY_PREFIX}{token}"),
        },
        FrameKind::Command {
            request_id: state_read,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        },
    ];
    let before = client.outgoing.len();
    for frame in &frames {
        if let Err(error) = client.queue_frame(frame) {
            client.outgoing.truncate(before);
            return Err(error);
        }
    }
    Ok(Creation {
        name: name.to_owned(),
        token,
        result_read,
        state_read,
        confirmed: false,
        receipt_id: None,
        state_finished: false,
        released: false,
        status: PENDING,
        session_id: 0,
        message: Vec::new(),
    })
}

/// Create an empty durable session on this exact negotiated connection.
/// `keep_empty` must be true; older servers are refused before any write.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread. Name is a
/// readable span. Request IDs obey the bridge's monotonic operation namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_create_session(
    client: *mut PhuxClient,
    request_id: u32,
    name: PhuxBytes,
    keep_empty: bool,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if !client.protocol_ready || client.detached {
            return Err(BridgeError::state(
                "session creation needs a negotiated connection",
            ));
        }
        if !keep_empty || !client.keep_empty_sessions {
            return Err(BridgeError::state(
                "server must support keep-empty sessions",
            ));
        }
        // SAFETY: caller supplies a readable name span.
        let name = valid_name(unsafe { bytes_in(name.data, name.len) }?)?;
        client.operations.check_request_id(request_id)?;
        ensure_capacity(client)?;
        let entry = queue(client, request_id, name)?;
        client.operations.consume_request_id(request_id);
        client.session_creates.entries.insert(request_id, entry);
        Ok(())
    })
}

fn ensure_capacity(client: &Client) -> Result<(), BridgeError> {
    if client.session_creates.entries.len() >= crate::operations::MAX_OPERATIONS
        || client.outgoing.len() + 3 > crate::operations::MAX_OPERATIONS
    {
        return Err(BridgeError::state(
            "session creation queue is full; drain and release completed requests",
        ));
    }
    Ok(())
}

/// Exact request state: 1 pending, 2 created, 3 refused, 4 unknown outcome.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxSessionCreateInfo {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub status: u32,
    pub session_id: u32,
    pub message: PhuxBytes,
}

/// Read one request; another concurrent request cannot replace its result.
///
/// # Safety
/// Client is live and unmodified; out is writable with initialized size/version.
/// Returned message borrows the client until the next mutable client call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_session_create_info(
    client: *const PhuxClient,
    request_id: u32,
    out: *mut PhuxSessionCreateInfo,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller provides a writable output when non-null.
        let out = unsafe { out.as_mut() }
            .ok_or_else(|| BridgeError::invalid("create info output is null"))?;
        check_struct(
            out.size,
            mem::size_of::<PhuxSessionCreateInfo>(),
            out.version,
        )?;
        let entry = client
            .session_creates
            .entries
            .get(&request_id)
            .ok_or_else(|| BridgeError::invalid("unknown session creation request"))?;
        out.request_id = request_id;
        out.status = entry.status;
        out.session_id = entry.session_id;
        out.message = bytes_out(&entry.message);
        Ok(())
    })
}

/// Release interest in a result. Pending writes continue; their reply correlation
/// is retained until drained or disconnected, without exposing another result.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_session_create_release(
    client: *mut PhuxClient,
    request_id: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let entry = client
            .session_creates
            .entries
            .get_mut(&request_id)
            .ok_or_else(|| BridgeError::invalid("unknown session creation request"))?;
        // A refused metadata read can precede the already-queued state reply.
        // Keep its correlation tombstone until that reply has been consumed.
        entry.released = true;
        if entry.state_finished {
            client.session_creates.entries.remove(&request_id);
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Limits;
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
    use phux_protocol::{ResourceId, SessionId, WindowId};

    fn client() -> Box<PhuxClient> {
        let mut inner = Client::new(Limits {
            bootstrap_chunk: 1024,
            history_page: 1024,
            history_page_rows: 128,
            history_cache_bytes: 4096,
            history_materialized_rows: 1024,
            history_prefetch_rows: 64,
        });
        inner.protocol_ready = true;
        inner.keep_empty_sessions = true;
        Box::new(PhuxClient {
            inner,
            _not_send_sync: std::marker::PhantomData,
        })
    }

    fn create(client: &mut PhuxClient, id: u32, name: &str) -> PhuxClientResult {
        // SAFETY: owned client and name live throughout the call.
        unsafe { phux_client_create_session(client, id, bytes_out(name.as_bytes()), true) }
    }

    fn feed(client: &mut PhuxClient, frame: &FrameKind) {
        let mut encoded = bytes::BytesMut::new();
        frame.encode(&mut encoded);
        // SAFETY: encoded and client are live for the entire call.
        assert_eq!(
            unsafe { crate::phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) },
            PhuxClientResult::Ok
        );
    }

    fn info(client: &PhuxClient, id: u32) -> PhuxSessionCreateInfo {
        let mut out = PhuxSessionCreateInfo {
            size: mem::size_of::<PhuxSessionCreateInfo>(),
            version: crate::types::ABI_VERSION,
            request_id: 0,
            status: 0,
            session_id: 0,
            message: bytes_out(&[]),
        };
        // SAFETY: client and initialized output remain live.
        assert_eq!(
            unsafe { phux_client_session_create_info(client, id, &raw mut out) },
            PhuxClientResult::Ok
        );
        out
    }

    fn receipt(client: &PhuxClient, id: u32) -> FrameKind {
        receipt_with_id(client, id, if id == 2 { 22 } else { 42 })
    }

    fn receipt_with_id(client: &PhuxClient, id: u32, session: u32) -> FrameKind {
        let entry = &client.inner.session_creates.entries[&id];
        FrameKind::MetadataValue { request_id: entry.result_read, value: Some(serde_json::to_vec(&serde_json::json!({
            "name": entry.name, "request_token": entry.token, "terminal_id": null, "empty": true,
            "session_id": session,
        })).unwrap()) }
    }

    fn state(client: &PhuxClient, id: u32, session: u32) -> FrameKind {
        let entry = &client.inner.session_creates.entries[&id];
        FrameKind::CommandResult {
            request_id: entry.state_read,
            result: CommandResult::OkWith(CommandValue::State(
                SessionSnapshot::new(
                    SessionId::new(session),
                    WindowId::new(0),
                    ResourceId::local(0),
                )
                .with_sessions(vec![
                    SessionInfo::new(SessionId::new(session), &entry.name).with_keep_empty(true),
                ]),
            )),
        }
    }

    #[test]
    fn sends_real_cli_create_with_keep_empty_and_confirms_server_identity_without_attach() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "work"), PhuxClientResult::Ok);
        assert_eq!(info(&client, 1).status, PENDING);
        assert_eq!(client.inner.outgoing.len(), 3);
        let (frame, _) = FrameKind::decode(&client.inner.outgoing[0]).unwrap();
        let FrameKind::SetMetadata { key, value, .. } = frame else {
            panic!("expected CLI metadata create")
        };
        assert_eq!(key, SESSION_CREATE_KEY);
        let value: serde_json::Value = serde_json::from_slice(&value).unwrap();
        assert_eq!(value["keep_empty"], true);
        assert_eq!(value["empty"], true);
        assert_eq!(value["name"], "work");
        let response = receipt(&client, 1);
        feed(&mut client, &response);
        assert_eq!(
            info(&client, 1).status,
            PENDING,
            "receipt alone cannot confirm current availability"
        );
        let response = state(&client, 1, 42);
        feed(&mut client, &response);
        assert_eq!(info(&client, 1).session_id, 42);
        assert_eq!(info(&client, 1).status, CREATED);
        assert!(client.inner.sessions[0].keep_empty);
        assert!(!client.inner.attached);
        assert!(!client.inner.attach_queued);
    }

    #[test]
    fn concurrent_requests_keep_distinct_nonces_and_out_of_order_results() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "one"), PhuxClientResult::Ok);
        assert_eq!(create(&mut client, 2, "two"), PhuxClientResult::Ok);
        assert_ne!(
            client.inner.session_creates.entries[&1].token,
            client.inner.session_creates.entries[&2].token
        );
        let response = receipt(&client, 2);
        feed(&mut client, &response);
        let response = state(&client, 2, 22);
        feed(&mut client, &response);
        assert_eq!(info(&client, 1).status, PENDING);
        assert_eq!(info(&client, 2).session_id, 22);
        let response = receipt_with_id(&client, 1, 11);
        feed(&mut client, &response);
        let response = state(&client, 1, 11);
        feed(&mut client, &response);
        assert_eq!(info(&client, 1).session_id, 11);
        assert_eq!(info(&client, 2).session_id, 22);
    }

    #[test]
    fn missing_or_wrong_nonce_refuses_and_release_drains_the_queued_state_reply() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "duplicate"), PhuxClientResult::Ok);
        let entry = &client.inner.session_creates.entries[&1];
        let response = FrameKind::MetadataValue {
            request_id: entry.result_read,
            value: None,
        };
        feed(&mut client, &response);
        assert_eq!(info(&client, 1).status, REFUSED);
        let response = state(&client, 1, 10);
        // SAFETY: owned client stays live and exclusively accessed.
        assert_eq!(
            unsafe { phux_client_session_create_release(&raw mut *client, 1) },
            PhuxClientResult::Ok
        );
        feed(&mut client, &response);
        assert!(client.inner.session_creates.entries.is_empty());
        assert!(!matches_result(
            br#"{"name":"x","request_token":"other","empty":true,"terminal_id":null}"#,
            "x",
            "expected"
        ));
    }

    #[test]
    fn old_servers_invalid_names_and_false_keep_empty_refuse_before_any_write() {
        let mut client = client();
        client.inner.keep_empty_sessions = false;
        assert_eq!(
            create(&mut client, 1, "work"),
            PhuxClientResult::InvalidState
        );
        client.inner.keep_empty_sessions = true;
        assert_eq!(
            create(&mut client, 1, "bad\nname"),
            PhuxClientResult::InvalidArgument
        );
        // SAFETY: spans and client live throughout the call.
        assert_eq!(
            unsafe { phux_client_create_session(&raw mut *client, 1, bytes_out(b"work"), false) },
            PhuxClientResult::InvalidState
        );
        assert!(client.inner.outgoing.is_empty());
        assert_eq!(
            create(&mut client, 1, "good"),
            PhuxClientResult::Ok,
            "refusals do not consume request ID"
        );
    }

    #[test]
    fn disconnect_records_unknown_outcome_instead_of_a_successful_duplicate() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "work"), PhuxClientResult::Ok);
        // SAFETY: client is live and exclusively accessed.
        assert_eq!(
            unsafe { crate::phux_client_disconnect(&raw mut *client) },
            PhuxClientResult::Ok
        );
        assert_eq!(info(&client, 1).status, UNKNOWN);
        assert_eq!(info(&client, 1).session_id, 0);
    }

    #[test]
    fn same_name_replacement_cannot_authorize_a_different_session() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "work"), PhuxClientResult::Ok);
        let response = receipt_with_id(&client, 1, 42);
        feed(&mut client, &response);
        // Another client renames created #42 and creates #99 with its old name.
        let mut response = state(&client, 1, 99);
        if let FrameKind::CommandResult {
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
            ..
        } = &mut response
        {
            snapshot
                .sessions
                .push(SessionInfo::new(SessionId::new(42), "renamed").with_keep_empty(true));
        }
        feed(&mut client, &response);
        assert_eq!(info(&client, 1).status, CREATED);
        assert_eq!(info(&client, 1).session_id, 42);
    }

    #[test]
    fn confirmed_write_with_missing_identity_is_unknown_not_refused() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "work"), PhuxClientResult::Ok);
        let response = receipt_with_id(&client, 1, 42);
        feed(&mut client, &response);
        let response = state(&client, 1, 99);
        feed(&mut client, &response);
        assert_eq!(info(&client, 1).status, UNKNOWN);
    }

    #[test]
    fn disconnect_retires_released_refusal_tombstones() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "work"), PhuxClientResult::Ok);
        let response = FrameKind::MetadataValue {
            request_id: client.inner.session_creates.entries[&1].result_read,
            value: None,
        };
        feed(&mut client, &response);
        // SAFETY: owned client remains live and exclusively accessed.
        unsafe { phux_client_session_create_release(&raw mut *client, 1) };
        client.inner.session_creates.disconnect();
        assert!(client.inner.session_creates.entries.is_empty());
    }

    #[test]
    fn legacy_receipt_without_identity_is_unknown_and_release_drains_state() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "work"), PhuxClientResult::Ok);
        let mut response = receipt(&client, 1);
        if let FrameKind::MetadataValue {
            value: Some(value), ..
        } = &mut response
        {
            let mut doc: serde_json::Value = serde_json::from_slice(value).unwrap();
            doc.as_object_mut().unwrap().remove("session_id");
            *value = serde_json::to_vec(&doc).unwrap();
        }
        feed(&mut client, &response);
        assert_eq!(info(&client, 1).status, UNKNOWN);
        let response = state(&client, 1, 42);
        // SAFETY: owned client remains live and exclusively accessed.
        assert_eq!(
            unsafe { phux_client_session_create_release(&raw mut *client, 1) },
            PhuxClientResult::Ok
        );
        assert_eq!(client.inner.session_creates.entries.len(), 1);
        feed(&mut client, &response);
        assert!(client.inner.session_creates.entries.is_empty());
    }

    #[test]
    fn abandoned_pending_creation_retires_after_its_own_replies() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "work"), PhuxClientResult::Ok);
        let receipt = receipt(&client, 1);
        let state = state(&client, 1, 42);
        // SAFETY: owned client remains live and exclusively accessed.
        assert_eq!(
            unsafe { phux_client_session_create_release(&raw mut *client, 1) },
            PhuxClientResult::Ok
        );
        assert_eq!(client.inner.session_creates.entries.len(), 1);
        feed(&mut client, &receipt);
        feed(&mut client, &state);
        assert!(client.inner.session_creates.entries.is_empty());
    }

    #[test]
    fn state_error_after_confirmed_write_never_claims_nothing_was_created() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "work"), PhuxClientResult::Ok);
        let response = receipt(&client, 1);
        feed(&mut client, &response);
        let response = FrameKind::CommandResult {
            request_id: client.inner.session_creates.entries[&1].state_read,
            result: CommandResult::Ok,
        };
        feed(&mut client, &response);
        assert_eq!(info(&client, 1).status, UNKNOWN);
        assert!(
            String::from_utf8_lossy(&client.inner.session_creates.entries[&1].message)
                .contains("Refresh Sessions")
        );
    }
}
