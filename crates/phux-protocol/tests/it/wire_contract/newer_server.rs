//! What a 1.0 decoder makes of a newer 1.x server's additions.
//!
//! A state-bearing enum in a server frame decodes an `Unknown` variant, a
//! correlated reply it cannot read settles its request, and anything else a
//! newer peer may send fails with an error [`DecodeError::is_unknown_value`]
//! classifies, so a client skips that one frame instead of dropping the
//! connection. Malformed bytes stay malformed.

#![allow(clippy::unwrap_used, clippy::panic, reason = "tests")]

use phux_protocol::ids::{
    ArrangementKey, BootstrapId, ResourceId, Revision, SatelliteHost, SessionKey, StreamId,
};
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    ArrangementDoc, ArrangementGroup, ArrangementNode, ArrangementOwner, ArrangementValue,
    CommandResult, CommandValue, EntryRef, EntryTarget, ErrorCode, FrameKind, MoveError,
    MoveResult, SpawnError, SpawnResult, SplitDirection, TYPE_BOOTSTRAP_TOMBSTONE,
    TYPE_RESOURCE_MOVED, TYPE_RESOURCE_SPAWNED, TombstoneReason,
};

use crate::common::{assert_round_trip, decode, encode, framed_tlv, local_id_bytes, tlv_field};

/// A `request_id` (field 1) and `result` (field 2) reply body.
fn reply(type_byte: u8, result: &[u8]) -> Vec<u8> {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &7_u32.to_be_bytes());
    tlv_field(&mut fields, 2, result);
    framed_tlv(type_byte, &fields)
}

#[test]
fn an_unknown_spawn_refusal_or_result_tag_settles_the_spawn() {
    for (result, tag) in [(vec![0x01, 0xFE, 1, 2], 0xFE), (vec![0xFD], 0xFD)] {
        assert_eq!(
            decode(&reply(TYPE_RESOURCE_SPAWNED, &result)).unwrap(),
            FrameKind::ResourceSpawned {
                request_id: 7,
                result: SpawnResult::Err(SpawnError::Unknown(tag)),
            }
        );
    }
}

#[test]
fn an_unknown_move_refusal_or_result_tag_settles_the_move() {
    for (result, tag) in [(vec![0x01, 0xFE, 1, 2], 0xFE), (vec![0xFD], 0xFD)] {
        assert_eq!(
            decode(&reply(TYPE_RESOURCE_MOVED, &result)).unwrap(),
            FrameKind::ResourceMoved {
                request_id: 7,
                result: MoveResult::Err(MoveError::Unknown(tag)),
            }
        );
    }
}

#[test]
fn unknown_spawn_and_move_refusals_round_trip() {
    assert_round_trip(&FrameKind::ResourceSpawned {
        request_id: 3,
        result: SpawnResult::Err(SpawnError::Unknown(0x70)),
    });
    assert_round_trip(&FrameKind::ResourceMoved {
        request_id: 3,
        result: MoveResult::Err(MoveError::Unknown(0x70)),
    });
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
        assert!(error.is_unknown_value(), "{error:?}");
    }
    // A truncated body and an oversized length are malformed.
    let mut truncated = Vec::new();
    tlv_field(&mut truncated, 1, &[0x00, 0x00]);
    for bytes in [
        framed_tlv(TYPE_RESOURCE_SPAWNED, &truncated),
        vec![0xFF, 0xFF, 0xFF, 0xFF, 0x01],
    ] {
        let error = FrameKind::decode(&bytes).unwrap_err();
        assert!(!error.is_unknown_value(), "{error:?}");
    }
    assert!(!DecodeError::UnexpectedEof.is_unknown_value());
}
