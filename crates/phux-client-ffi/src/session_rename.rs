//! Rename a session on the server this client is connected to
//! (`phux.session.name/v1`, `docs/spec/L3.md` section 3.1).
//!
//! A rename is a `SET_METADATA` of the conventional key with the value
//! `current\0new`. The server applies it to its registry and broadcasts the
//! applied value as `METADATA_CHANGED` to subscribers of the key; a refused
//! or no-op rename broadcasts nothing, and `SET_METADATA` has no reply. So
//! the bridge does what `phux rename` does: it judges the request against the
//! latest session list first (an unknown session or a name another session
//! holds is refused here, with a reason), then sends the write, then a
//! `GET_STATE` as an ordering barrier. Frames are ordered on one connection,
//! so once that read is answered the write has been applied or refused: the
//! answer names the outcome even if no `METADATA_CHANGED` arrives.
//!
//! The first rename on a client subscribes to the key, so this client hears
//! its own rename and every later one on that server, and applies each to
//! the session list in place. A client that asks to follow renames
//! (`phux_client_follow_session_names`) subscribes as soon as `HELLO_OK` is
//! applied instead, so renames other clients make reach its list before it
//! has renamed anything. Neither a listing nor an attached client needs to
//! attach anything for this: the subscription is read-only and a rename
//! writes metadata; neither sizes anything. The server delivers
//! `METADATA_CHANGED` to a subscriber that never attached.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared by the bridge dispatcher and Client"
)]

use std::mem;

use crate::client::Client;
use crate::error::{BridgeError, bytes_in, check_struct};
use crate::types::{PhuxBytes, bytes_out};
use crate::{PhuxClient, PhuxClientResult, with_client_mut, with_client_ref};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, SESSION_NAME_KEY, Scope, StateScope,
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

/// A session name, as the workspace catalog bounds one.
const MAX_NAME_BYTES: usize = 4096;

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
/// ahead of the workspace and operations dispatchers, which would read the
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
        } if key == SESSION_NAME_KEY => {
            if let Some(value) = value {
                applied(client, &value);
            }
            Ok(None)
        }
        FrameKind::Error {
            request_id: Some(request_id),
            message,
            ..
        } if awaits(client, request_id) => {
            if client.session_rename.barrier == Some(request_id) {
                client.session_rename.barrier = None;
            }
            if client.session_rename.pending() {
                client.session_rename.settle(STATUS_REFUSED, &message);
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
    if crate::workspace::rename_sessions(client, |s| s.name == current_owned, new_name) {
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
        CommandResult::Error { message, .. } => {
            if client.session_rename.pending() {
                client.session_rename.settle(STATUS_REFUSED, &message);
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
    let Some(session) = snapshot.sessions.iter().find(|s| s.id.get() == id) else {
        if client.session_rename.pending() {
            client
                .session_rename
                .settle(STATUS_REFUSED, "the session no longer exists");
        }
        return Ok(());
    };
    let name = session.name.as_bytes().to_vec();
    // The server's name for it wins, whether or not a METADATA_CHANGED said so.
    if valid_name(&name).is_ok()
        && crate::workspace::rename_sessions(client, |s| s.session_id == id, &name)
    {
        client.session_rename.sessions_revision += 1;
    }
    let rename = &mut client.session_rename;
    if !rename.pending() {
        return Ok(());
    }
    if name == rename.new_name {
        rename.settle(STATUS_RENAMED, "");
    } else {
        rename.settle(
            STATUS_REFUSED,
            "the server did not rename the session (the name may have been taken meanwhile)",
        );
    }
    Ok(())
}

fn valid_name(name: &[u8]) -> Result<(), BridgeError> {
    if name.is_empty() {
        return Err(BridgeError::invalid("a session name must not be empty"));
    }
    if name.len() > MAX_NAME_BYTES || name.contains(&0) {
        return Err(BridgeError::invalid(
            "a session name exceeds 4096 bytes or contains NUL",
        ));
    }
    std::str::from_utf8(name)
        .map(|_| ())
        .map_err(|_| BridgeError::invalid("a session name must be UTF-8"))
}

/// Why this rename must not be sent, judged against the latest list: an
/// unknown session, or a name another session already holds. The session's
/// id when the rename may go ahead, else the reason.
fn refusal(client: &Client, current: &[u8], new_name: &[u8]) -> Result<u32, String> {
    let Some(session) = client.sessions.iter().find(|s| s.name == current) else {
        return Err(format!(
            "no session named {:?}",
            String::from_utf8_lossy(current)
        ));
    };
    let taken = client
        .sessions
        .iter()
        .any(|other| other.name == new_name && other.session_id != session.session_id);
    if taken {
        return Err(format!(
            "{:?} already exists",
            String::from_utf8_lossy(new_name)
        ));
    }
    Ok(session.session_id)
}

/// Subscribe to the rename key once per client. Read-only: it attaches
/// nothing and sizes nothing.
fn subscribe(client: &mut Client) -> Result<(), BridgeError> {
    if client.session_rename.subscribed {
        return Ok(());
    }
    client.queue_frame(&FrameKind::SubscribeMetadata {
        scope: Scope::Global,
        key: SESSION_NAME_KEY.to_owned(),
    })?;
    client.session_rename.subscribed = true;
    Ok(())
}

/// `HELLO_OK` was applied: a client following renames subscribes now, as the
/// first frame it queues on the negotiated connection.
pub(crate) fn negotiated(client: &mut Client) -> Result<(), BridgeError> {
    if client.session_rename.follow {
        subscribe(client)?;
    }
    Ok(())
}

fn queue_rename(
    client: &mut Client,
    request_id: u32,
    current: &[u8],
    new_name: &[u8],
) -> Result<(), BridgeError> {
    subscribe(client)?;
    let mut value = current.to_vec();
    value.push(0);
    value.extend_from_slice(new_name);
    client.queue_frame(&FrameKind::SetMetadata {
        request_id,
        scope: Scope::Global,
        key: SESSION_NAME_KEY.to_owned(),
        value,
    })?;
    let barrier = client.workspace.reserve_internal()?;
    client.queue_frame(&FrameKind::Command {
        request_id: barrier,
        command: Command::GetState {
            scope: StateScope::Server,
        },
    })?;
    client.session_rename.barrier = Some(barrier);
    Ok(())
}

/// Rename the session named `current` to `new_name` on the connected server.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread; each span is
/// readable for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_rename_session(
    client: *mut PhuxClient,
    request_id: u32,
    current: PhuxBytes,
    new_name: PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if !client.protocol_ready || client.detached {
            return Err(BridgeError::state("a rename needs a negotiated connection"));
        }
        if client.session_rename.pending() {
            return Err(BridgeError::state("a rename is already pending"));
        }
        // SAFETY: caller supplies readable spans.
        let current = unsafe { bytes_in(current.data, current.len) }?;
        // SAFETY: as above.
        let new_name = unsafe { bytes_in(new_name.data, new_name.len) }?;
        valid_name(current)?;
        valid_name(new_name)?;
        client.operations.check_request_id(request_id)?;
        if client.outgoing.len() + 3 > crate::operations::MAX_OPERATIONS {
            return Err(BridgeError::state(
                "outgoing queue is full; drain outgoing frames before renaming",
            ));
        }
        client.operations.consume_request_id(request_id);
        let judged = refusal(client, current, new_name);
        let rename = &mut client.session_rename;
        rename.request_id = request_id;
        rename.current = current.to_vec();
        rename.new_name = new_name.to_vec();
        match judged {
            Err(reason) => {
                rename.session_id = 0;
                rename.settle(STATUS_REFUSED, &reason);
            }
            Ok(session_id) if current == new_name => {
                rename.session_id = session_id;
                rename.settle(STATUS_RENAMED, "");
            }
            Ok(session_id) => {
                rename.session_id = session_id;
                rename.settle(STATUS_PENDING, "");
                if let Err(error) = queue_rename(client, request_id, current, new_name) {
                    client.session_rename.settle(STATUS_REFUSED, &error.message);
                    return Err(error);
                }
            }
        }
        Ok(())
    })
}

/// Follow renames any client makes, from the start of the connection.
///
/// Subscribes to `phux.session.name/v1` now if `HELLO_OK` has been applied,
/// else as soon as it is. Once per client; idempotent.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_follow_session_names(
    client: *mut PhuxClient,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if client.detached {
            return Err(BridgeError::state("the connection has ended"));
        }
        client.session_rename.follow = true;
        if client.protocol_ready {
            subscribe(client)?;
        }
        Ok(())
    })
}

/// The latest rename's state, as `phux_client_session_rename_info` reports it.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxSessionRenameInfo {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub status: u32,
    pub session_id: u32,
    pub sessions_revision: u64,
    pub message: PhuxBytes,
}

/// Read the latest rename's request ID, status and reason.
///
/// # Safety
/// Client is live and unmodified for the call; `out` is writable and its
/// `size`/`version` are initialized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_session_rename_info(
    client: *const PhuxClient,
    out: *mut PhuxSessionRenameInfo,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(
            out.size,
            mem::size_of::<PhuxSessionRenameInfo>(),
            out.version,
        )?;
        let rename = &client.session_rename;
        *out = PhuxSessionRenameInfo {
            size: out.size,
            version: out.version,
            request_id: rename.request_id,
            status: rename.status,
            session_id: rename.session_id,
            sessions_revision: rename.sessions_revision,
            message: bytes_out(&rename.message),
        };
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Limits, SessionSummary};
    use crate::types::ABI_VERSION;
    use phux_protocol::wire::frame::ErrorCode;
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
    use phux_protocol::{ResourceId, SessionId, WindowId};

    fn negotiated(names: &[&str]) -> *mut PhuxClient {
        let mut inner = Client::new(Limits {
            bootstrap_chunk: 1024,
            history_page: 1024,
            history_page_rows: 128,
            history_cache_bytes: 4096,
            history_materialized_rows: 1024,
            history_prefetch_rows: 64,
        });
        inner.protocol_ready = true;
        inner.sessions = names
            .iter()
            .zip(1..)
            .map(|(name, id)| SessionSummary {
                session_id: id,
                name: name.as_bytes().to_vec(),
                created_at_unix_secs: 0,
                window_count: 1,
                attached_client_count: 0,
                focused: id == 1,
                keep_empty: false,
            })
            .collect();
        Box::into_raw(Box::new(PhuxClient {
            inner,
            _not_send_sync: std::marker::PhantomData,
        }))
    }

    fn span(text: &str) -> PhuxBytes {
        PhuxBytes {
            data: text.as_ptr(),
            len: text.len(),
        }
    }

    fn rename(client: *mut PhuxClient, id: u32, from: &str, to: &str) -> PhuxClientResult {
        unsafe { phux_client_rename_session(client, id, span(from), span(to)) }
    }

    fn info(client: *mut PhuxClient) -> (u32, u32, u64, String) {
        let mut out = PhuxSessionRenameInfo {
            size: mem::size_of::<PhuxSessionRenameInfo>(),
            version: ABI_VERSION,
            request_id: 0,
            status: u32::MAX,
            session_id: 0,
            sessions_revision: 0,
            message: PhuxBytes::default(),
        };
        assert_eq!(
            unsafe { phux_client_session_rename_info(client, &raw mut out) },
            PhuxClientResult::Ok
        );
        let message = unsafe { bytes_in(out.message.data, out.message.len) }.expect("span");
        (
            out.request_id,
            out.status,
            out.sessions_revision,
            String::from_utf8(message.to_vec()).expect("UTF-8"),
        )
    }

    fn feed(client: *mut PhuxClient, frame: &FrameKind) -> PhuxClientResult {
        let mut encoded = bytes::BytesMut::new();
        frame.encode(&mut encoded);
        unsafe { crate::phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) }
    }

    fn sent(client: *mut PhuxClient) -> Vec<FrameKind> {
        let queued = std::mem::take(unsafe { &mut (*client).inner.outgoing });
        queued
            .iter()
            .map(|bytes| FrameKind::decode(bytes).expect("queued frame decodes").0)
            .collect()
    }

    fn names(client: *mut PhuxClient) -> Vec<String> {
        unsafe { &(*client).inner.sessions }
            .iter()
            .map(|s| String::from_utf8(s.name.clone()).expect("UTF-8"))
            .collect()
    }

    fn changed(value: &[u8]) -> FrameKind {
        FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: SESSION_NAME_KEY.to_owned(),
            value: Some(value.to_vec()),
        }
    }

    fn state(request_id: u32, names: &[&str]) -> FrameKind {
        let sessions = names
            .iter()
            .zip(1..)
            .map(|(name, id)| SessionInfo::new(SessionId::new(id), *name))
            .collect();
        FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(
                SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
                    .with_sessions(sessions),
            )),
        }
    }

    fn barrier_of(frames: &[FrameKind]) -> u32 {
        frames
            .iter()
            .find_map(|frame| match frame {
                FrameKind::Command {
                    request_id,
                    command: Command::GetState { .. },
                } => Some(*request_id),
                _ => None,
            })
            .expect("a confirmation GET_STATE")
    }

    #[test]
    fn a_rename_subscribes_writes_and_confirms_and_the_list_follows_the_broadcast() {
        let client = negotiated(&["build", "deploy"]);
        assert_eq!(rename(client, 5, "build", "ship"), PhuxClientResult::Ok);
        assert_eq!(info(client).1, STATUS_PENDING);
        let frames = sent(client);
        assert_eq!(frames.len(), 3, "subscribe, write, confirmation read");
        assert_eq!(
            frames[0],
            FrameKind::SubscribeMetadata {
                scope: Scope::Global,
                key: SESSION_NAME_KEY.to_owned()
            }
        );
        assert_eq!(
            frames[1],
            FrameKind::SetMetadata {
                request_id: 5,
                scope: Scope::Global,
                key: SESSION_NAME_KEY.to_owned(),
                value: b"build\0ship".to_vec(),
            }
        );
        let barrier = barrier_of(&frames);
        assert!(barrier >= crate::workspace::INTERNAL_START);
        assert!(!frames.iter().any(|f| matches!(f, FrameKind::Attach { .. })));

        // The broadcast renames the list in place and settles the rename.
        assert_eq!(feed(client, &changed(b"build\0ship")), PhuxClientResult::Ok);
        assert_eq!(names(client), ["ship", "deploy"]);
        let (request, status, revision, _) = info(client);
        assert_eq!((request, status, revision), (5, STATUS_RENAMED, 1));
        // The confirmation read is consumed here, not read as a stray reply.
        assert_eq!(
            feed(client, &state(barrier, &["ship", "deploy"])),
            PhuxClientResult::Ok
        );
        assert_eq!(info(client).1, STATUS_RENAMED);
        assert_eq!(info(client).2, 1, "nothing new to apply");

        // A second rename does not subscribe twice.
        assert_eq!(rename(client, 6, "ship", "sail"), PhuxClientResult::Ok);
        let again = sent(client);
        assert_eq!(again.len(), 2);
        assert!(matches!(
            again[0],
            FrameKind::SetMetadata { request_id: 6, .. }
        ));
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn a_duplicate_or_unknown_name_is_refused_with_a_reason_and_nothing_is_sent() {
        let client = negotiated(&["build", "deploy"]);
        assert_eq!(rename(client, 1, "build", "deploy"), PhuxClientResult::Ok);
        let (_, status, _, message) = info(client);
        assert_eq!(status, STATUS_REFUSED);
        assert_eq!(message, "\"deploy\" already exists");
        assert!(sent(client).is_empty());

        assert_eq!(rename(client, 2, "gone", "x"), PhuxClientResult::Ok);
        let (_, status, _, message) = info(client);
        assert_eq!(status, STATUS_REFUSED);
        assert_eq!(message, "no session named \"gone\"");
        assert!(sent(client).is_empty());

        // The same name is nothing to do.
        assert_eq!(rename(client, 3, "build", "build"), PhuxClientResult::Ok);
        assert_eq!(info(client).1, STATUS_RENAMED);
        assert!(sent(client).is_empty());
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn a_silent_server_refusal_is_read_from_the_confirmation_and_an_error_is_its_reason() {
        let client = negotiated(&["build", "deploy"]);
        assert_eq!(rename(client, 1, "build", "ship"), PhuxClientResult::Ok);
        let barrier = barrier_of(&sent(client));
        // Someone took "ship" first: no broadcast, the name is unchanged.
        assert_eq!(
            feed(client, &state(barrier, &["build", "deploy"])),
            PhuxClientResult::Ok
        );
        let (_, status, _, message) = info(client);
        assert_eq!(status, STATUS_REFUSED);
        assert!(message.contains("did not rename"), "{message}");
        assert_eq!(names(client), ["build", "deploy"]);

        // A correlated ERROR on the write names the server's reason.
        assert_eq!(rename(client, 2, "build", "ship"), PhuxClientResult::Ok);
        let barrier = barrier_of(&sent(client));
        let refused = FrameKind::Error {
            request_id: Some(2),
            code: ErrorCode::PermissionDenied,
            message: "not allowed".to_owned(),
        };
        assert_eq!(feed(client, &refused), PhuxClientResult::Ok);
        assert_eq!(info(client).1, STATUS_REFUSED);
        assert_eq!(info(client).3, "not allowed");
        assert_eq!(
            feed(client, &state(barrier, &["build", "deploy"])),
            PhuxClientResult::Ok,
            "the late confirmation is still consumed"
        );
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn another_clients_rename_moves_the_list_and_a_disconnect_makes_ours_unknown() {
        let client = negotiated(&["build", "deploy"]);
        assert_eq!(rename(client, 1, "build", "ship"), PhuxClientResult::Ok);
        let _ = sent(client);
        // Someone else renamed deploy meanwhile: the list follows, ours waits.
        assert_eq!(
            feed(client, &changed(b"deploy\0prod")),
            PhuxClientResult::Ok
        );
        assert_eq!(names(client), ["build", "prod"]);
        assert_eq!(info(client).1, STATUS_PENDING);
        // A malformed broadcast changes nothing and fails nothing.
        assert_eq!(
            feed(client, &changed(b"no separator")),
            PhuxClientResult::Ok
        );
        assert_eq!(
            unsafe { crate::phux_client_disconnect(client) },
            PhuxClientResult::Ok
        );
        assert_eq!(info(client).1, STATUS_UNKNOWN_OUTCOME);
        unsafe { crate::phux_client_free(client) };
    }

    fn hello_ok() -> FrameKind {
        FrameKind::HelloOk {
            protocol_major: crate::PROTOCOL_VERSION.major,
            protocol_minor: crate::PROTOCOL_VERSION.minor,
            protocol_patch: crate::PROTOCOL_VERSION.patch,
            server_caps: phux_protocol::caps::ServerCapabilities::new(),
            server_id: b"server".to_vec(),
            selected_profile: phux_protocol::BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: phux_protocol::caps::BootstrapLimits::new(1024, 1024)
                .expect("limits"),
        }
    }

    fn subscription() -> FrameKind {
        FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: SESSION_NAME_KEY.to_owned(),
        }
    }

    #[test]
    fn a_follower_subscribes_right_after_hello_ok_and_hears_other_clients_renames() {
        // A client that has queued HELLO and not yet heard HELLO_OK.
        let client = negotiated(&["build", "deploy"]);
        unsafe {
            (*client).inner.protocol_ready = false;
            (*client).inner.hello_queued = true;
        }
        assert_eq!(
            unsafe { phux_client_follow_session_names(client) },
            PhuxClientResult::Ok
        );
        assert!(sent(client).is_empty(), "nothing before HELLO_OK");
        assert_eq!(feed(client, &hello_ok()), PhuxClientResult::Ok);
        // One read-only subscription, and never an attach.
        assert_eq!(sent(client), [subscription()]);

        // Another client's rename moves the list, though this one renamed
        // nothing; no rename of ours is pending or settled by it.
        assert_eq!(feed(client, &changed(b"build\0ship")), PhuxClientResult::Ok);
        assert_eq!(names(client), ["ship", "deploy"]);
        let (_, status, revision, _) = info(client);
        assert_eq!((status, revision), (STATUS_NONE, 1));

        // Following again, or renaming, does not subscribe a second time.
        assert_eq!(
            unsafe { phux_client_follow_session_names(client) },
            PhuxClientResult::Ok
        );
        assert!(sent(client).is_empty());
        assert_eq!(rename(client, 1, "ship", "sail"), PhuxClientResult::Ok);
        let frames = sent(client);
        assert_eq!(frames.len(), 2, "write and confirmation read only");
        assert!(!frames.contains(&subscription()));
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn following_on_a_negotiated_client_subscribes_at_once_and_not_after_detach() {
        let client = negotiated(&["build"]);
        assert_eq!(
            unsafe { phux_client_follow_session_names(client) },
            PhuxClientResult::Ok
        );
        assert_eq!(sent(client), [subscription()]);
        unsafe { crate::phux_client_free(client) };

        // A client that never follows subscribes only at its first rename.
        let lazy = negotiated(&["build"]);
        unsafe {
            (*lazy).inner.protocol_ready = false;
            (*lazy).inner.hello_queued = true;
        }
        assert_eq!(feed(lazy, &hello_ok()), PhuxClientResult::Ok);
        assert!(sent(lazy).is_empty());
        unsafe { crate::phux_client_free(lazy) };

        let ended = negotiated(&["build"]);
        unsafe { (*ended).inner.detached = true };
        assert_eq!(
            unsafe { phux_client_follow_session_names(ended) },
            PhuxClientResult::InvalidState
        );
        assert!(sent(ended).is_empty());
        unsafe { crate::phux_client_free(ended) };
    }

    #[test]
    fn a_rename_is_refused_outside_its_lifecycle_or_with_a_bad_name() {
        let client = negotiated(&["build"]);
        unsafe { (*client).inner.protocol_ready = false };
        assert_eq!(
            rename(client, 1, "build", "x"),
            PhuxClientResult::InvalidState
        );
        unsafe { (*client).inner.protocol_ready = true };
        assert_eq!(
            rename(client, 1, "build", ""),
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(
            rename(client, 1, "build", "a\0b"),
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(rename(client, 1, "build", "x"), PhuxClientResult::Ok);
        assert_eq!(
            rename(client, 2, "build", "y"),
            PhuxClientResult::InvalidState,
            "one rename at a time"
        );
        unsafe { crate::phux_client_free(client) };
    }
}
