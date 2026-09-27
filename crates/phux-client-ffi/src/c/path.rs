//! C ABI for capability-gated, correlated host path queries.
//! Results are copied to a bounded borrow-retained batch when taken. The
//! caller owns shell escaping and any later target-pane input lease.

use std::mem;

use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::ids::SatelliteHost;

use crate::c::client::Client;
use crate::c::directory::{request_host, request_path};
use crate::c::error::{BridgeError, bytes_in, check_struct};
use crate::c::types::{PhuxBytes, bytes_out};
use crate::c::{PhuxClient, PhuxClientResult, with_client_mut, with_client_ref};
use crate::projection::outcome::{self, PathError, PathKind, PathStatus};

/// One path query; empty host targets the serving host.
#[repr(C)]
#[derive(Debug)]
pub struct PhuxPathQuery {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub root: PhuxBytes,
    pub query: PhuxBytes,
    pub recursive: bool,
    pub host: PhuxBytes,
}

fn parse_query(
    request: &PhuxPathQuery,
) -> Result<(String, String, Option<SatelliteHost>), BridgeError> {
    check_struct(
        request.size,
        mem::size_of::<PhuxPathQuery>(),
        request.version,
    )?;
    // SAFETY: caller guarantees all nonempty spans readable for the call.
    let root = request_path(unsafe { bytes_in(request.root.data, request.root.len) }?)?;
    // SAFETY: same caller contract.
    let query = request_path(unsafe { bytes_in(request.query.data, request.query.len) }?)?;
    // SAFETY: same caller contract.
    let host = request_host(unsafe { bytes_in(request.host.data, request.host.len) }?)?;
    Ok((root.to_owned(), query.to_owned(), host))
}

fn queue_query(client: &mut Client, request: &PhuxPathQuery) -> Result<u32, BridgeError> {
    let (root, query, host) = parse_query(request)?;
    client.operations.check_request_id(request.request_id)?;
    let sent = client.control().path_query_with_id(
        request.request_id,
        root,
        query,
        request.recursive,
        host,
    );
    if !sent {
        return Ok(0);
    }
    client.operations.consume_request_id(request.request_id);
    client.drain_outbound();
    Ok(request.request_id)
}

/// Queue `PATH_QUERY` and write its correlation; zero means unsupported,
/// disconnected or full (no frame sent). Paths are never pasted here.
///
/// # Safety
/// Client is live on its owning thread; request and `out_id` are writable/readable
/// respectively, disjoint, and the request's nonempty spans are readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_path_query(
    client: *mut PhuxClient,
    request: *const PhuxPathQuery,
    out_id: *mut u32,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: validated against caller's readable/writable contract.
        let request =
            unsafe { request.as_ref() }.ok_or_else(|| BridgeError::invalid("request is null"))?;
        let out =
            unsafe { out_id.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        *out = queue_query(client, request)?;
        Ok(())
    })
}

/// Whether the current handshake advertised the extended `PATH_QUERY` bit.
///
/// # Safety
/// Client is live and `out_supported` writable and disjoint from it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_path_query_supported(
    client: *const PhuxClient,
    out_supported: *mut bool,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies writable output.
        let out = unsafe { out_supported.as_mut() }
            .ok_or_else(|| BridgeError::invalid("output is null"))?;
        let control = client.control();
        *out = control.handshake_ready()
            && control
                .server()
                .is_some_and(|server| server.has_ext(ServerFeatureExt::PathQuery));
        drop(control);
        Ok(())
    })
}

/// Drain runtime answers into the C borrow-retained batch.
///
/// This replaces the old batch; call `phux_client_path_answer_get` for each
/// index. The server cannot enqueue more than 128 undrained/outstanding answers.
///
/// # Safety
/// Client is live on its owning thread; `out_count` is writable and disjoint.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_path_answers_take(
    client: *mut PhuxClient,
    out_count: *mut usize,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies writable output.
        let count =
            unsafe { out_count.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        let answers = client.control().take_path_answers();
        client.path_answers = answers.into_iter().map(outcome::path_result).collect();
        *count = client.path_answers.len();
        Ok(())
    })
}

/// Typed header of a correlated answer. UTF-8 spans are borrowed until the
/// next mutable call on this client; failure 4 is local cancellation.
#[repr(C)]
#[derive(Debug)]
pub struct PhuxPathAnswer {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub root: PhuxBytes,
    pub parent: PhuxBytes,
    pub has_parent: bool,
    pub row_count: usize,
    /// 0 complete, 1 warming, 2 truncated; ignored when `has_error`.
    pub status: u32,
    pub has_error: bool,
    /// 0 not found, 1 denied, 2 not a directory, 3 other, 4 unanswered.
    pub error: u32,
    pub message: PhuxBytes,
}

/// Borrow the indexed answer; invalid indices return `InvalidArgument`.
///
/// # Safety
/// Client is live and out is initialized, writable and disjoint from it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_path_answer_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxPathAnswer,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies writable output.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxPathAnswer>(), out.version)?;
        let answer = client
            .path_answers
            .get(index)
            .ok_or_else(|| BridgeError::invalid("path answer index out of range"))?;
        *out = PhuxPathAnswer {
            size: out.size,
            version: out.version,
            request_id: answer.request_id,
            root: bytes_out(answer.root.as_bytes()),
            parent: bytes_out(answer.parent.as_deref().unwrap_or_default().as_bytes()),
            has_parent: answer.parent.is_some(),
            row_count: answer.rows.len(),
            status: match answer.status {
                Some(PathStatus::Warming) => 1,
                Some(PathStatus::Truncated) => 2,
                _ => 0,
            },
            has_error: answer.error.is_some(),
            error: match answer.error {
                Some(PathError::PermissionDenied) => 1,
                Some(PathError::NotADirectory) => 2,
                Some(PathError::Other) => 3,
                Some(PathError::Unanswered) => 4,
                _ => 0,
            },
            message: bytes_out(answer.message.as_bytes()),
        };
        Ok(())
    })
}

/// One absolute matched path; kind 0 file, 1 directory, 2 symlink.
#[repr(C)]
#[derive(Debug)]
pub struct PhuxPathRow {
    pub size: usize,
    pub version: u32,
    pub path: PhuxBytes,
    pub kind: u32,
}

/// Borrow a row in the indexed answer; invalid indices return `InvalidArgument`.
///
/// # Safety
/// Client is live and out is initialized, writable and disjoint from it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_path_row_get(
    client: *const PhuxClient,
    answer_index: usize,
    row_index: usize,
    out: *mut PhuxPathRow,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies writable output.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxPathRow>(), out.version)?;
        let row = client
            .path_answers
            .get(answer_index)
            .and_then(|answer| answer.rows.get(row_index))
            .ok_or_else(|| BridgeError::invalid("path row index out of range"))?;
        *out = PhuxPathRow {
            size: out.size,
            version: out.version,
            path: bytes_out(row.path.as_bytes()),
            kind: match row.kind {
                PathKind::File => 0,
                PathKind::Directory => 1,
                PathKind::Symlink => 2,
            },
        };
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::c::client::{Client, Limits};
    use crate::c::types::ABI_VERSION;
    use phux_protocol::PROTOCOL_VERSION;
    use phux_protocol::caps::{
        BootstrapLimits, BootstrapProfile, Layer, LayerSet, ServerCapabilities, ServerFeatureExtSet,
    };
    use phux_protocol::wire::frame::{
        FrameKind, PathKind as WireKind, PathResults, PathRow, PathStatus as WireStatus,
    };

    fn new_client() -> *mut PhuxClient {
        let inner = Client::new(Limits {
            bootstrap_chunk: 1024,
            history_page: 1024,
            history_page_rows: 128,
            history_cache_bytes: 4096,
            history_materialized_rows: 1024,
            history_prefetch_rows: 64,
        });
        Box::into_raw(Box::new(PhuxClient::new(inner)))
    }

    /// Complete the handshake with a server that advertises `PATH_QUERY`.
    fn negotiate_path_query(client: *mut PhuxClient) {
        let inner = unsafe { &mut (*client).inner };
        inner.control().connection_opened();
        let _ = inner.control().take_outbound();
        inner
            .control()
            .feed(FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new()
                    .with_layers(LayerSet::with(&[Layer::L3]))
                    .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::PathQuery])),
                server_id: vec![1; 16],
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::new(1024, 1024).unwrap(),
            })
            .unwrap();
        let _ = inner.control().take_outbound();
    }

    fn feed_results(client: *mut PhuxClient) {
        let mut encoded = bytes::BytesMut::new();
        FrameKind::PathResults {
            request_id: 75,
            result: Ok(PathResults {
                root: "/tmp".into(),
                parent: Some("/".into()),
                rows: vec![PathRow {
                    path: "/tmp/my log".into(),
                    kind: WireKind::File,
                }],
                status: WireStatus::Complete,
            }),
        }
        .encode(&mut encoded);
        assert_eq!(
            unsafe { crate::c::phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) },
            PhuxClientResult::Ok
        );
    }

    fn first_answer(client: *mut PhuxClient) -> PhuxPathAnswer {
        let mut count = 0;
        assert_eq!(
            unsafe { phux_client_path_answers_take(client, &raw mut count) },
            PhuxClientResult::Ok
        );
        assert_eq!(count, 1);
        let mut answer = PhuxPathAnswer {
            size: mem::size_of::<PhuxPathAnswer>(),
            version: ABI_VERSION,
            request_id: 0,
            root: PhuxBytes::default(),
            parent: PhuxBytes::default(),
            has_parent: false,
            row_count: 0,
            status: 0,
            has_error: false,
            error: 0,
            message: PhuxBytes::default(),
        };
        assert_eq!(
            unsafe { phux_client_path_answer_get(client, 0, &raw mut answer) },
            PhuxClientResult::Ok
        );
        answer
    }

    fn first_row(client: *mut PhuxClient) -> PhuxPathRow {
        let mut row = PhuxPathRow {
            size: mem::size_of::<PhuxPathRow>(),
            version: ABI_VERSION,
            path: PhuxBytes::default(),
            kind: 99,
        };
        assert_eq!(
            unsafe { phux_client_path_row_get(client, 0, 0, &raw mut row) },
            PhuxClientResult::Ok
        );
        row
    }

    #[test]
    fn c_query_is_gated_and_projects_a_correlated_path_without_pasting() {
        let client = new_client();
        let request = PhuxPathQuery {
            size: mem::size_of::<PhuxPathQuery>(),
            version: ABI_VERSION,
            request_id: 75,
            root: bytes_out(b"/tmp"),
            query: bytes_out(b"log"),
            recursive: true,
            host: bytes_out(b"peer"),
        };
        let mut id = 1;
        assert_eq!(
            unsafe { phux_client_path_query(client, &raw const request, &raw mut id) },
            PhuxClientResult::Ok
        );
        assert_eq!(id, 0, "an unnegotiated peer never sees the frame");
        assert!(unsafe { &*client }.inner.outgoing.is_empty());

        negotiate_path_query(client);
        assert_eq!(
            unsafe { phux_client_path_query(client, &raw const request, &raw mut id) },
            PhuxClientResult::Ok
        );
        assert_eq!(id, 75);
        let (frame, remaining) = FrameKind::decode(&unsafe { &*client }.inner.outgoing[0]).unwrap();
        assert!(remaining.is_empty());
        assert!(matches!(
            frame,
            FrameKind::PathQuery {
                request_id: 75,
                host: Some(_),
                recursive: true,
                ..
            }
        ));

        feed_results(client);
        let answer = first_answer(client);
        assert_eq!(
            (answer.request_id, answer.row_count, answer.status),
            (75, 1, 0)
        );
        assert_eq!(
            unsafe { bytes_in(answer.root.data, answer.root.len) }.unwrap(),
            b"/tmp"
        );
        let row = first_row(client);
        assert_eq!(
            unsafe { bytes_in(row.path.data, row.path.len) }.unwrap(),
            b"/tmp/my log"
        );
        assert_eq!(row.kind, 0);
        unsafe { crate::c::phux_client_free(client) };
    }
}
