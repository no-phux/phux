//! Host directory listing over `LIST_DIRECTORY` / `DIRECTORY_LISTING`
//! (`docs/spec/L3.md` section 4, feature bit `LIST_DIRECTORY` 0x00008000).
//!
//! A go-to-directory picker asks the serving server for one directory's child
//! directories and waits for the correlated reply. The client keeps exactly
//! one listing: a new request replaces the previous one, and a reply that
//! answers anything but the latest request is dropped. That is how a picker
//! that was cancelled, or moved on to another directory, never sees a late
//! answer: the embedder simply issues its next request (or none), and the
//! stale `DIRECTORY_LISTING` falls on the floor here rather than surfacing as
//! a protocol error, because a reply outliving the request that asked for it
//! is ordinary.
//!
//! Request IDs share the embedder's strictly increasing host request space
//! (spawn, subscribe, workspace refresh and mutation), so one ledger serves
//! every correlated request. The frame carries no terminal identity, so it is
//! answered by whichever server this client is connected to: a local
//! coordinator or a registered remote host alike.
#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared by the bridge dispatcher and Client"
)]

use crate::client::Client;
use crate::error::{BridgeError, bytes_in, check_struct};
use crate::types::{PhuxBytes, bytes_out};
use crate::{PhuxClient, PhuxClientResult, with_client_mut, with_client_ref};
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::{DirectoryErrorCode, DirectoryListingResult, FrameKind};
use std::mem;

/// The spec's request path bound; longer paths are refused before queueing,
/// as the reference server would refuse them after a round trip.
pub const MAX_DIRECTORY_PATH_BYTES: usize = 4096;

/// No listing has been requested on this client.
const STATUS_NONE: u32 = 0;
/// The request is queued or on the wire; no reply yet.
const STATUS_PENDING: u32 = 1;
/// The reply is a listing.
const STATUS_LISTED: u32 = 2;
/// The reply is a typed refusal (`error_code`, `message`).
const STATUS_REFUSED: u32 = 3;
/// The connection ended before the reply arrived.
const STATUS_UNKNOWN_OUTCOME: u32 = 4;

/// Entry flag bit 0: the entry is a symbolic link resolving to a directory.
const ENTRY_FLAG_SYMLINK: u32 = 0x1;

/// The one retained listing.
pub(crate) struct DirectoryState {
    request_id: u32,
    status: u32,
    error_code: u32,
    truncated: bool,
    path: Vec<u8>,
    parent: Option<Vec<u8>>,
    entries: Vec<(Vec<u8>, bool)>,
    message: Vec<u8>,
}

impl Default for DirectoryState {
    fn default() -> Self {
        Self {
            request_id: 0,
            status: STATUS_NONE,
            error_code: 0,
            truncated: false,
            path: Vec::new(),
            parent: None,
            entries: Vec::new(),
            message: Vec::new(),
        }
    }
}

impl DirectoryState {
    fn begin(&mut self, request_id: u32, path: &[u8]) {
        *self = Self {
            request_id,
            status: STATUS_PENDING,
            path: path.to_vec(),
            ..Self::default()
        };
    }

    /// True when `request_id` is the one reply this client is waiting on.
    const fn awaits(&self, request_id: u32) -> bool {
        self.status == STATUS_PENDING && self.request_id == request_id
    }

    fn receive(&mut self, request_id: u32, result: DirectoryListingResult) {
        if !self.awaits(request_id) {
            return;
        }
        match result {
            Ok(listing) => {
                self.status = STATUS_LISTED;
                self.path = listing.path.into_bytes();
                self.parent = listing.parent.map(String::into_bytes);
                self.truncated = listing.truncated;
                self.entries = listing
                    .entries
                    .into_iter()
                    .map(|entry| (entry.name.into_bytes(), entry.is_symlink))
                    .collect();
            }
            Err(error) => {
                self.refuse(error.code, error.message.into_bytes());
                self.path = error.path.into_bytes();
            }
        }
    }

    fn refuse(&mut self, code: DirectoryErrorCode, message: Vec<u8>) {
        self.status = STATUS_REFUSED;
        self.error_code = u32::from(code.as_wire());
        self.message = message;
        self.parent = None;
        self.entries.clear();
        self.truncated = false;
    }

    /// A pending listing can no longer be answered on this connection.
    pub(crate) fn disconnect(&mut self) {
        if self.status == STATUS_PENDING {
            self.status = STATUS_UNKNOWN_OUTCOME;
            self.message = b"connection ended before the directory listing arrived".to_vec();
        }
    }
}

/// Consume the frames this module answers; pass every other frame on.
pub(crate) fn dispatch(client: &mut Client, frame: FrameKind) -> Option<FrameKind> {
    match frame {
        FrameKind::DirectoryListing { request_id, result } => {
            client.directory.receive(request_id, result);
            None
        }
        // A server that refuses the request with a correlated ERROR instead
        // of a DIRECTORY_LISTING refusal still settles the picker.
        FrameKind::Error {
            request_id: Some(request_id),
            message,
            ..
        } if client.directory.awaits(request_id) => {
            client
                .directory
                .refuse(DirectoryErrorCode::Other, message.into_bytes());
            None
        }
        other => Some(other),
    }
}

fn request_path(bytes: &[u8]) -> Result<&str, BridgeError> {
    if bytes.len() > MAX_DIRECTORY_PATH_BYTES {
        return Err(BridgeError::invalid("directory path exceeds 4096 bytes"));
    }
    if bytes.contains(&0) {
        return Err(BridgeError::invalid("directory path contains NUL"));
    }
    std::str::from_utf8(bytes).map_err(|_| BridgeError::invalid("directory path is not UTF-8"))
}

/// A satellite name's bound: the host storage of a satellite-tagged terminal
/// identity in every embedder this ABI serves.
pub const MAX_DIRECTORY_HOST_BYTES: usize = 255;

fn request_host(bytes: &[u8]) -> Result<Option<SatelliteHost>, BridgeError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.len() > MAX_DIRECTORY_HOST_BYTES {
        return Err(BridgeError::invalid("directory host exceeds 255 bytes"));
    }
    if bytes.contains(&0) {
        return Err(BridgeError::invalid("directory host contains NUL"));
    }
    let host = std::str::from_utf8(bytes)
        .map_err(|_| BridgeError::invalid("directory host is not UTF-8"))?;
    Ok(Some(SatelliteHost::new(host)))
}

/// Queue one `LIST_DIRECTORY`, replacing any previous listing. `host` names a
/// satellite of the attached hub (`docs/spec/L3.md` section 4.1), and needs
/// `LIST_DIRECTORY_HOST`: an older server skips the field and lists itself,
/// so without the bit the request is refused before anything is queued
/// rather than answered by the wrong host.
fn list_directory(
    client: &mut Client,
    request_id: u32,
    path: &str,
    host: Option<SatelliteHost>,
) -> Result<(), BridgeError> {
    client.ensure_attached()?;
    if !client.list_directory {
        return Err(BridgeError::state(
            "server does not advertise LIST_DIRECTORY",
        ));
    }
    if host.is_some() && !client.list_directory_host {
        return Err(BridgeError::state(
            "server does not advertise LIST_DIRECTORY_HOST; it would list itself",
        ));
    }
    client.operations.check_request_id(request_id)?;
    if client.outgoing.len() >= crate::operations::MAX_OPERATIONS {
        return Err(BridgeError::state(
            "outgoing queue is full; drain outgoing frames before listing a directory",
        ));
    }
    // Without a host the frame is byte-identical for servers that predate
    // LIST_DIRECTORY_HOST.
    client.queue_frame(&FrameKind::ListDirectory {
        request_id,
        path: path.to_owned(),
        host,
    })?;
    client.operations.consume_request_id(request_id);
    client.directory.begin(request_id, path.as_bytes());
    Ok(())
}

/// Queue `LIST_DIRECTORY` for `path` (empty or `~` for the serving user's
/// home, `~/rest`, or absolute) on the serving host, replacing any previous
/// listing.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread; a nonempty
/// `path` span is readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_list_directory(
    client: *mut PhuxClient,
    request_id: u32,
    path: PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: forwards the caller's readable-span contract.
        let path = request_path(unsafe { bytes_in(path.data, path.len) }?)?;
        list_directory(client, request_id, path, None)
    })
}

/// One `LIST_DIRECTORY` request, with an optional satellite host.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxDirectoryRequest {
    pub size: usize,
    pub version: u32,
    pub request_id: u32,
    pub path: PhuxBytes,
    /// Empty: the serving host, exactly `phux_client_list_directory`.
    pub host: PhuxBytes,
}

/// Queue `LIST_DIRECTORY` for `request.path` on `request.host`: a satellite
/// of the attached hub, or the serving host when empty. A nonempty host
/// needs `LIST_DIRECTORY_HOST`; see `list_directory`.
///
/// # Safety
/// Client is live and exclusively accessed on its owning thread; `request`
/// is initialized with `size`/`version` and its nonempty spans are readable
/// for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_list_directory_on(
    client: *mut PhuxClient,
    request: *const PhuxDirectoryRequest,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        // SAFETY: caller supplies the readable request when non-null.
        let request =
            unsafe { request.as_ref() }.ok_or_else(|| BridgeError::invalid("request is null"))?;
        check_struct(
            request.size,
            mem::size_of::<PhuxDirectoryRequest>(),
            request.version,
        )?;
        // SAFETY: forwards the caller's readable-span contract.
        let path = request_path(unsafe { bytes_in(request.path.data, request.path.len) }?)?;
        // SAFETY: as above.
        let host = request_host(unsafe { bytes_in(request.host.data, request.host.len) }?)?;
        list_directory(client, request.request_id, path, host)
    })
}

/// Whether `HELLO_OK` advertised `LIST_DIRECTORY_HOST`, so a nonempty
/// `PhuxDirectoryRequest.host` is listed by that satellite.
///
/// # Safety
/// Client is live and unmodified for the call; `out` is writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_directory_host_supported(
    client: *const PhuxClient,
    out: *mut bool,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        *out = client.list_directory && client.list_directory_host;
        Ok(())
    })
}

/// Whether the server answers `LIST_DIRECTORY`, and the retained listing.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxDirectoryListingInfo {
    pub size: usize,
    pub version: u32,
    pub supported: bool,
    pub truncated: bool,
    pub has_parent: bool,
    pub request_id: u32,
    pub status: u32,
    pub error_code: u32,
    pub entry_count: u32,
    pub path: PhuxBytes,
    pub parent: PhuxBytes,
    pub message: PhuxBytes,
}

/// Read the retained listing's header. Spans are borrowed until the next
/// mutable client call.
///
/// # Safety
/// Client is live and unmodified for the call; `out` is initialized with
/// `size`/`version`, writable and disjoint from client storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_directory_info(
    client: *const PhuxClient,
    out: *mut PhuxDirectoryListingInfo,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(
            out.size,
            mem::size_of::<PhuxDirectoryListingInfo>(),
            out.version,
        )?;
        let listing = &client.directory;
        let count = u32::try_from(listing.entries.len())
            .map_err(|_| BridgeError::state("directory listing too large"))?;
        *out = PhuxDirectoryListingInfo {
            size: out.size,
            version: out.version,
            supported: client.list_directory,
            truncated: listing.truncated,
            has_parent: listing.parent.is_some(),
            request_id: listing.request_id,
            status: listing.status,
            error_code: listing.error_code,
            entry_count: count,
            path: bytes_out(&listing.path),
            parent: bytes_out(listing.parent.as_deref().unwrap_or_default()),
            message: bytes_out(&listing.message),
        };
        Ok(())
    })
}

/// One child directory of the retained listing, in the server's order
/// (ascending byte order by name).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct PhuxDirectoryEntry {
    pub size: usize,
    pub version: u32,
    pub flags: u32,
    pub name: PhuxBytes,
}

/// Read entry `index` of the retained listing. The name is borrowed until
/// the next mutable client call.
///
/// # Safety
/// Client is live and unmodified for the call; `out` is initialized with
/// `size`/`version`, writable and disjoint from client storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_directory_entry_get(
    client: *const PhuxClient,
    index: usize,
    out: *mut PhuxDirectoryEntry,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        // SAFETY: caller supplies the writable output when non-null.
        let out = unsafe { out.as_mut() }.ok_or_else(|| BridgeError::invalid("output is null"))?;
        check_struct(out.size, mem::size_of::<PhuxDirectoryEntry>(), out.version)?;
        let (name, symlink) = client
            .directory
            .entries
            .get(index)
            .ok_or_else(|| BridgeError::invalid("directory entry index out of range"))?;
        *out = PhuxDirectoryEntry {
            size: out.size,
            version: out.version,
            flags: if *symlink { ENTRY_FLAG_SYMLINK } else { 0 },
            name: bytes_out(name),
        };
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Limits;
    use crate::types::ABI_VERSION;
    use phux_protocol::wire::frame::{DirectoryEntry, DirectoryListing, DirectoryListingError};

    fn client(supported: bool) -> *mut PhuxClient {
        let mut inner = Client::new(Limits {
            bootstrap_chunk: 1024,
            history_page: 1024,
            history_page_rows: 128,
            history_cache_bytes: 4096,
            history_materialized_rows: 1024,
            history_prefetch_rows: 64,
        });
        inner.protocol_ready = true;
        inner.attached = true;
        inner.list_directory = supported;
        Box::into_raw(Box::new(PhuxClient {
            inner,
            _not_send_sync: std::marker::PhantomData,
        }))
    }

    fn list(client: *mut PhuxClient, request_id: u32, path: &str) -> PhuxClientResult {
        unsafe {
            phux_client_list_directory(
                client,
                request_id,
                PhuxBytes {
                    data: path.as_ptr(),
                    len: path.len(),
                },
            )
        }
    }

    fn feed(client: *mut PhuxClient, frame: &FrameKind) -> PhuxClientResult {
        let mut encoded = bytes::BytesMut::new();
        frame.encode(&mut encoded);
        unsafe { crate::phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) }
    }

    fn listed(request_id: u32, path: &str, names: &[(&str, bool)], truncated: bool) -> FrameKind {
        FrameKind::DirectoryListing {
            request_id,
            result: Ok(DirectoryListing {
                path: path.to_owned(),
                parent: (path != "/").then(|| "/".to_owned()),
                entries: names
                    .iter()
                    .map(|(name, is_symlink)| DirectoryEntry {
                        name: (*name).to_owned(),
                        is_symlink: *is_symlink,
                    })
                    .collect(),
                truncated,
            }),
        }
    }

    fn info(client: *mut PhuxClient) -> PhuxDirectoryListingInfo {
        let mut out = PhuxDirectoryListingInfo {
            size: mem::size_of::<PhuxDirectoryListingInfo>(),
            version: ABI_VERSION,
            supported: false,
            truncated: false,
            has_parent: false,
            request_id: 0,
            status: u32::MAX,
            error_code: 0,
            entry_count: 0,
            path: PhuxBytes::default(),
            parent: PhuxBytes::default(),
            message: PhuxBytes::default(),
        };
        assert_eq!(
            unsafe { phux_client_directory_info(client, &raw mut out) },
            PhuxClientResult::Ok
        );
        out
    }

    fn text(bytes: PhuxBytes) -> String {
        let slice = unsafe { bytes_in(bytes.data, bytes.len) }.expect("borrowed span");
        String::from_utf8(slice.to_vec()).expect("UTF-8")
    }

    fn entry(client: *mut PhuxClient, index: usize) -> Result<(String, u32), PhuxClientResult> {
        let mut out = PhuxDirectoryEntry {
            size: mem::size_of::<PhuxDirectoryEntry>(),
            version: ABI_VERSION,
            flags: 0,
            name: PhuxBytes::default(),
        };
        match unsafe { phux_client_directory_entry_get(client, index, &raw mut out) } {
            PhuxClientResult::Ok => Ok((text(out.name), out.flags)),
            other => Err(other),
        }
    }

    #[test]
    fn a_listing_is_requested_by_path_and_read_back_with_its_entries() {
        let client = client(true);
        assert_eq!(info(client).status, STATUS_NONE);
        assert!(info(client).supported);
        assert_eq!(list(client, 3, "~/src"), PhuxClientResult::Ok);
        let queued = unsafe { &(*client).inner.outgoing };
        assert_eq!(queued.len(), 1);
        let (sent, _) = FrameKind::decode(&queued[0]).expect("queued frame decodes");
        assert_eq!(
            sent,
            FrameKind::ListDirectory {
                request_id: 3,
                path: "~/src".to_owned(),
                host: None,
            }
        );
        let pending = info(client);
        assert_eq!(pending.status, STATUS_PENDING);
        assert_eq!(pending.request_id, 3);

        let reply = listed(
            3,
            "/home/me/src",
            &[("cockpit", false), ("link", true)],
            true,
        );
        assert_eq!(feed(client, &reply), PhuxClientResult::Ok);
        let done = info(client);
        assert_eq!(done.status, STATUS_LISTED);
        assert!(done.truncated);
        assert!(done.has_parent);
        assert_eq!(done.entry_count, 2);
        assert_eq!(text(done.path), "/home/me/src");
        assert_eq!(text(done.parent), "/");
        assert_eq!(entry(client, 0), Ok(("cockpit".to_owned(), 0)));
        assert_eq!(
            entry(client, 1),
            Ok(("link".to_owned(), ENTRY_FLAG_SYMLINK))
        );
        assert_eq!(entry(client, 2), Err(PhuxClientResult::InvalidArgument));
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn a_refusal_is_typed_and_names_the_path_the_server_tried() {
        let client = client(true);
        assert_eq!(list(client, 1, "/root"), PhuxClientResult::Ok);
        let refusal = FrameKind::DirectoryListing {
            request_id: 1,
            result: Err(DirectoryListingError {
                path: "/root".to_owned(),
                code: DirectoryErrorCode::PermissionDenied,
                message: "permission denied".to_owned(),
            }),
        };
        assert_eq!(feed(client, &refusal), PhuxClientResult::Ok);
        let refused = info(client);
        assert_eq!(refused.status, STATUS_REFUSED);
        assert_eq!(refused.error_code, 1);
        assert_eq!(refused.entry_count, 0);
        assert!(!refused.has_parent);
        assert_eq!(text(refused.path), "/root");
        assert_eq!(text(refused.message), "permission denied");
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn a_reply_to_a_superseded_request_is_dropped_not_a_protocol_error() {
        let client = client(true);
        assert_eq!(list(client, 1, "/a"), PhuxClientResult::Ok);
        assert_eq!(list(client, 2, "/b"), PhuxClientResult::Ok);
        assert_eq!(
            feed(client, &listed(1, "/a", &[("old", false)], false)),
            PhuxClientResult::Ok
        );
        assert_eq!(info(client).status, STATUS_PENDING);
        assert_eq!(
            feed(client, &listed(2, "/b", &[("new", false)], false)),
            PhuxClientResult::Ok
        );
        assert_eq!(info(client).status, STATUS_LISTED);
        assert_eq!(entry(client, 0), Ok(("new".to_owned(), 0)));
        // A duplicate of the answered request changes nothing either.
        assert_eq!(
            feed(client, &listed(2, "/b", &[("dup", false)], false)),
            PhuxClientResult::Ok
        );
        assert_eq!(entry(client, 0), Ok(("new".to_owned(), 0)));
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn a_correlated_error_settles_the_listing_and_disconnect_makes_it_unknown() {
        let client = client(true);
        assert_eq!(list(client, 4, "/x"), PhuxClientResult::Ok);
        let error = FrameKind::Error {
            request_id: Some(4),
            code: phux_protocol::wire::frame::ErrorCode::NotAttached,
            message: "busy".to_owned(),
        };
        assert_eq!(feed(client, &error), PhuxClientResult::Ok);
        assert_eq!(info(client).status, STATUS_REFUSED);
        assert_eq!(info(client).error_code, 3);

        assert_eq!(list(client, 5, "/y"), PhuxClientResult::Ok);
        assert_eq!(
            unsafe { crate::phux_client_disconnect(client) },
            PhuxClientResult::Ok
        );
        assert_eq!(info(client).status, STATUS_UNKNOWN_OUTCOME);
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn requests_are_refused_before_anything_is_queued() {
        let unsupported = client(false);
        assert!(!info(unsupported).supported);
        assert_eq!(list(unsupported, 1, "/"), PhuxClientResult::InvalidState);
        unsafe { crate::phux_client_free(unsupported) };

        let client = client(true);
        assert_eq!(list(client, 1, "a\0b"), PhuxClientResult::InvalidArgument);
        assert_eq!(
            list(client, 1, &"a".repeat(MAX_DIRECTORY_PATH_BYTES + 1)),
            PhuxClientResult::InvalidArgument
        );
        let invalid = [0xffu8];
        assert_eq!(
            unsafe {
                phux_client_list_directory(
                    client,
                    1,
                    PhuxBytes {
                        data: invalid.as_ptr(),
                        len: 1,
                    },
                )
            },
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(list(client, 7, "/"), PhuxClientResult::Ok);
        assert_eq!(
            list(client, 7, "/"),
            PhuxClientResult::InvalidArgument,
            "IDs must increase"
        );
        assert_eq!(unsafe { (*client).inner.outgoing.len() }, 1);
        unsafe { (*client).inner.attached = false };
        assert_eq!(list(client, 8, "/"), PhuxClientResult::InvalidState);
        unsafe { crate::phux_client_free(client) };
    }

    fn list_on(
        client: *mut PhuxClient,
        request_id: u32,
        path: &str,
        host: &[u8],
    ) -> PhuxClientResult {
        let request = PhuxDirectoryRequest {
            size: mem::size_of::<PhuxDirectoryRequest>(),
            version: ABI_VERSION,
            request_id,
            path: PhuxBytes {
                data: path.as_ptr(),
                len: path.len(),
            },
            host: PhuxBytes {
                data: host.as_ptr(),
                len: host.len(),
            },
        };
        unsafe { phux_client_list_directory_on(client, &raw const request) }
    }

    fn host_supported(client: *mut PhuxClient) -> bool {
        let mut out = false;
        assert_eq!(
            unsafe { phux_client_directory_host_supported(client, &raw mut out) },
            PhuxClientResult::Ok
        );
        out
    }

    fn only_queued(client: *mut PhuxClient) -> FrameKind {
        let queued = unsafe { &(*client).inner.outgoing };
        assert_eq!(queued.len(), 1);
        FrameKind::decode(&queued[0])
            .expect("queued frame decodes")
            .0
    }

    #[test]
    fn a_satellite_host_is_carried_when_the_hub_advertises_it() {
        let client = client(true);
        unsafe { (*client).inner.list_directory_host = true };
        assert!(host_supported(client));
        assert_eq!(
            list_on(client, 2, "~/src", b"build-host"),
            PhuxClientResult::Ok
        );
        assert_eq!(
            only_queued(client),
            FrameKind::ListDirectory {
                request_id: 2,
                path: "~/src".to_owned(),
                host: Some(SatelliteHost::new("build-host")),
            }
        );
        let pending = info(client);
        assert_eq!((pending.status, pending.request_id), (STATUS_PENDING, 2));
        // The satellite's reply is read back like any other listing.
        let reply = listed(2, "/home/b/src", &[("phux", false)], false);
        assert_eq!(feed(client, &reply), PhuxClientResult::Ok);
        assert_eq!(info(client).status, STATUS_LISTED);
        assert_eq!(entry(client, 0), Ok(("phux".to_owned(), 0)));
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn a_satellite_host_without_the_bit_is_refused_and_sends_nothing() {
        let client = client(true);
        assert!(!host_supported(client));
        assert_eq!(
            list_on(client, 1, "/", b"build-host"),
            PhuxClientResult::InvalidState
        );
        assert_eq!(unsafe { (*client).inner.outgoing.len() }, 0);
        assert_eq!(info(client).status, STATUS_NONE);
        // The refusal consumed no request ID, and an empty host is still the
        // serving host's own listing, byte-identical to the older call.
        assert_eq!(list_on(client, 1, "/", b""), PhuxClientResult::Ok);
        assert_eq!(
            only_queued(client),
            FrameKind::ListDirectory {
                request_id: 1,
                path: "/".to_owned(),
                host: None,
            }
        );
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn a_malformed_host_or_request_is_refused_before_anything_is_queued() {
        let client = client(true);
        unsafe { (*client).inner.list_directory_host = true };
        for host in [
            b"a\0b".as_slice(),
            &[0xff],
            "h".repeat(MAX_DIRECTORY_HOST_BYTES + 1).as_bytes(),
        ] {
            assert_eq!(
                list_on(client, 1, "/", host),
                PhuxClientResult::InvalidArgument
            );
        }
        assert_eq!(
            unsafe { phux_client_list_directory_on(client, std::ptr::null()) },
            PhuxClientResult::InvalidArgument
        );
        let stale = PhuxDirectoryRequest {
            size: mem::size_of::<PhuxDirectoryRequest>(),
            version: ABI_VERSION + 1,
            request_id: 1,
            path: PhuxBytes::default(),
            host: PhuxBytes::default(),
        };
        assert_eq!(
            unsafe { phux_client_list_directory_on(client, &raw const stale) },
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(unsafe { (*client).inner.outgoing.len() }, 0);
        let longest = "h".repeat(MAX_DIRECTORY_HOST_BYTES);
        assert_eq!(
            list_on(client, 1, "/", longest.as_bytes()),
            PhuxClientResult::Ok
        );
        unsafe { crate::phux_client_free(client) };
    }

    #[test]
    fn hello_ok_gates_the_host_on_its_own_bit() {
        use phux_protocol::ServerFeature::{ListDirectory, ListDirectoryHost};
        use phux_protocol::caps::ServerCapabilities;
        for (features, expected) in [
            (
                phux_protocol::ServerFeatureSet::with(&[ListDirectory]),
                false,
            ),
            (
                phux_protocol::ServerFeatureSet::with(&[ListDirectory, ListDirectoryHost]),
                true,
            ),
            // The host bit alone cannot list anything.
            (
                phux_protocol::ServerFeatureSet::with(&[ListDirectoryHost]),
                false,
            ),
        ] {
            let client = client(false);
            unsafe {
                (*client).inner.protocol_ready = false;
                (*client).inner.hello_queued = true;
            }
            let hello = FrameKind::HelloOk {
                protocol_major: crate::PROTOCOL_VERSION.major,
                protocol_minor: crate::PROTOCOL_VERSION.minor,
                protocol_patch: crate::PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new().with_features(features),
                server_id: b"server".to_vec(),
                selected_profile: phux_protocol::BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: phux_protocol::caps::BootstrapLimits::new(1024, 1024)
                    .expect("limits"),
            };
            assert_eq!(feed(client, &hello), PhuxClientResult::Ok);
            assert_eq!(host_supported(client), expected);
            unsafe { crate::phux_client_free(client) };
        }
    }

    #[test]
    fn hello_ok_gates_the_feature_on_the_advertised_bit() {
        use phux_protocol::caps::ServerCapabilities;
        for (features, expected) in [
            (phux_protocol::ServerFeatureSet::with(&[]), false),
            (
                phux_protocol::ServerFeatureSet::with(&[
                    phux_protocol::ServerFeature::ListDirectory,
                ]),
                true,
            ),
        ] {
            let client = client(false);
            unsafe {
                (*client).inner.protocol_ready = false;
                (*client).inner.hello_queued = true;
            }
            let hello = FrameKind::HelloOk {
                protocol_major: crate::PROTOCOL_VERSION.major,
                protocol_minor: crate::PROTOCOL_VERSION.minor,
                protocol_patch: crate::PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new().with_features(features),
                server_id: b"server".to_vec(),
                selected_profile: phux_protocol::BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: phux_protocol::caps::BootstrapLimits::new(1024, 1024)
                    .expect("limits"),
            };
            assert_eq!(feed(client, &hello), PhuxClientResult::Ok);
            assert_eq!(unsafe { (*client).inner.list_directory }, expected);
            unsafe { crate::phux_client_free(client) };
        }
    }
}
