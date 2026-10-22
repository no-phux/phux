//! Directory listing request, reply, and host-routing tests.

use super::*;
use crate::c::test_support::attached_client;
use crate::c::types::ABI_VERSION;
use phux_protocol::ServerFeature;
use phux_protocol::wire::frame::{DirectoryEntry, DirectoryListing, DirectoryListingError};

const LISTING: &[ServerFeature] = &[ServerFeature::ListDirectory];
const HOST_LISTING: &[ServerFeature] = &[
    ServerFeature::ListDirectory,
    ServerFeature::ListDirectoryHost,
];

fn feed(client: *mut PhuxClient, frame: &FrameKind) -> PhuxClientResult {
    crate::c::test_support::feed(client, frame)
}

fn list(client: *mut PhuxClient, request_id: u32, path: &str) -> PhuxClientResult {
    list_on(client, request_id, path, b"")
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
    let client = attached_client(LISTING);
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
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn a_refusal_is_typed_and_names_the_path_the_server_tried() {
    let client = attached_client(LISTING);
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
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn a_reply_to_a_superseded_request_is_dropped_not_a_protocol_error() {
    let client = attached_client(LISTING);
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
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn a_correlated_error_settles_the_listing_and_disconnect_makes_it_unknown() {
    let client = attached_client(LISTING);
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
        unsafe { crate::c::phux_client_disconnect(client) },
        PhuxClientResult::Ok
    );
    assert_eq!(info(client).status, STATUS_UNKNOWN_OUTCOME);
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn requests_are_refused_before_anything_is_queued() {
    let unsupported = attached_client(&[]);
    assert!(!info(unsupported).supported);
    assert_eq!(list(unsupported, 1, "/"), PhuxClientResult::InvalidState);
    unsafe { crate::c::phux_client_free(unsupported) };

    let client = attached_client(LISTING);
    assert_eq!(list(client, 1, "a\0b"), PhuxClientResult::InvalidArgument);
    assert_eq!(
        list(client, 1, &"a".repeat(MAX_DIRECTORY_PATH_BYTES + 1)),
        PhuxClientResult::InvalidArgument
    );
    let invalid = [0xffu8];
    let invalid_request = PhuxDirectoryRequest {
        size: mem::size_of::<PhuxDirectoryRequest>(),
        version: ABI_VERSION,
        request_id: 1,
        path: PhuxBytes {
            data: invalid.as_ptr(),
            len: 1,
        },
        host: PhuxBytes::default(),
    };
    assert_eq!(
        unsafe { phux_client_list_directory_on(client, &raw const invalid_request) },
        PhuxClientResult::InvalidArgument
    );
    assert_eq!(list(client, 7, "/"), PhuxClientResult::Ok);
    assert_eq!(
        list(client, 7, "/"),
        PhuxClientResult::InvalidArgument,
        "IDs must increase"
    );
    assert_eq!(unsafe { (*client).inner.outgoing.len() }, 1);
    unsafe { crate::c::phux_client_free(client) };

    let unattached = crate::c::test_support::negotiated_client(LISTING);
    assert_eq!(list(unattached, 8, "/"), PhuxClientResult::InvalidState);
    unsafe { crate::c::phux_client_free(unattached) };
}

fn list_on(client: *mut PhuxClient, request_id: u32, path: &str, host: &[u8]) -> PhuxClientResult {
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
    let client = attached_client(HOST_LISTING);
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
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn a_satellite_host_without_the_bit_is_refused_and_sends_nothing() {
    let client = attached_client(LISTING);
    assert!(!host_supported(client));
    assert_eq!(
        list_on(client, 1, "/", b"build-host"),
        PhuxClientResult::InvalidState
    );
    assert_eq!(unsafe { (*client).inner.outgoing.len() }, 0);
    assert_eq!(info(client).status, STATUS_NONE);
    // The refusal consumed no request ID, and an empty host is still the
    // serving host's own listing.
    assert_eq!(list_on(client, 1, "/", b""), PhuxClientResult::Ok);
    assert_eq!(
        only_queued(client),
        FrameKind::ListDirectory {
            request_id: 1,
            path: "/".to_owned(),
            host: None,
        }
    );
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn a_malformed_host_or_request_is_refused_before_anything_is_queued() {
    let client = attached_client(HOST_LISTING);
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
    unsafe { crate::c::phux_client_free(client) };
}

#[test]
fn hello_ok_gates_listing_and_the_host_on_their_own_bits() {
    use ServerFeature::{ListDirectory, ListDirectoryHost};
    for (features, listing, host) in [
        (&[][..], false, false),
        (&[ListDirectory][..], true, false),
        (&[ListDirectory, ListDirectoryHost][..], true, true),
        // The host bit alone cannot list anything.
        (&[ListDirectoryHost][..], false, false),
    ] {
        let client = crate::c::test_support::negotiated_client(features);
        assert_eq!(unsafe { (*client).inner.list_directory }, listing);
        assert_eq!(host_supported(client), host);
        unsafe { crate::c::phux_client_free(client) };
    }
}
