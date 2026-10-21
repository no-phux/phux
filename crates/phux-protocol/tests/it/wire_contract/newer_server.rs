//! What a 1.0 decoder makes of a newer 1.x server's additions.
//!
//! A state-bearing enum in a server frame decodes an `Unknown` variant, a
//! correlated reply it cannot read settles its request, and anything else a
//! newer peer may send fails with an error [`DecodeError::is_unsupported`]
//! classifies, so a client skips that one frame instead of dropping the
//! connection. Malformed bytes stay malformed.

#![allow(clippy::unwrap_used, clippy::panic, reason = "tests")]

use phux_protocol::ids::{
    ArrangementKey, BootstrapId, ResourceId, Revision, SatelliteHost, SessionKey, StreamId,
};
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::UnreadableResult;
use phux_protocol::wire::frame::{
    ArrangementDoc, ArrangementGroup, ArrangementNode, ArrangementOwner, ArrangementValue,
    CommandResult, CommandValue, EntryRef, EntryTarget, ErrorCode, FrameKind, MoveError,
    MoveResult, SpawnError, SpawnResult, SplitDirection, TYPE_BOOTSTRAP_TOMBSTONE,
    TYPE_DIRECTORY_LISTING, TYPE_RESOURCE_MOVED, TYPE_RESOURCE_SPAWNED, TombstoneReason,
};

use crate::common::{assert_round_trip, decode, encode, framed_tlv, local_id_bytes, tlv_field};

/// A `request_id` (field 1) and `result` (field 2) reply body.
fn reply(type_byte: u8, result: &[u8]) -> Vec<u8> {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &7_u32.to_be_bytes());
    tlv_field(&mut fields, 2, result);
    framed_tlv(type_byte, &fields)
}

/// Results a 1.0 build cannot read: an unknown error tag with a payload, an
/// unknown result tag, and a known `Ok` tag around an unknown id tag.
fn unreadable_results() -> [Vec<u8>; 3] {
    [
        vec![0x01, 0xFE, 1, 2],
        vec![0xFD, 9, 9],
        vec![0x00, 0x7E, 0, 0, 0, 1],
    ]
}

#[test]
fn an_unreadable_spawn_result_settles_the_spawn_verbatim() {
    for result in unreadable_results() {
        let framed = reply(TYPE_RESOURCE_SPAWNED, &result);
        let decoded = decode(&framed).unwrap();
        assert_eq!(
            decoded,
            FrameKind::ResourceSpawned {
                request_id: 7,
                result: SpawnResult::Err(SpawnError::Unknown(UnreadableResult::from_wire(
                    result.clone()
                ))),
            }
        );
        // A hub relaying it re-encodes the satellite's bytes exactly.
        assert_eq!(encode(&decoded), framed, "{result:02x?}");
    }
}

#[test]
fn an_unreadable_move_result_settles_the_move_verbatim() {
    for result in unreadable_results() {
        let framed = reply(TYPE_RESOURCE_MOVED, &result);
        let decoded = decode(&framed).unwrap();
        assert_eq!(
            decoded,
            FrameKind::ResourceMoved {
                request_id: 7,
                result: MoveResult::Err(MoveError::Unknown(UnreadableResult::from_wire(
                    result.clone()
                ))),
            }
        );
        assert_eq!(encode(&decoded), framed, "{result:02x?}");
    }
}

#[test]
fn an_unreadable_result_names_its_unknown_tag_and_malformed_bytes_still_fail() {
    assert_eq!(UnreadableResult::from_wire(vec![0x01, 0xFE, 1]).tag(), 0xFE);
    assert_eq!(UnreadableResult::from_wire(vec![0xFD]).tag(), 0xFD);
    // A known error whose string is truncated is malformed, not newer.
    assert!(decode(&reply(TYPE_RESOURCE_SPAWNED, &[0x01, 0x01, 0, 0, 0, 9])).is_err());
}

#[test]
fn an_unknown_tombstone_reason_still_retires_the_generation() {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_id_bytes(1));
    tlv_field(&mut fields, 2, &2_u64.to_be_bytes());
    tlv_field(&mut fields, 3, &3_u64.to_be_bytes());
    tlv_field(&mut fields, 4, &[0x7E]);
    tlv_field(&mut fields, 5, &9_u64.to_be_bytes());
    let tombstone = FrameKind::BootstrapTombstone {
        terminal_id: ResourceId::local(1),
        stream_id: StreamId::new(2).unwrap(),
        bootstrap_id: BootstrapId::new(3).unwrap(),
        reason: TombstoneReason::Unknown,
        last_valid_seq: 9,
    };
    assert_eq!(
        decode(&framed_tlv(TYPE_BOOTSTRAP_TOMBSTONE, &fields)).unwrap(),
        tombstone
    );
    assert_round_trip(&tombstone);
}

fn satellite_leaf(entry: u8) -> ArrangementNode {
    ArrangementNode::Leaf(EntryRef {
        entry: [entry; 16],
        target: EntryTarget::Satellite {
            host: SatelliteHost::new("zzzz"),
            id: 1,
        },
    })
}

/// An unknown entry-target tag deep inside an arrangement reply settles the
/// request as unreadable, never `Unsupported`: the server did answer.
#[test]
fn an_unknown_enum_inside_an_arrangement_reply_settles_the_request() {
    let value = ArrangementValue::new(
        ArrangementKey::new([0xA7; 16]).unwrap(),
        ArrangementOwner::Session(SessionKey::new([0x5E; 16]).unwrap()),
        Revision::new(1),
        ArrangementDoc {
            groups: vec![ArrangementGroup {
                id: [0x61; 16],
                name: None,
                tree: ArrangementNode::Split {
                    dir: SplitDirection::Horizontal,
                    ratio: 0.5,
                    first: Box::new(satellite_leaf(0xE1)),
                    second: Box::new(satellite_leaf(0xE2)),
                },
            }],
        },
    );
    let mut bytes = encode(&FrameKind::CommandResult {
        request_id: 7,
        result: CommandResult::OkWith(CommandValue::Arrangement(Box::new(value))),
    });
    // The satellite target tag precedes its host: `0x01 || u32 len || host`.
    let host = b"\x00\x00\x00\x04zzzz";
    let at = bytes
        .windows(host.len())
        .position(|window| window == host)
        .unwrap();
    assert_eq!(bytes[at - 1], 0x01, "satellite target tag");
    bytes[at - 1] = 0xEE;
    let FrameKind::CommandResult {
        request_id: 7,
        result: CommandResult::Error { code, message },
    } = decode(&bytes).unwrap()
    else {
        panic!("an unreadable reply still settles its request");
    };
    assert_eq!(code, ErrorCode::UNREADABLE_REPLY);
    assert!(message.contains("EntryTarget"), "{message}");
}

#[test]
fn newer_values_are_classified_and_malformed_bytes_are_not() {
    // An unknown frame type, and an unknown enum in an uncorrelated frame.
    let unknown_kind = framed_tlv(0x6E, &[]);
    let mut scope = Vec::new();
    tlv_field(&mut scope, 1, &[0xFE]);
    tlv_field(&mut scope, 2, b"key");
    let unknown_scope = framed_tlv(phux_protocol::wire::frame::TYPE_METADATA_CHANGED, &scope);
    for bytes in [unknown_kind, unknown_scope] {
        let error = FrameKind::decode(&bytes).unwrap_err();
        assert!(error.is_unsupported(), "{error:?}");
    }
    // A truncated body and an oversized length are malformed.
    let mut truncated = Vec::new();
    tlv_field(&mut truncated, 1, &[0x00, 0x00]);
    for bytes in [
        framed_tlv(TYPE_RESOURCE_SPAWNED, &truncated),
        vec![0xFF, 0xFF, 0xFF, 0xFF, 0x01],
    ] {
        let error = FrameKind::decode(&bytes).unwrap_err();
        assert!(!error.is_unsupported(), "{error:?}");
    }
    assert!(!DecodeError::UnexpectedEof.is_unsupported());
}

/// A correlated reply whose body carries a value no `Unknown` variant covers
/// (here a directory listing's `truncated` flag with a newer value) settles
/// its request as an unreadable `ERROR`; an uncorrelated one is skipped;
/// malformed bytes stay an error.
#[test]
fn the_newer_server_fallback_settles_replies_and_skips_the_rest() {
    use phux_protocol::wire::decode::newer_server_fallback;

    let mut listing = Vec::new();
    tlv_field(&mut listing, 1, &7_u32.to_be_bytes());
    tlv_field(&mut listing, 2, b"/srv");
    tlv_field(&mut listing, 5, &[2]);
    let unknown_id = framed_tlv(TYPE_DIRECTORY_LISTING, &listing);
    let error = FrameKind::decode(&unknown_id).unwrap_err();
    let Ok(Some(FrameKind::Error {
        request_id: Some(7),
        code,
        ..
    })) = newer_server_fallback(&unknown_id, error)
    else {
        panic!("a correlated reply settles its request");
    };
    assert_eq!(code, ErrorCode::UNREADABLE_REPLY);

    let unknown_kind = framed_tlv(0x6E, &[]);
    let error = FrameKind::decode(&unknown_kind).unwrap_err();
    assert_eq!(newer_server_fallback(&unknown_kind, error), Ok(None));

    let mut truncated = Vec::new();
    tlv_field(&mut truncated, 1, &[0x00, 0x00]);
    let malformed = framed_tlv(TYPE_RESOURCE_SPAWNED, &truncated);
    let error = FrameKind::decode(&malformed).unwrap_err();
    assert_eq!(
        newer_server_fallback(&malformed, error),
        Err(DecodeError::UnexpectedEof)
    );
}

/// `frame` with `extra` appended to the value of its top-level field `id`.
fn grow_field(frame: &[u8], id: u32, extra: &[u8]) -> Vec<u8> {
    let mut dec = phux_protocol::wire::decode::Decoder::new(&frame[5..]);
    let mut fields = Vec::new();
    while let Some((field_id, value)) = dec.read_field().unwrap() {
        let mut value = value.to_vec();
        if field_id == id {
            value.extend_from_slice(extra);
        }
        tlv_field(&mut fields, field_id, &value);
    }
    framed_tlv(frame[4], &fields)
}

/// Server-to-client values that end their field grow at their end: a 1.0
/// client skips what a newer server appends after an actor's principal pair
/// or after a snapshot's extension block.
#[test]
fn a_newer_servers_tail_after_an_actor_or_a_snapshot_is_skipped() {
    use phux_protocol::ids::{ClientId, PrincipalId, SessionId, WindowId};
    use phux_protocol::wire::field;
    use phux_protocol::wire::frame::{ActorRef, AgentEvent, EventStamp};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

    let event = FrameKind::Event {
        terminal: None,
        event: AgentEvent::Bell,
        stamp: Some(Box::new(EventStamp::new(4, 9).with_actor(Some(
            ActorRef::new(ClientId::new(3)).with_principal(PrincipalId::new([0x91; 16])),
        )))),
    };
    let grown = grow_field(&encode(&event), field::event::ACTOR, &[0xEE, 0xEE]);
    assert_eq!(decode(&grown).unwrap(), event);

    let snapshot = SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
        .with_sessions(vec![SessionInfo::new(SessionId::new(1), "main")])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "w",
        )])
        .with_resources(vec![ResourceInfo::new(
            ResourceId::local(1),
            WindowId::new(1),
            80,
            24,
        )])
        .with_journal_head(Some(12));
    let attached = FrameKind::Attached {
        attach_id: 1,
        snapshot,
        initial_client_id: ClientId::new(2),
    };
    let grown = grow_field(&encode(&attached), field::attached::SNAPSHOT, &[0xEE; 6]);
    assert_eq!(decode(&grown).unwrap(), attached);
}

/// A `CHECKOUT` answer body: `OK_WITH`, the checkout value tag, then the
/// length-prefixed JSON.
fn checkout_reply(json: &[u8]) -> Vec<u8> {
    let mut result = vec![0x01, 0x0b];
    result.extend_from_slice(&u32::try_from(json.len()).unwrap().to_be_bytes());
    result.extend_from_slice(json);
    reply(phux_protocol::wire::frame::TYPE_COMMAND_RESULT, &result)
}

/// A newer server's checkout answer kind settles the request as unreadable
/// instead of closing the connection as malformed; a newer field in a known
/// answer is skipped.
#[test]
fn a_newer_checkout_answer_settles_and_new_fields_are_skipped() {
    use phux_protocol::checkout::{CheckoutAnswer, CheckoutStatus};
    use phux_protocol::wire::decode::newer_server_fallback;
    use phux_protocol::wire::frame::{CommandResult, CommandValue};

    let future = checkout_reply(br#"{"kind":"worktrees","items":[]}"#);
    let error = FrameKind::decode(&future).unwrap_err();
    assert!(error.is_unsupported(), "{error:?}");
    let Ok(Some(FrameKind::Error {
        request_id: Some(7),
        code,
        ..
    })) = newer_server_fallback(&future, error)
    else {
        panic!("the checkout request settles");
    };
    assert_eq!(code, ErrorCode::UNREADABLE_REPLY);

    let mut status =
        serde_json::to_value(CheckoutAnswer::Status(CheckoutStatus::default())).unwrap();
    status["stash_count"] = serde_json::json!(3);
    let grown = checkout_reply(&serde_json::to_vec(&status).unwrap());
    let FrameKind::CommandResult {
        result: CommandResult::OkWith(CommandValue::Checkout(answer)),
        ..
    } = decode(&grown).unwrap()
    else {
        panic!("a known answer with a newer field decodes");
    };
    assert_eq!(*answer, CheckoutAnswer::Status(CheckoutStatus::default()));

    let malformed = checkout_reply(br#"{"kind":"status","branch":7}"#);
    let error = FrameKind::decode(&malformed).unwrap_err();
    assert!(!error.is_unsupported(), "{error:?}");
}
