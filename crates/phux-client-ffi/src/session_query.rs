//! A server's session list without attaching (`GET_STATE`, SPEC section 13).
//!
//! A client that only needs to know which sessions a server holds, such as
//! the standby coordinator of a multi-host selector, asks `GET_STATE` after
//! `HELLO_OK` instead of attaching. Attaching would make it a subscriber: it
//! would contribute its viewport to every pane's `window-size` policy (a
//! placeholder size clamps `smallest` for every other client) and stream
//! every pane's output for nothing. The reply replaces the list that
//! `phux_client_session_count` and `phux_client_session_get` read, validated
//! and bounded exactly as `ATTACHED`'s catalog is.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared by the bridge dispatcher and Client"
)]

use crate::client::Client;
use crate::error::BridgeError;
use crate::{PhuxClient, PhuxClientResult, with_client_mut, with_client_ref};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FrameKind, StateScope};

/// No query has been sent on this client.
const STATUS_NONE: u32 = 0;
/// `GET_STATE` is queued or on the wire.
const STATUS_PENDING: u32 = 1;
/// The reply replaced the session list.
const STATUS_OK: u32 = 2;
/// The server refused; the previous list is kept.
const STATUS_REFUSED: u32 = 3;
/// The connection ended before the reply arrived.
const STATUS_UNKNOWN_OUTCOME: u32 = 4;

/// The one session query this client may have outstanding.
pub(crate) struct SessionQuery {
    request_id: u32,
    status: u32,
}

impl Default for SessionQuery {
    fn default() -> Self {
        Self {
            request_id: 0,
            status: STATUS_NONE,
        }
    }
}

impl SessionQuery {
    const fn awaits(&self, request_id: u32) -> bool {
        self.status == STATUS_PENDING && self.request_id == request_id
    }

    /// A pending query can no longer be answered on this connection.
    pub(crate) const fn disconnect(&mut self) {
        if self.status == STATUS_PENDING {
            self.status = STATUS_UNKNOWN_OUTCOME;
        }
    }
}

/// Consume the reply to the outstanding query; pass every other frame on.
/// Runs ahead of the operations dispatcher, which would otherwise read an
/// uncorrelated `COMMAND_RESULT` as a protocol error.
pub(crate) fn dispatch(
    client: &mut Client,
    frame: FrameKind,
) -> Result<Option<FrameKind>, BridgeError> {
    match frame {
        FrameKind::CommandResult { request_id, result }
            if client.session_query.awaits(request_id) =>
        {
            receive(client, result)?;
            Ok(None)
        }
        FrameKind::Error {
            request_id: Some(request_id),
            ..
        } if client.session_query.awaits(request_id) => {
            client.session_query.status = STATUS_REFUSED;
            Ok(None)
        }
        other => Ok(Some(other)),
    }
}

fn receive(client: &mut Client, result: CommandResult) -> Result<(), BridgeError> {
    match result {
        CommandResult::OkWith(CommandValue::State(snapshot)) => {
            client.sessions = crate::workspace::session_summaries(snapshot)?;
            client.session_query.status = STATUS_OK;
        }
        CommandResult::Error { .. } => client.session_query.status = STATUS_REFUSED,
        _ => {
            return Err(BridgeError::protocol(
                "GET_STATE was answered with an unexpected value",
            ));
        }
    }
    Ok(())
}

/// Ask for the server's session list without attaching.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_query_sessions(
    client: *mut PhuxClient,
    request_id: u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if !client.protocol_ready || client.detached || client.attached || client.attach_queued {
            return Err(BridgeError::state(
                "a session query needs a negotiated client that has not attached",
            ));
        }
        if client.session_query.status == STATUS_PENDING {
            return Err(BridgeError::state("a session query is already pending"));
        }
        client.operations.check_request_id(request_id)?;
        if client.outgoing.len() >= crate::operations::MAX_OPERATIONS {
            return Err(BridgeError::state(
                "outgoing queue is full; drain outgoing frames before querying sessions",
            ));
        }
        client.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        })?;
        client.operations.consume_request_id(request_id);
        client.session_query = SessionQuery {
            request_id,
            status: STATUS_PENDING,
        };
        Ok(())
    })
}

/// Read the latest query's request ID and status.
///
/// # Safety
/// Client is live and unmodified for the call; both outputs are writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_session_query_status(
    client: *const PhuxClient,
    out_request_id: *mut u32,
    out_status: *mut u32,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies writable outputs when non-null.
        let id = unsafe { out_request_id.as_mut() }
            .ok_or_else(|| BridgeError::invalid("request ID output is null"))?;
        // SAFETY: as above.
        let status = unsafe { out_status.as_mut() }
            .ok_or_else(|| BridgeError::invalid("status output is null"))?;
        *id = client.session_query.request_id;
        *status = client.session_query.status;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Limits;
    use phux_protocol::wire::frame::ErrorCode;
    use phux_protocol::wire::info::{SessionInfo, SessionSnapshot};
    use phux_protocol::{ResourceId, SessionId, WindowId};

    fn negotiated() -> *mut PhuxClient {
        let mut inner = Client::new(Limits {
            bootstrap_chunk: 1024,
            history_page: 1024,
            history_page_rows: 128,
            history_cache_bytes: 4096,
            history_materialized_rows: 1024,
            history_prefetch_rows: 64,
        });
        inner.protocol_ready = true;
        Box::into_raw(Box::new(PhuxClient {
            inner,
            _not_send_sync: std::marker::PhantomData,
        }))
    }

    fn feed(client: *mut PhuxClient, frame: &FrameKind) -> PhuxClientResult {
        let mut encoded = bytes::BytesMut::new();
        frame.encode(&mut encoded);
        unsafe { crate::phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) }
    }

    fn status(client: *mut PhuxClient) -> (u32, u32) {
        let (mut id, mut status) = (0, u32::MAX);
        assert_eq!(
            unsafe { phux_client_session_query_status(client, &raw mut id, &raw mut status) },
            PhuxClientResult::Ok
        );
        (id, status)
    }

    fn state_reply(request_id: u32) -> FrameKind {
        let snapshot =
            SessionSnapshot::new(SessionId::new(1), WindowId::new(10), ResourceId::local(1))
                .with_sessions(vec![
                    SessionInfo::new(SessionId::new(1), "build"),
                    SessionInfo::new(SessionId::new(2), "deploy"),
                ]);
        FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::State(snapshot)),
        }
    }

    #[test]
    fn a_negotiated_client_lists_sessions_without_attaching() {
        let client = negotiated();
        assert_eq!(status(client), (0, STATUS_NONE));
        assert_eq!(
            unsafe { phux_client_query_sessions(client, 1) },
            PhuxClientResult::Ok
        );
        let queued = unsafe { &(*client).inner.outgoing };
        assert_eq!(queued.len(), 1, "GET_STATE and nothing else: no ATTACH");
        let (sent, _) = FrameKind::decode(&queued[0]).expect("queued frame decodes");
        assert!(matches!(
            sent,
            FrameKind::Command {
                request_id: 1,
                command: Command::GetState {
                    scope: StateScope::Server
                }
            }
        ));
        assert_eq!(status(client), (1, STATUS_PENDING));
        assert_eq!(feed(client, &state_reply(1)), PhuxClientResult::Ok);
        assert_eq!(status(client), (1, STATUS_OK));
        assert_eq!(unsafe { crate::phux_client_session_count(client) }, 2);
        let names: Vec<_> = unsafe { &(*client).inner.sessions }
            .iter()
            .map(|session| String::from_utf8(session.name.clone()).expect("UTF-8"))
            .collect();
        assert_eq!(names, ["build", "deploy"]);
        assert!(!unsafe { (*client).inner.attached });
        assert!(!unsafe { (*client).inner.attach_queued });
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn a_query_is_refused_outside_its_lifecycle() {
        let fresh = negotiated();
        unsafe { (*fresh).inner.protocol_ready = false };
        assert_eq!(
            unsafe { phux_client_query_sessions(fresh, 1) },
            PhuxClientResult::InvalidState
        );
        unsafe { crate::phux_client_free(fresh) };

        let attached = negotiated();
        unsafe { (*attached).inner.attached = true };
        assert_eq!(
            unsafe { phux_client_query_sessions(attached, 1) },
            PhuxClientResult::InvalidState
        );
        unsafe { crate::phux_client_free(attached) };

        let client = negotiated();
        assert_eq!(
            unsafe { phux_client_query_sessions(client, 4) },
            PhuxClientResult::Ok
        );
        assert_eq!(
            unsafe { phux_client_query_sessions(client, 5) },
            PhuxClientResult::InvalidState,
            "one query at a time"
        );
        assert_eq!(feed(client, &state_reply(4)), PhuxClientResult::Ok);
        assert_eq!(
            unsafe { phux_client_query_sessions(client, 4) },
            PhuxClientResult::InvalidArgument,
            "request IDs strictly increase"
        );
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn refusals_keep_the_list_and_a_disconnect_makes_the_outcome_unknown() {
        let client = negotiated();
        assert_eq!(
            unsafe { phux_client_query_sessions(client, 1) },
            PhuxClientResult::Ok
        );
        assert_eq!(feed(client, &state_reply(1)), PhuxClientResult::Ok);

        assert_eq!(
            unsafe { phux_client_query_sessions(client, 2) },
            PhuxClientResult::Ok
        );
        let refused = FrameKind::Error {
            request_id: Some(2),
            code: ErrorCode::NotAttached,
            message: "no".to_owned(),
        };
        assert_eq!(feed(client, &refused), PhuxClientResult::Ok);
        assert_eq!(status(client), (2, STATUS_REFUSED));
        assert_eq!(unsafe { crate::phux_client_session_count(client) }, 2);

        assert_eq!(
            unsafe { phux_client_query_sessions(client, 3) },
            PhuxClientResult::Ok
        );
        assert_eq!(
            unsafe { crate::phux_client_disconnect(client) },
            PhuxClientResult::Ok
        );
        assert_eq!(status(client), (3, STATUS_UNKNOWN_OUTCOME));
        unsafe { crate::phux_client_free(client) };
    }
}
