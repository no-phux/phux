//! Additive wire shapes of `0.9.0-draft.15`: the journaled `EVENT`
//! envelope and `SUBSCRIBE_EVENTS.after_seq` (ADR-0123), retain-on-exit and
//! `RESOURCE_CLOSED.signal` (ADR-0124), and idempotent create (ADR-0126).
//!
//! Every test pins two things: the new field round-trips, and a frame that
//! does not use it keeps the bytes a `0.9.0-draft.14` encoder wrote. "What an
//! older decoder sees" is modelled by renumbering or dropping the new field,
//! since the current decoder skips an unknown field id by length exactly as
//! an older one does.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::ids::{
    ClientId, GroupId, IdempotencyKey, ResourceId, ResourceKind, ServerInstance, SessionId,
    WindowId,
};
use phux_protocol::wire::decode::Decoder;
use phux_protocol::wire::frame::{
    ActorRef, AgentEvent, CloseReason, CommandResult, CommandValue, ControlAction, EventStamp,
    FrameKind, ResourceLifecycle, Scope, SpawnError, SpawnResource, SpawnResult,
};
use phux_protocol::wire::info::{ExitFacet, ResourceInfo, SessionSnapshot};
use phux_protocol::wire::{DecodeError, frame::TYPE_EVENT};

use crate::common::{framed_tlv, tlv_field};

fn encode(frame: &FrameKind) -> Vec<u8> {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    buf.to_vec()
}

fn decode(bytes: &[u8]) -> Result<FrameKind, DecodeError> {
    let (frame, tail) = FrameKind::decode(bytes)?;
    assert!(tail.is_empty(), "decoder left {} bytes", tail.len());
    Ok(frame)
}

fn assert_round_trip(frame: &FrameKind) {
    assert_eq!(&decode(&encode(frame)).unwrap(), frame);
}

/// Split an encoded frame into its type byte and its top-level TLV fields.
fn fields(bytes: &[u8]) -> (u8, Vec<(u32, Vec<u8>)>) {
    let body = &bytes[4..];
    let mut dec = Decoder::new(&body[1..]);
    let mut out = Vec::new();
    while let Some((id, value)) = dec.read_field().unwrap() {
        out.push((id, value.to_vec()));
    }
    (body[0], out)
}

/// Re-frame `bytes` with each field id passed through `map`; `None` drops
/// the field. Dropping a new field yields what the older encoder wrote;
/// renumbering it to an unassigned id yields what an older decoder skips.
fn refield(bytes: &[u8], map: impl Fn(u32) -> Option<u32>) -> Vec<u8> {
    let (ty, fs) = fields(bytes);
    let mut body = Vec::new();
    for (id, value) in fs {
        if let Some(id) = map(id) {
            tlv_field(&mut body, id, &value);
        }
    }
    framed_tlv(ty, &body)
}

fn drop_ids(bytes: &[u8], ids: &[u32]) -> Vec<u8> {
    refield(bytes, |id| (!ids.contains(&id)).then_some(id))
}

const fn key(byte: u8) -> IdempotencyKey {
    IdempotencyKey::new([byte; 16]).unwrap()
}

fn actor() -> ActorRef {
    ActorRef::new(ClientId::new(7))
        .with_credential_id(Some("cred-1".to_owned()))
        .with_client_name(Some("phux-cli".to_owned()))
}

fn event(terminal: Option<ResourceId>, event: AgentEvent, stamp: Option<EventStamp>) -> FrameKind {
    FrameKind::Event {
        terminal,
        event,
        stamp: stamp.map(Box::new),
    }
}

fn spawn(resource: Option<SpawnResource>) -> FrameKind {
    FrameKind::SpawnResource {
        request_id: 9,
        group: GroupId::new(1),
        // No process shape, so the same helper serves both kinds.
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: resource.map(Box::new),
    }
}

#[test]
fn event_envelope_roundtrips_seq_ts_actor_and_operation_id() {
    let full = EventStamp::new(42, 1_700_000_000_123)
        .with_actor(Some(actor()))
        .with_operation_id(Some(key(0x5a)));
    let bare = EventStamp::new(1, 0).with_actor(Some(ActorRef::new(ClientId::new(0))));
    for stamp in [full, bare, EventStamp::new(u64::MAX - 1, u64::MAX)] {
        assert_round_trip(&event(
            Some(ResourceId::local(3)),
            AgentEvent::ResourceSpawned {
                kind: ResourceKind::Terminal,
                parent: None,
            },
            Some(stamp),
        ));
    }
    let bytes = encode(&event(None, AgentEvent::Bell, Some(EventStamp::new(5, 6))));
    let ids: Vec<u32> = fields(&bytes).1.into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, [2, 3, 4], "seq and ts_ms always travel together");
}

/// A stamp's fields 4-6 mean nothing without field 3 and are dropped.
#[test]
fn event_stamp_exists_only_with_its_sequence() {
    let stamped = encode(&event(
        None,
        AgentEvent::Bell,
        Some(EventStamp::new(5, 6).with_operation_id(Some(key(1)))),
    ));
    let without_seq = drop_ids(&stamped, &[3]);
    assert_eq!(
        decode(&without_seq).unwrap(),
        event(None, AgentEvent::Bell, None)
    );
}

#[test]
fn event_without_journal_fields_is_byte_identical_to_0_9_0() {
    // 0.9.0-draft.14 EVENT: field 1 = LOCAL(7), field 2 = bell (tag 0x03,
    // empty length-prefixed body).
    let mut golden = Vec::new();
    tlv_field(&mut golden, 1, &[0, 0, 0, 0, 7]);
    tlv_field(&mut golden, 2, &[0x03, 0, 0, 0, 0]);
    let golden = framed_tlv(TYPE_EVENT, &golden);
    let plain = event(Some(ResourceId::local(7)), AgentEvent::Bell, None);
    assert_eq!(encode(&plain), golden);

    let stamped = event(
        Some(ResourceId::local(7)),
        AgentEvent::Bell,
        Some(
            EventStamp::new(9, 10)
                .with_actor(Some(actor()))
                .with_operation_id(Some(key(2))),
        ),
    );
    let stamped = encode(&stamped);
    assert_eq!(drop_ids(&stamped, &[3, 4, 5, 6]), golden);
    // An older decoder skips fields it does not know by length.
    let older = refield(&stamped, |id| Some(if id > 2 { id + 100 } else { id }));
    assert_eq!(decode(&older).unwrap(), plain);
}

#[test]
fn subscribe_events_after_seq_is_skipped_by_an_older_decoder() {
    for terminal in [None, Some(ResourceId::local(4))] {
        let with = FrameKind::SubscribeEvents {
            terminal: terminal.clone(),
            after_seq: Some(41),
        };
        let without = FrameKind::SubscribeEvents {
            terminal,
            after_seq: None,
        };
        assert_round_trip(&with);
        assert_round_trip(&without);
        let bytes = encode(&with);
        assert_eq!(drop_ids(&bytes, &[2]), encode(&without));
        let older = refield(&bytes, |id| Some(if id == 2 { 99 } else { id }));
        assert_eq!(decode(&older).unwrap(), without);
    }
    // The live-only journal subscription is the maximum cursor.
    assert_round_trip(&FrameKind::SubscribeEvents {
        terminal: None,
        after_seq: Some(u64::MAX),
    });
}

#[test]
fn agent_event_journal_gap_and_source_gap_roundtrip_and_unknown_tag_still_skips() {
    assert_round_trip(&event(
        None,
        AgentEvent::JournalGap {
            first_missing: 12,
            last_missing: 4_107,
        },
        None,
    ));
    assert_round_trip(&event(
        Some(ResourceId::local(8)),
        AgentEvent::SourceGap { dropped: 3 },
        Some(EventStamp::new(4_200, 1)),
    ));
    let (_, fs) = fields(&encode(&event(
        None,
        AgentEvent::SourceGap { dropped: 3 },
        None,
    )));
    assert_eq!(fs[0].1[0], 0x0c, "source_gap is tag 0x0c");

    // 0x0d is the next unallocated tag: still skipped by its length.
    let agent_event = [0x0d, 0, 0, 0, 2, 0xaa, 0xbb];
    let mut body = Vec::new();
    tlv_field(&mut body, 2, &agent_event);
    assert_eq!(
        decode(&framed_tlv(TYPE_EVENT, &body)).unwrap(),
        event(
            None,
            AgentEvent::Unknown {
                tag: 0x0d,
                body: vec![0xaa, 0xbb],
            },
            None,
        )
    );
}

#[test]
fn terminal_control_expired_round_trips_and_an_unknown_action_is_opaque() {
    assert_round_trip(&event(
        Some(ResourceId::local(2)),
        AgentEvent::TerminalControl {
            lifecycle: ResourceLifecycle::Running,
            exit_status: None,
            input_holder: None,
            action: ControlAction::Expired,
            actor: None,
        },
        None,
    ));
    // lifecycle RUNNING, no exit, no holder, action 0x0a, no actor.
    let control = [0u8, 0, 0, 0x0a, 0];
    let mut agent_event = vec![0x08, 0, 0, 0, 5];
    agent_event.extend_from_slice(&control);
    let mut body = Vec::new();
    tlv_field(&mut body, 2, &agent_event);
    assert_eq!(
        decode(&framed_tlv(TYPE_EVENT, &body)).unwrap(),
        event(
            None,
            AgentEvent::Unknown {
                tag: 0x08,
                body: control.to_vec(),
            },
            None,
        )
    );
}

#[test]
fn resource_closed_signal_field_roundtrips_and_is_absent_by_default() {
    let closed = |exit_status, signal| FrameKind::ResourceClosed {
        terminal_id: ResourceId::local(5),
        exit_status,
        reason: CloseReason::Exited,
        signal,
    };
    assert_round_trip(&closed(None, Some(9)));
    assert_round_trip(&closed(None, Some(-1)));
    assert_round_trip(&closed(Some(0), None));
    let with = encode(&closed(None, Some(9)));
    let (_, fs) = fields(&with);
    assert_eq!(fs.last().unwrap(), &(4, 9u32.to_be_bytes().to_vec()));
    assert_eq!(drop_ids(&with, &[4]), encode(&closed(None, None)));
}

#[test]
fn spawn_resource_fields_16_17_roundtrip_and_older_encoding_is_unchanged() {
    for resource in [
        SpawnResource::default().with_retain_secs(Some(0)),
        SpawnResource::default().with_retain_secs(Some(600)),
        SpawnResource::default().with_idempotency_key(Some(key(3))),
        SpawnResource::default()
            .with_retain_secs(Some(30))
            .with_idempotency_key(Some(key(4)))
            .with_bind_instance(true),
        SpawnResource::agent_session(ResourceId::local(1), "claude")
            .with_idempotency_key(Some(key(5))),
    ] {
        assert_round_trip(&spawn(Some(resource)));
    }
    let keyed = encode(&spawn(Some(
        SpawnResource::default()
            .with_retain_secs(Some(30))
            .with_idempotency_key(Some(key(6))),
    )));
    assert_eq!(drop_ids(&keyed, &[16, 17]), encode(&spawn(None)));
    assert_eq!(
        decode(&refield(&keyed, |id| Some(id + u32::from(id >= 16) * 100))).unwrap(),
        spawn(None)
    );
}

#[test]
fn spawn_resource_fields_16_17_are_validated() {
    let agent = encode(&spawn(Some(
        SpawnResource::agent_session(ResourceId::local(1), "claude").with_retain_secs(Some(5)),
    )));
    assert!(matches!(
        decode(&agent),
        Err(DecodeError::InvalidSpawnForKind {
            field: 16,
            required: false,
            ..
        })
    ));
    let keyed = encode(&spawn(Some(
        SpawnResource::default().with_idempotency_key(Some(key(7))),
    )));
    for bad in [vec![0u8; 16], vec![7u8; 15], vec![7u8; 17]] {
        let (ty, fs) = fields(&keyed);
        let mut body = Vec::new();
        for (id, value) in fs {
            tlv_field(&mut body, id, if id == 17 { &bad } else { &value });
        }
        assert!(matches!(
            decode(&framed_tlv(ty, &body)),
            Err(DecodeError::InvalidIdempotencyKey)
        ));
    }
}

#[test]
fn resource_spawned_replayed_flag_only_beside_ok() {
    let instance = ServerInstance::new([0x11; 16]);
    let spawned = |result| FrameKind::ResourceSpawned {
        request_id: 3,
        result,
    };
    for result in [
        SpawnResult::Replayed {
            id: ResourceId::local(8),
            instance: None,
        },
        SpawnResult::Replayed {
            id: ResourceId::local(8),
            instance: Some(instance),
        },
        SpawnResult::Err(SpawnError::IdempotencyConflict),
    ] {
        assert_round_trip(&spawned(result));
    }
    let replayed = encode(&spawned(SpawnResult::Replayed {
        id: ResourceId::local(8),
        instance: None,
    }));
    assert_eq!(
        drop_ids(&replayed, &[4]),
        encode(&spawned(SpawnResult::Ok(ResourceId::local(8))))
    );
    // A replayed flag beside a refusal means nothing and is dropped.
    let refused = encode(&spawned(SpawnResult::Err(SpawnError::GroupNotFound)));
    let (ty, mut fs) = fields(&refused);
    fs.push((4, vec![1]));
    let mut body = Vec::new();
    for (id, value) in &fs {
        tlv_field(&mut body, *id, value);
    }
    assert_eq!(
        decode(&framed_tlv(ty, &body)).unwrap(),
        spawned(SpawnResult::Err(SpawnError::GroupNotFound))
    );
    // `0` is not replayed; any other value is malformed.
    let ok = encode(&spawned(SpawnResult::Ok(ResourceId::local(8))));
    for (flag, expect_ok) in [(0u8, true), (2, false)] {
        let (ty, mut fs) = fields(&ok);
        fs.push((4, vec![flag]));
        let mut body = Vec::new();
        for (id, value) in &fs {
            tlv_field(&mut body, *id, value);
        }
        assert_eq!(decode(&framed_tlv(ty, &body)).is_ok(), expect_ok);
    }
}

fn state(resources: Vec<ResourceInfo>) -> FrameKind {
    FrameKind::CommandResult {
        request_id: 1,
        result: CommandResult::OkWith(CommandValue::State(
            SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
                .with_resources(resources),
        )),
    }
}

const fn plain(id: u32) -> ResourceInfo {
    ResourceInfo::new(ResourceId::local(id), WindowId::new(1), 80, 24)
}

/// Bytes in the `COMMAND_RESULT` result field (id 2), which holds the
/// snapshot.
fn result_len(frame: &FrameKind) -> usize {
    let (_, fs) = fields(&encode(frame));
    fs.iter().find(|(id, _)| *id == 2).unwrap().1.len()
}

#[test]
fn resource_info_facet_row_carries_lifecycle_exit_and_input_holder_additively() {
    let exited = plain(1)
        .with_lifecycle(ResourceLifecycle::Exited)
        .with_exit(Some(
            ExitFacet::new(1_700_000_000_000, 1_700_000_600_000)
                .with_signal(Some(9))
                .with_reason(CloseReason::Exited),
        ));
    let held = plain(2).with_input_holder(Some(ClientId::new(4)));
    let child = ResourceInfo::resource(ResourceId::local(3), ResourceKind::AgentSession)
        .with_parent(Some(ResourceId::local(1)))
        .with_input_holder(Some(ClientId::new(5)));
    let frozen = plain(6).with_lifecycle(ResourceLifecycle::Frozen);
    let all = vec![exited.clone(), held.clone(), child, frozen, plain(7)];
    assert_round_trip(&state(all));
    assert_round_trip(&state(vec![
        plain(1).with_exit(Some(ExitFacet::new(1, 2).with_exit_status(Some(0)))),
    ]));

    // A plain entry adds only its positional prefix (id 5 + window 4 +
    // cols 2 + rows 2 + title 1 + cwd 1 bytes): no state entry is written.
    let one = state(vec![exited.clone(), held.clone()]);
    let two = state(vec![exited, held, plain(7)]);
    assert_eq!(result_len(&two) - result_len(&one), 15);

    // A snapshot of plain entries carries no trailing field at all.
    let bare = state(vec![plain(1), plain(2)]);
    assert_eq!(result_len(&bare) - result_len(&state(vec![plain(1)])), 15);
    let empty = state(Vec::new());
    assert_eq!(result_len(&state(vec![plain(1)])) - result_len(&empty), 15);
}

#[test]
fn metadata_changed_actor_roundtrips() {
    let changed = |actor| FrameKind::MetadataChanged {
        scope: Scope::Global,
        key: "phux.tui.layout/v1/1".to_owned(),
        value: Some(b"{}".to_vec()),
        actor,
    };
    assert_round_trip(&changed(Some(actor())));
    assert_round_trip(&changed(Some(ActorRef::new(ClientId::new(1)))));
    assert_round_trip(&changed(None));
    let with = encode(&changed(Some(actor())));
    assert_eq!(drop_ids(&with, &[4]), encode(&changed(None)));
    let older = refield(&with, |id| Some(if id == 4 { 50 } else { id }));
    assert_eq!(decode(&older).unwrap(), changed(None));
}
