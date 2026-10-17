//! Session creation on the owning connection. Against a server that
//! advertises `ORGANIZATION` it is one typed `SESSION_CREATE` whose committed
//! key an ordered state read binds to its live id; otherwise the CLI's
//! keep-empty metadata operation, whose nonce result binds the server-owned
//! ID before the same ordered read. No attach or second coordinator is
//! involved.
#![allow(clippy::redundant_pub_crate, reason = "private bridge module")]

use std::collections::BTreeMap;
use std::mem;

use crate::c::client::Client;
use crate::c::error::{BridgeError, bytes_in, check_struct};
use crate::c::types::{PhuxBytes, bytes_out};
use crate::c::{PhuxClient, PhuxClientResult, with_client_mut, with_client_ref};
use phux_client_core::org_ops::OrgOutcome;
use phux_client_core::rename::check_session_name;
use phux_protocol::ids::{OperationId, SessionKey};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, LaunchSpec, SESSION_CREATE_KEY,
    SESSION_CREATE_RESULT_KEY_PREFIX, Scope, SessionLifetime, StateScope,
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
    /// The typed `SESSION_CREATE` this creation submitted, against a server
    /// that advertises `ORGANIZATION`.
    operation: Option<OperationId>,
    /// The committed session's key, which the confirming read matches.
    key: Option<SessionKey>,
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

/// Consume only the exact operation's replies, before arrangement dispatch.
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
    let sessions = crate::c::arrangement::session_summaries(snapshot).inspect_err(|_| {
        entry.refuse("could not decode the created session's state");
    })?;
    let created = sessions.iter().find(|session| match entry.key {
        Some(key) => session.key == Some(key),
        None => Some(session.session_id) == entry.receipt_id && session.keep_empty,
    });
    let Some(created) = created else {
        entry.refuse("the created session is no longer available");
        return Ok(());
    };
    entry.session_id = created.session_id;
    entry.status = CREATED;
    client.sessions = sessions;
    client.org.sessions_dirty = true;
    Ok(())
}

/// A name to create: UTF-8 that `check_session_name` accepts.
fn valid_name(name: &[u8]) -> Result<&str, BridgeError> {
    let name = std::str::from_utf8(name)
        .map_err(|_| BridgeError::invalid("session name must be UTF-8"))?;
    check_session_name(name).map_err(|invalid| BridgeError::invalid(invalid.to_string()))?;
    Ok(name)
}

fn create_value(name: &str, directory: Option<&str>, token: &str) -> Result<Vec<u8>, BridgeError> {
    let mut value = serde_json::json!({
        "name": name,
        "keep_empty": true,
        "request_token": token,
    });
    if let Some(directory) = directory.filter(|directory| !directory.is_empty()) {
        value["cwd"] = serde_json::Value::String(directory.to_owned());
    } else {
        value["empty"] = serde_json::Value::Bool(true);
    }
    serde_json::to_vec(&value).map_err(|error| BridgeError::invalid(error.to_string()))
}

fn queue(
    client: &mut Client,
    id: u32,
    name: &str,
    directory: Option<&str>,
) -> Result<Creation, BridgeError> {
    let token = uuid::Uuid::new_v4().to_string();
    let result_read = client.arrangement.reserve_internal()?;
    let state_read = client.arrangement.reserve_internal()?;
    let value = create_value(name, directory, &token)?;
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
        operation: None,
        key: None,
    })
}

/// Against a server that advertises `ORGANIZATION`: one typed
/// `SESSION_CREATE` through the runtime's operation journal. A non-empty
/// directory, or an ephemeral lifetime, launches the first shell with it
/// (an ephemeral session is never empty).
fn submit_typed(
    client: &Client,
    name: &str,
    directory: Option<&str>,
    lifetime: SessionLifetime,
) -> Result<Creation, BridgeError> {
    let directory = directory.filter(|directory| !directory.is_empty());
    let launch = (directory.is_some() || lifetime == SessionLifetime::Ephemeral)
        .then(|| Box::new(LaunchSpec::new(Vec::new()).with_cwd(directory.map(str::to_owned))));
    let attrs = directory
        .map(|directory| vec![("directory".to_owned(), directory.to_owned())])
        .unwrap_or_default();
    let operation = client
        .control()
        .submit_organization(Command::SessionCreate {
            operation: phux_client_runtime::control::mint_operation(),
            name: name.to_owned(),
            lifetime,
            attrs,
            launch,
        })
        .map_err(|error| BridgeError::state(error.to_string()))?;
    Ok(Creation {
        name: name.to_owned(),
        token: String::new(),
        result_read: 0,
        state_read: 0,
        confirmed: false,
        receipt_id: None,
        state_finished: true,
        released: false,
        status: PENDING,
        session_id: 0,
        message: Vec::new(),
        operation: Some(operation),
        key: None,
    })
}

/// A typed creation resolved. On commit, an ordered `GET_STATE` binds the
/// created key to its live session id. True when `operation` was one of
/// this client's creations.
pub(crate) fn organization_resolved(
    client: &mut Client,
    operation: OperationId,
    outcome: &OrgOutcome,
) -> bool {
    let Some(request) = client
        .session_creates
        .entries
        .iter()
        .find_map(|(request, entry)| (entry.operation == Some(operation)).then_some(*request))
    else {
        return false;
    };
    let resolution = crate::projection::organization::resolution(outcome);
    let committed = match outcome {
        OrgOutcome::Committed(committed) => committed.session,
        _ => None,
    };
    let read = match committed {
        Some(_) => client.arrangement.reserve_internal().ok(),
        None => None,
    };
    let queued = read.is_some_and(|read| {
        client
            .queue_frame(&FrameKind::Command {
                request_id: read,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            })
            .is_ok()
    });
    let Some(entry) = client.session_creates.entries.get_mut(&request) else {
        return true;
    };
    entry.operation = None;
    match (committed, read) {
        (Some(key), Some(read)) if queued => {
            entry.confirmed = true;
            entry.key = Some(key);
            entry.state_read = read;
            entry.state_finished = false;
        }
        (Some(_), _) => entry.unknown(
            "The session was created but its identity could not be read. Refresh Sessions to find it.",
        ),
        (None, _) if resolution.status == crate::projection::organization::Status::Unknown => {
            entry.unknown(&resolution.message);
        }
        (None, _) => {
            let message = if resolution.message.is_empty() {
                "the server did not create the session".to_owned()
            } else {
                resolution.message
            };
            entry.refuse(&message);
        }
    }
    if entry.released && entry.state_finished {
        client.session_creates.entries.remove(&request);
    }
    true
}

/// Create a session on this exact negotiated connection.
///
/// `lifetime` is a `PhuxSessionLifetime`. Against a server that advertises `ORGANIZATION`
/// it is one typed `SESSION_CREATE`; an ephemeral session launches its
/// first shell, a persistent one is created empty. Otherwise the legacy
/// keep-empty metadata operation creates a persistent, empty session, and an
/// ephemeral one is refused. Against an `ORGANIZATION` server the typed
/// command is held until the records' seed lands; only when no seed can land
/// (refused, with no authority known) is the call refused (`INVALID_STATE`,
/// request ID not consumed) until `phux_client_organization_refresh`.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread. Name is a
/// readable span. Request IDs obey the bridge's monotonic operation namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_create_session(
    client: *mut PhuxClient,
    request_id: u32,
    name: PhuxBytes,
    lifetime: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies a readable name span.
        let name = unsafe { bytes_in(name.data, name.len) }?;
        create(client, request_id, name, None, lifetime)
    })
}

/// As [`phux_client_create_session`], with `directory` as the session's
/// directory: a non-empty one launches the first shell there. An empty
/// directory is the same session as [`phux_client_create_session`].
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread. Name and
/// directory are readable spans. Request IDs obey the bridge's monotonic
/// operation namespace.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_create_session_in(
    client: *mut PhuxClient,
    request_id: u32,
    name: PhuxBytes,
    directory: PhuxBytes,
    lifetime: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies readable name and directory spans.
        let name = unsafe { bytes_in(name.data, name.len) }?;
        // SAFETY: as above.
        let directory = unsafe { bytes_in(directory.data, directory.len) }?;
        let directory = std::str::from_utf8(directory)
            .map_err(|_| BridgeError::invalid("session directory must be UTF-8"))?;
        create(client, request_id, name, Some(directory), lifetime)
    })
}

fn create(
    client: &mut Client,
    request_id: u32,
    name: &[u8],
    directory: Option<&str>,
    lifetime: u32,
) -> Result<(), BridgeError> {
    if !client.protocol_ready || client.detached {
        return Err(BridgeError::state(
            "session creation needs a negotiated connection",
        ));
    }
    let lifetime = crate::c::organization::lifetime_in(lifetime)?;
    let name = valid_name(name)?;
    // An ORGANIZATION server is only ever sent the typed command, which the
    // runtime's journal holds until the seed names the authority; the legacy
    // key is never the fallback.
    let typed = client.control().organization_supported();
    crate::c::organization::ensure_deliverable(client)?;
    if !typed && (lifetime != SessionLifetime::Persistent || !client.keep_empty_sessions) {
        return Err(BridgeError::state(
            "without ORGANIZATION the server must support keep-empty sessions, and only persistent sessions can be created",
        ));
    }
    client.operations.check_request_id(request_id)?;
    ensure_capacity(client)?;
    let entry = if typed {
        submit_typed(client, name, directory, lifetime)?
    } else {
        queue(client, request_id, name, directory)?
    };
    client.operations.consume_request_id(request_id);
    client.session_creates.entries.insert(request_id, entry);
    client.process_runtime_events()?;
    Ok(())
}

fn ensure_capacity(client: &Client) -> Result<(), BridgeError> {
    if client.session_creates.entries.len() >= crate::c::operations::MAX_OPERATIONS
        || client.outgoing.len() + 3 > crate::c::operations::MAX_OPERATIONS
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
    use crate::c::test_support::negotiated_client;
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
    use phux_protocol::{ResourceId, SessionId, WindowId};

    fn client() -> Box<PhuxClient> {
        let client = negotiated_client(&[phux_protocol::ServerFeature::KeepEmptySessions]);
        // SAFETY: the fixture returns a uniquely owned heap client.
        unsafe { Box::from_raw(client) }
    }

    fn create_in(
        client: &mut PhuxClient,
        id: u32,
        name: &str,
        directory: &str,
    ) -> PhuxClientResult {
        // SAFETY: owned client and spans live throughout the call.
        unsafe {
            phux_client_create_session_in(
                client,
                id,
                bytes_out(name.as_bytes()),
                bytes_out(directory.as_bytes()),
                crate::c::organization::LIFETIME_PERSISTENT,
            )
        }
    }

    fn create(client: &mut PhuxClient, id: u32, name: &str) -> PhuxClientResult {
        // SAFETY: owned client and name live throughout the call.
        unsafe {
            phux_client_create_session(
                client,
                id,
                bytes_out(name.as_bytes()),
                crate::c::organization::LIFETIME_PERSISTENT,
            )
        }
    }

    fn feed(client: &mut PhuxClient, frame: &FrameKind) {
        assert_eq!(
            crate::c::test_support::feed(client, frame),
            PhuxClientResult::Ok
        );
    }

    fn info(client: &PhuxClient, id: u32) -> PhuxSessionCreateInfo {
        let mut out = PhuxSessionCreateInfo {
            size: mem::size_of::<PhuxSessionCreateInfo>(),
            version: crate::c::types::ABI_VERSION,
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
    fn create_in_records_the_directory_and_does_not_mark_the_session_empty() {
        let mut client = client();
        assert_eq!(
            create_in(&mut client, 7, "api", "/src/api"),
            PhuxClientResult::Ok
        );
        let (frame, _) = FrameKind::decode(&client.inner.outgoing[0]).unwrap();
        let FrameKind::SetMetadata { value, .. } = frame else {
            panic!("expected CLI metadata create")
        };
        let value: serde_json::Value = serde_json::from_slice(&value).unwrap();
        assert_eq!(value["cwd"], "/src/api");
        assert_eq!(value["keep_empty"], true);
        assert_eq!(value["name"], "api");
        assert!(value.get("empty").is_none());
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
        assert!(value.get("cwd").is_none());
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
        // SAFETY: the fixture returns a uniquely owned heap client.
        let mut old = unsafe { Box::from_raw(negotiated_client(&[])) };
        assert_eq!(create(&mut old, 1, "work"), PhuxClientResult::InvalidState);
        assert!(old.inner.outgoing.is_empty());
        let mut client = client();
        assert_eq!(
            create(&mut client, 1, "bad\nname"),
            PhuxClientResult::InvalidArgument
        );
        // SAFETY: spans and client live throughout the call.
        assert_eq!(
            unsafe {
                phux_client_create_session(
                    &raw mut *client,
                    1,
                    bytes_out(b"work"),
                    crate::c::organization::LIFETIME_EPHEMERAL,
                )
            },
            PhuxClientResult::InvalidState
        );
        assert!(client.inner.outgoing.is_empty());
        assert_eq!(
            create(&mut client, 1, "good"),
            PhuxClientResult::Ok,
            "refusals do not consume request ID"
        );
    }

    /// Create judges the name by core's one rule, before any write.
    #[test]
    fn a_name_is_judged_by_cores_session_name_rule() {
        use phux_client_core::rename::MAX_SESSION_NAME_BYTES;
        let mut client = client();
        let too_long = "a".repeat(MAX_SESSION_NAME_BYTES + 1);
        for bad in ["x:y", too_long.as_str(), "a\tb", "a\u{7f}b", "@3", ""] {
            assert_eq!(
                create(&mut client, 1, bad),
                PhuxClientResult::InvalidArgument,
                "{bad:?}"
            );
        }
        assert!(client.inner.outgoing.is_empty());
        let at_limit = "a".repeat(MAX_SESSION_NAME_BYTES);
        assert_eq!(create(&mut client, 1, &at_limit), PhuxClientResult::Ok);
        assert_eq!(create(&mut client, 2, "work"), PhuxClientResult::Ok);
    }

    #[test]
    fn disconnect_records_unknown_outcome_instead_of_a_successful_duplicate() {
        let mut client = client();
        assert_eq!(create(&mut client, 1, "work"), PhuxClientResult::Ok);
        // SAFETY: client is live and exclusively accessed.
        assert_eq!(
            unsafe { crate::c::phux_client_disconnect(&raw mut *client) },
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
