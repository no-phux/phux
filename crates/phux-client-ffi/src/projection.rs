//! Named-projection L3 key ops on the C FFI (ADR-0129).
//!
//! Get/set/delete for keys shaped `<prefix>.layout/v1/<session-id>` only.
//! This is not a new server resource: the frames are the ordinary Group-1
//! `GET_METADATA` / `SET_METADATA` / `DELETE_METADATA` verbs. SET and DELETE
//! have no success reply, so a confirming GET is queued on an internal
//! request id (the same last-write-wins barrier the CLI layout ops use).
//! Values stay opaque bytes; the host owns the §3.2 CBOR envelope.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared by the bridge dispatcher and Client"
)]

use std::mem;

use phux_client_core::layout::{
    LAYOUT_METADATA_GROUP, MAX_LAYOUT_METADATA_BYTES, projection_key_session,
};
use phux_protocol::wire::frame::{FrameKind, Scope};

use crate::client::Client;
use crate::error::{BridgeError, bytes_in, check_struct};
use crate::types::{PhuxBytes, bytes_out};
use crate::{PhuxClient, PhuxClientResult, with_client_mut, with_client_ref};

/// Spec-adjacent key bound; longer names are refused before queueing.
pub const MAX_PROJECTION_KEY_BYTES: usize = 4096;

const STATUS_NONE: u32 = 0;
const STATUS_PENDING: u32 = 1;
const STATUS_OK: u32 = 2;
const STATUS_REFUSED: u32 = 3;
const STATUS_UNKNOWN_OUTCOME: u32 = 4;

const OP_NONE: u32 = 0;
const OP_GET: u32 = 1;
const OP_SET: u32 = 2;
const OP_DELETE: u32 = 3;

#[derive(Clone, Copy)]
enum Op {
    Get,
    Set,
    Delete,
}

impl Op {
    const fn as_u32(self) -> u32 {
        match self {
            Self::Get => OP_GET,
            Self::Set => OP_SET,
            Self::Delete => OP_DELETE,
        }
    }
}

/// The one named-projection operation this client may have outstanding.
pub(crate) struct Projection {
    request_id: u32,
    confirm_id: Option<u32>,
    status: u32,
    op: u32,
    session_id: u32,
    key: Vec<u8>,
    expected: Option<Vec<u8>>,
    value: Option<Vec<u8>>,
    message: Vec<u8>,
}

impl Default for Projection {
    fn default() -> Self {
        Self {
            request_id: 0,
            confirm_id: None,
            status: STATUS_NONE,
            op: OP_NONE,
            session_id: 0,
            key: Vec::new(),
            expected: None,
            value: None,
            message: Vec::new(),
        }
    }
}

impl Projection {
    const fn pending(&self) -> bool {
        self.status == STATUS_PENDING
    }

    fn awaits(&self, request_id: u32) -> bool {
        self.pending() && (self.request_id == request_id || self.confirm_id == Some(request_id))
    }

    fn begin(
        &mut self,
        request_id: u32,
        confirm_id: Option<u32>,
        op: Op,
        session_id: u32,
        key: &[u8],
        expected: Option<Vec<u8>>,
    ) {
        *self = Self {
            request_id,
            confirm_id,
            status: STATUS_PENDING,
            op: op.as_u32(),
            session_id,
            key: key.to_vec(),
            expected,
            value: None,
            message: Vec::new(),
        };
    }

    fn settle(&mut self, status: u32, value: Option<Vec<u8>>, message: &str) {
        self.status = status;
        self.confirm_id = None;
        self.value = value;
        self.message = message.as_bytes().to_vec();
    }

    /// A pending op can no longer be confirmed on this connection.
    pub(crate) fn disconnect(&mut self) {
        self.confirm_id = None;
        if self.pending() {
            self.settle(
                STATUS_UNKNOWN_OUTCOME,
                None,
                "the connection ended before the named projection operation was confirmed",
            );
        }
    }
}

/// Consume this module's frames; pass every other frame on.
/// Runs ahead of the workspace dispatcher so a confirming GET on an
/// internal id is not swallowed as a workspace metadata reply.
pub(crate) fn dispatch(client: &mut Client, frame: FrameKind) -> Option<FrameKind> {
    match frame {
        FrameKind::MetadataValue { request_id, value } if client.projection.awaits(request_id) => {
            receive(client, value);
            None
        }
        FrameKind::Error {
            request_id: Some(request_id),
            message,
            ..
        } if client.projection.awaits(request_id) => {
            client.projection.settle(STATUS_REFUSED, None, &message);
            None
        }
        other => Some(other),
    }
}

fn receive(client: &mut Client, value: Option<Vec<u8>>) {
    if value
        .as_ref()
        .is_some_and(|bytes| bytes.len() > MAX_LAYOUT_METADATA_BYTES)
    {
        client.projection.settle(
            STATUS_REFUSED,
            None,
            "named projection metadata exceeds 256 KiB",
        );
        return;
    }
    match client.projection.op {
        OP_GET => client.projection.settle(STATUS_OK, value, ""),
        OP_SET => {
            let matched = client.projection.expected.as_ref() == value.as_ref();
            if matched {
                client.projection.settle(STATUS_OK, value, "");
            } else {
                client.projection.settle(
                    STATUS_REFUSED,
                    value,
                    "named projection write was not confirmed (concurrent writer or over-cap drop)",
                );
            }
        }
        OP_DELETE => {
            if value.is_none() {
                client.projection.settle(STATUS_OK, None, "");
            } else {
                client.projection.settle(
                    STATUS_REFUSED,
                    value,
                    "named projection delete was not confirmed (concurrent writer)",
                );
            }
        }
        _ => client
            .projection
            .settle(STATUS_REFUSED, None, "unexpected named projection reply"),
    }
}

fn request_key(bytes: &[u8]) -> Result<(&str, u32), BridgeError> {
    if bytes.len() > MAX_PROJECTION_KEY_BYTES {
        return Err(BridgeError::invalid(
            "named projection key exceeds 4096 bytes",
        ));
    }
    if bytes.contains(&0) {
        return Err(BridgeError::invalid("named projection key contains NUL"));
    }
    let key = std::str::from_utf8(bytes)
        .map_err(|_| BridgeError::invalid("named projection key is not UTF-8"))?;
    let session = projection_key_session(key).ok_or_else(|| {
        BridgeError::invalid("named projection key must be <prefix>.layout/v1/<session-id>")
    })?;
    Ok((key, session.get()))
}

fn ensure_ready(client: &Client) -> Result<(), BridgeError> {
    if !client.protocol_ready || client.detached {
        return Err(BridgeError::state(
            "named projection ops need a negotiated connection",
        ));
    }
    if !client.l3_metadata {
        return Err(BridgeError::state("server does not advertise L3 metadata"));
    }
    Ok(())
}

fn queue(
    client: &mut Client,
    request_id: u32,
    key: &str,
    session_id: u32,
    op: Op,
    value: Option<Vec<u8>>,
) -> Result<(), BridgeError> {
    ensure_ready(client)?;
    if client.projection.pending() {
        return Err(BridgeError::state(
            "a named projection operation is already pending",
        ));
    }
    client.operations.check_request_id(request_id)?;
    let frames = match op {
        Op::Get => 1,
        Op::Set | Op::Delete => 2,
    };
    if client.outgoing.len() + frames > crate::operations::MAX_OPERATIONS {
        return Err(BridgeError::state(
            "outgoing queue is full; drain outgoing frames before a named projection op",
        ));
    }
    let confirm_id = match op {
        Op::Get => {
            client.queue_frame(&FrameKind::GetMetadata {
                request_id,
                scope: Scope::Group(LAYOUT_METADATA_GROUP),
                key: key.to_owned(),
            })?;
            None
        }
        Op::Set => {
            let bytes = value
                .clone()
                .ok_or_else(|| BridgeError::invalid("named projection set needs a value"))?;
            let confirm = client.workspace.reserve_internal()?;
            client.queue_frame(&FrameKind::SetMetadata {
                request_id,
                scope: Scope::Group(LAYOUT_METADATA_GROUP),
                key: key.to_owned(),
                value: bytes,
            })?;
            client.queue_frame(&FrameKind::GetMetadata {
                request_id: confirm,
                scope: Scope::Group(LAYOUT_METADATA_GROUP),
                key: key.to_owned(),
            })?;
            Some(confirm)
        }
        Op::Delete => {
            let confirm = client.workspace.reserve_internal()?;
            client.queue_frame(&FrameKind::DeleteMetadata {
                request_id,
                scope: Scope::Group(LAYOUT_METADATA_GROUP),
                key: key.to_owned(),
            })?;
            client.queue_frame(&FrameKind::GetMetadata {
                request_id: confirm,
                scope: Scope::Group(LAYOUT_METADATA_GROUP),
                key: key.to_owned(),
            })?;
            Some(confirm)
        }
    };
    client.operations.consume_request_id(request_id);
    client.projection.begin(
        request_id,
        confirm_id,
        op,
        session_id,
        key.as_bytes(),
        value,
    );
    Ok(())
}

/// Whether `HELLO_OK` advertised L3 metadata.
///
/// # Safety
/// Client is live and unmodified for the call; `out_supported` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_projection_supported(
    client: *const PhuxClient,
    out_supported: *mut bool,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        let out = unsafe { out_supported.as_mut() }
            .ok_or_else(|| BridgeError::invalid("supported output is null"))?;
        *out = client.l3_metadata;
        Ok(())
    })
}

/// Queue `GET_METADATA` for a named projection key.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread; a nonempty
/// `key` span is readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_projection_get(
    client: *mut PhuxClient,
    request_id: u32,
    key: PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let (key, session_id) = request_key(unsafe { bytes_in(key.data, key.len) }?)?;
        queue(client, request_id, key, session_id, Op::Get, None)
    })
}

/// Queue `SET_METADATA` plus a confirming `GET_METADATA` for a named
/// projection key. Values are opaque; larger than 256 KiB is refused here.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread; nonempty
/// spans are readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_projection_set(
    client: *mut PhuxClient,
    request_id: u32,
    key: PhuxBytes,
    value: PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let (key, session_id) = request_key(unsafe { bytes_in(key.data, key.len) }?)?;
        let bytes = unsafe { bytes_in(value.data, value.len) }?;
        if bytes.len() > MAX_LAYOUT_METADATA_BYTES {
            return Err(BridgeError::invalid(
                "named projection value exceeds 256 KiB",
            ));
        }
        queue(
            client,
            request_id,
            key,
            session_id,
            Op::Set,
            Some(bytes.to_vec()),
        )
    })
}

/// Queue `DELETE_METADATA` plus a confirming `GET_METADATA` for a named
/// projection key. Success is an absent confirming read.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread; a nonempty
/// `key` span is readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_projection_delete(
    client: *mut PhuxClient,
    request_id: u32,
    key: PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let (key, session_id) = request_key(unsafe { bytes_in(key.data, key.len) }?)?;
        queue(client, request_id, key, session_id, Op::Delete, None)
    })
}

/// Latest named-projection op. Initialize size/version. Spans are borrowed
/// until the next mutable client call.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxProjectionInfo {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub status: u32,
    pub op: u32,
    pub session_id: u32,
    pub present: bool,
    pub key: PhuxBytes,
    pub value: PhuxBytes,
    pub message: PhuxBytes,
}

/// Poll the outstanding named-projection operation.
///
/// # Safety
/// Client is live and unmodified for the call; `out_info` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_projection_info(
    client: *const PhuxClient,
    out_info: *mut PhuxProjectionInfo,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        let out = unsafe { out_info.as_mut() }
            .ok_or_else(|| BridgeError::invalid("projection info output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxProjectionInfo>(), out.version)?;
        let value = client.projection.value.as_deref().unwrap_or(&[]);
        *out = PhuxProjectionInfo {
            size: mem::size_of::<PhuxProjectionInfo>(),
            version: crate::ABI_VERSION,
            request_id: client.projection.request_id,
            status: client.projection.status,
            op: client.projection.op,
            session_id: client.projection.session_id,
            present: client.projection.value.is_some(),
            key: bytes_out(&client.projection.key),
            value: bytes_out(value),
            message: bytes_out(&client.projection.message),
        };
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Limits;
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::{BootstrapLimits, ServerCapabilities};
    use phux_protocol::wire::frame::ErrorCode;

    fn span(bytes: &[u8]) -> PhuxBytes {
        PhuxBytes {
            data: bytes.as_ptr(),
            len: bytes.len(),
        }
    }

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
        inner.l3_metadata = true;
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

    fn info(client: *mut PhuxClient) -> PhuxProjectionInfo {
        let mut out = PhuxProjectionInfo {
            size: mem::size_of::<PhuxProjectionInfo>(),
            version: crate::ABI_VERSION,
            request_id: 0,
            status: u32::MAX,
            op: 0,
            session_id: 0,
            present: false,
            key: PhuxBytes::default(),
            value: PhuxBytes::default(),
            message: PhuxBytes::default(),
        };
        assert_eq!(
            unsafe { phux_client_projection_info(client, &raw mut out) },
            PhuxClientResult::Ok
        );
        out
    }

    fn queued(client: *mut PhuxClient) -> Vec<FrameKind> {
        unsafe { &(*client).inner.outgoing }
            .iter()
            .map(|bytes| FrameKind::decode(bytes).expect("queued frame decodes").0)
            .collect()
    }

    fn get(client: *mut PhuxClient, request_id: u32, key: &[u8]) -> PhuxClientResult {
        unsafe { phux_client_projection_get(client, request_id, span(key)) }
    }

    fn set(client: *mut PhuxClient, request_id: u32, key: &[u8], value: &[u8]) -> PhuxClientResult {
        unsafe { phux_client_projection_set(client, request_id, span(key), span(value)) }
    }

    fn delete(client: *mut PhuxClient, request_id: u32, key: &[u8]) -> PhuxClientResult {
        unsafe { phux_client_projection_delete(client, request_id, span(key)) }
    }

    #[test]
    fn get_queues_group1_get_metadata_and_returns_the_value() {
        let client = negotiated();
        let key = b"myapp.layout/v1/7";
        assert_eq!(get(client, 1, key), PhuxClientResult::Ok);
        let sent = queued(client);
        assert_eq!(sent.len(), 1);
        assert!(
            matches!(
                &sent[0],
                FrameKind::GetMetadata {
                    request_id: 1,
                    scope: Scope::Group(group),
                    key: k,
                } if *group == LAYOUT_METADATA_GROUP && k.as_bytes() == key
            ),
            "GET must be Group 1 on the named key; got {sent:?}"
        );
        let snapshot = info(client);
        assert_eq!(snapshot.status, STATUS_PENDING);
        assert_eq!(snapshot.op, OP_GET);
        assert_eq!(snapshot.session_id, 7);
        assert_eq!(
            feed(
                client,
                &FrameKind::MetadataValue {
                    request_id: 1,
                    value: Some(b"cbor-bytes".to_vec()),
                }
            ),
            PhuxClientResult::Ok
        );
        let done = info(client);
        assert_eq!(done.status, STATUS_OK);
        assert!(done.present);
        assert_eq!(
            unsafe { std::slice::from_raw_parts(done.value.data, done.value.len) },
            b"cbor-bytes"
        );
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn get_absent_is_ok_with_present_false() {
        let client = negotiated();
        assert_eq!(get(client, 1, b"scratch.layout/v1/3"), PhuxClientResult::Ok);
        assert_eq!(
            feed(
                client,
                &FrameKind::MetadataValue {
                    request_id: 1,
                    value: None,
                }
            ),
            PhuxClientResult::Ok
        );
        let done = info(client);
        assert_eq!(done.status, STATUS_OK);
        assert!(!done.present);
        assert_eq!(done.value.len, 0);
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn set_queues_set_then_confirming_get_and_requires_byte_match() {
        let client = negotiated();
        let key = b"myapp.layout/v1/7";
        let value = b"envelope";
        assert_eq!(set(client, 4, key, value), PhuxClientResult::Ok);
        let sent = queued(client);
        assert_eq!(sent.len(), 2);
        assert!(
            matches!(
                &sent[0],
                FrameKind::SetMetadata {
                    request_id: 4,
                    scope: Scope::Group(group),
                    key: k,
                    value: v,
                } if *group == LAYOUT_METADATA_GROUP && k.as_bytes() == key && v == value
            ),
            "SET must use the host request id; got {sent:?}"
        );
        let FrameKind::GetMetadata {
            request_id: confirm,
            ..
        } = sent[1]
        else {
            panic!("expected confirming GET, got {:?}", sent[1]);
        };
        assert!(confirm >= crate::workspace::INTERNAL_START);
        assert_eq!(
            feed(
                client,
                &FrameKind::MetadataValue {
                    request_id: confirm,
                    value: Some(value.to_vec()),
                }
            ),
            PhuxClientResult::Ok
        );
        let done = info(client);
        assert_eq!(done.status, STATUS_OK);
        assert_eq!(done.op, OP_SET);
        assert!(done.present);
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn set_mismatch_is_refused_as_unconfirmed() {
        let client = negotiated();
        assert_eq!(
            set(client, 4, b"myapp.layout/v1/7", b"wrote"),
            PhuxClientResult::Ok
        );
        let confirm = match &queued(client)[1] {
            FrameKind::GetMetadata { request_id, .. } => *request_id,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            feed(
                client,
                &FrameKind::MetadataValue {
                    request_id: confirm,
                    value: Some(b"someone-else".to_vec()),
                }
            ),
            PhuxClientResult::Ok
        );
        let done = info(client);
        assert_eq!(done.status, STATUS_REFUSED);
        assert!(done.present);
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn delete_queues_delete_then_confirming_get_and_succeeds_when_absent() {
        let client = negotiated();
        let key = b"myapp.layout/v1/9";
        assert_eq!(delete(client, 8, key), PhuxClientResult::Ok);
        let sent = queued(client);
        assert_eq!(sent.len(), 2);
        assert!(
            matches!(
                &sent[0],
                FrameKind::DeleteMetadata {
                    request_id: 8,
                    key: k,
                    ..
                } if k.as_bytes() == key
            ),
            "DELETE must use the host request id; got {sent:?}"
        );
        let FrameKind::GetMetadata {
            request_id: confirm,
            ..
        } = sent[1]
        else {
            panic!("expected confirming GET, got {:?}", sent[1]);
        };
        assert_eq!(
            feed(
                client,
                &FrameKind::MetadataValue {
                    request_id: confirm,
                    value: None,
                }
            ),
            PhuxClientResult::Ok
        );
        let done = info(client);
        assert_eq!(done.status, STATUS_OK);
        assert_eq!(done.op, OP_DELETE);
        assert!(!done.present);
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn invalid_keys_are_refused_before_queueing() {
        let client = negotiated();
        for key in [
            b".layout/v1/7".as_slice(),
            b"not-a-layout-key".as_slice(),
            b"myapp.layout/v1/07".as_slice(),
            b"a.layout/v1/b.layout/v1/7".as_slice(),
            b"phux.tui.layout/v1".as_slice(),
        ] {
            assert_eq!(
                get(client, 1, key),
                PhuxClientResult::InvalidArgument,
                "{key:?} must not queue"
            );
            assert!(unsafe { (*client).inner.outgoing.is_empty() });
        }
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn default_tui_key_is_a_valid_projection_key() {
        let client = negotiated();
        assert_eq!(
            get(client, 1, b"phux.tui.layout/v1/7"),
            PhuxClientResult::Ok
        );
        let snapshot = info(client);
        assert_eq!(snapshot.session_id, 7);
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn pending_op_refuses_a_second_and_does_not_consume_the_id() {
        let client = negotiated();
        assert_eq!(get(client, 1, b"myapp.layout/v1/1"), PhuxClientResult::Ok);
        assert_eq!(
            set(client, 2, b"myapp.layout/v1/1", b"x"),
            PhuxClientResult::InvalidState
        );
        assert_eq!(queued(client).len(), 1);
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn oversize_set_is_refused_before_queueing() {
        let client = negotiated();
        let too_big = vec![0u8; MAX_LAYOUT_METADATA_BYTES + 1];
        assert_eq!(
            set(client, 1, b"myapp.layout/v1/1", &too_big),
            PhuxClientResult::InvalidArgument
        );
        assert!(unsafe { (*client).inner.outgoing.is_empty() });
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn correlated_error_settles_refused() {
        let client = negotiated();
        assert_eq!(get(client, 3, b"myapp.layout/v1/1"), PhuxClientResult::Ok);
        assert_eq!(
            feed(
                client,
                &FrameKind::Error {
                    request_id: Some(3),
                    code: ErrorCode::NotAttached,
                    message: "no metadata".into(),
                }
            ),
            PhuxClientResult::Ok
        );
        let done = info(client);
        assert_eq!(done.status, STATUS_REFUSED);
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn disconnect_while_pending_is_unknown_outcome() {
        let client = negotiated();
        assert_eq!(get(client, 1, b"myapp.layout/v1/1"), PhuxClientResult::Ok);
        assert_eq!(
            unsafe { crate::phux_client_disconnect(client) },
            PhuxClientResult::Ok
        );
        assert_eq!(info(client).status, STATUS_UNKNOWN_OUTCOME);
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn needs_negotiated_l3_and_does_not_require_attach() {
        let client = negotiated();
        assert!(
            !unsafe { (*client).inner.attached },
            "negotiated() is unattached"
        );
        assert_eq!(get(client, 1, b"myapp.layout/v1/1"), PhuxClientResult::Ok);
        unsafe { crate::phux_client_free(client) };

        let client = negotiated();
        unsafe {
            (*client).inner.l3_metadata = false;
        }
        assert_eq!(
            get(client, 1, b"myapp.layout/v1/1"),
            PhuxClientResult::InvalidState
        );
        assert!(
            unsafe { (*client).inner.outgoing.is_empty() },
            "missing L3 must not queue or consume the request id"
        );
        unsafe {
            (*client).inner.l3_metadata = true;
        }
        assert_eq!(get(client, 1, b"myapp.layout/v1/1"), PhuxClientResult::Ok);
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn hello_ok_gates_l3_from_advertised_layers() {
        for (layers, expected) in [
            (phux_protocol::LayerSet::new(), false),
            (
                phux_protocol::LayerSet::with(&[phux_protocol::Layer::L3]),
                true,
            ),
        ] {
            let mut inner = Client::new(Limits {
                bootstrap_chunk: 1024,
                history_page: 1024,
                history_page_rows: 128,
                history_cache_bytes: 4096,
                history_materialized_rows: 1024,
                history_prefetch_rows: 64,
            });
            inner.hello_queued = true;
            let client = Box::into_raw(Box::new(PhuxClient {
                inner,
                _not_send_sync: std::marker::PhantomData,
            }));
            let hello = FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new().with_layers(layers),
                server_id: b"server".to_vec(),
                selected_profile: phux_protocol::BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::new(1024, 1024).expect("limits"),
            };
            assert_eq!(feed(client, &hello), PhuxClientResult::Ok);
            let mut supported = !expected;
            assert_eq!(
                unsafe { phux_client_projection_supported(client, &raw mut supported) },
                PhuxClientResult::Ok
            );
            assert_eq!(supported, expected);
            let queued = if expected {
                PhuxClientResult::Ok
            } else {
                PhuxClientResult::InvalidState
            };
            assert_eq!(get(client, 1, b"myapp.layout/v1/1"), queued);
            unsafe { crate::phux_client_free(client) };
        }
    }
}
