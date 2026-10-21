//! A newer 1.x server's additions through `phux_client_feed_frame`: an
//! unknown frame type or enum value is skipped without a protocol error, and
//! a correlated reply this build cannot fully read still settles the request.

use super::*;
use phux_protocol::wire::frame::{ErrorCode, TYPE_METADATA_CHANGED, TYPE_RESOURCE_SPAWNED};

/// A frame of `type_byte` with the given `(field id, value)` TLV fields.
fn raw(type_byte: u8, fields: &[(u8, &[u8])]) -> Vec<u8> {
    let mut body = vec![type_byte];
    for (id, value) in fields {
        // Every id and length here is under 128: one varint byte each.
        body.extend([*id, 2, u8::try_from(value.len()).expect("short")]);
        body.extend_from_slice(value);
    }
    let mut frame = u32::try_from(body.len())
        .expect("short")
        .to_be_bytes()
        .to_vec();
    frame.extend(body);
    frame
}

fn feed_raw(client: *mut PhuxClient, bytes: &[u8]) -> PhuxClientResult {
    // SAFETY: the caller owns the live client; `bytes` outlives the call.
    unsafe { phux_client_feed_frame(client, bytes.as_ptr(), bytes.len()) }
}

fn queue_spawn(client: *mut PhuxClient, request_id: u32) {
    // SAFETY: the caller owns the live client; the options outlive the call.
    let queued = unsafe {
        phux_client_queue_spawn(
            client,
            &PhuxSpawnOptions {
                request_id,
                ..PhuxSpawnOptions::default()
            },
        )
    };
    assert_eq!(queued, PhuxClientResult::Ok);
}

fn operation_status(client: *mut PhuxClient, index: usize) -> u32 {
    let mut out = PhuxOperationResult::default();
    // SAFETY: output is writable and disjoint from the client.
    assert_eq!(
        unsafe { phux_client_operation_get(client, index, &raw mut out) },
        PhuxClientResult::Ok
    );
    out.status
}

#[test]
fn unknown_frame_kinds_and_enum_values_are_skipped() {
    let client = attached_client(&[]);
    assert_eq!(feed_raw(client, &raw(0x6E, &[])), PhuxClientResult::Ok);
    assert_eq!(
        feed_raw(
            client,
            &raw(TYPE_METADATA_CHANGED, &[(1, &[0xFE]), (2, b"key")])
        ),
        PhuxClientResult::Ok
    );
    // Malformed bytes are still a protocol error.
    assert_eq!(
        feed_raw(client, &raw(TYPE_RESOURCE_SPAWNED, &[(1, &[0, 0])])),
        PhuxClientResult::ProtocolError
    );
    // SAFETY: the fixture handed over ownership.
    unsafe { phux_client_free(client) };
}

#[test]
fn a_spawn_refused_with_an_unknown_tag_or_unsupported_settles() {
    let client = attached_client(&[]);
    queue_spawn(client, 10);
    assert_eq!(
        feed_raw(
            client,
            &raw(
                TYPE_RESOURCE_SPAWNED,
                &[(1, &10_u32.to_be_bytes()), (2, &[0x01, 0xFE])]
            )
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(operation_status(client, 0), 2, "a refused spawn");

    // A newer server that does not understand the spawn answers a
    // correlated ERROR UNSUPPORTED; the spawn settles, the session stays.
    queue_spawn(client, 11);
    assert_eq!(
        feed(
            client,
            &FrameKind::Error {
                request_id: Some(11),
                code: ErrorCode::Unsupported,
                message: "unknown spawn kind".into(),
            }
        ),
        PhuxClientResult::Ok
    );
    assert_eq!(operation_status(client, 1), 2, "an unsupported spawn");
    // SAFETY: the fixture handed over ownership.
    unsafe { phux_client_free(client) };
}
