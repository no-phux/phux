//! Resource kinds: the additive spawn, close, and append shapes.

use super::{
    AgentEvent, BytesMut, CloseReason, Command, DecodeError, FrameKind, GroupId, MAX_APPEND_BYTES,
    MAX_RESOURCE_NATIVE_ID_BYTES, MAX_RESOURCE_PROVIDER_BYTES, ResourceId, ResourceKind,
    SpawnResource, assert_round_trip, framed_tlv, local_id_bytes, tlv_field,
};

/// Build a `SPAWN_RESOURCE` body by hand from `(field_id, value)` pairs.
pub(super) fn spawn_terminal_frame(fields: &[(u32, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    tlv_field(&mut body, 1, &1u32.to_be_bytes()); // request_id
    tlv_field(&mut body, 2, &1u32.to_be_bytes()); // group
    for (id, value) in fields {
        tlv_field(&mut body, *id, value);
    }
    framed_tlv(0x22, &body)
}

#[test]
fn spawn_terminal_without_kind_field_decodes_as_terminal() {
    // A pre-kind body (fields 1-10 only) is a Terminal spawn with every
    // resource field at its default. This is the byte image every existing
    // spawn golden pins.
    let bytes = spawn_terminal_frame(&[]);
    let (decoded, _) = FrameKind::decode(&bytes).unwrap();
    let FrameKind::SpawnResource { resource, .. } = decoded else {
        panic!("expected SpawnResource");
    };
    assert_eq!(resource, None);
    // Spelling the defaults out is the same spawn: it encodes to the same
    // bytes and decodes back to `None`.
    let spelled = FrameKind::SpawnResource {
        request_id: 1,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: Some(Box::new(SpawnResource::default())),
    };
    let mut buf = BytesMut::new();
    spelled.encode(&mut buf);
    assert_eq!(buf.as_ref(), bytes.as_slice());
    let (decoded, _) = FrameKind::decode(&buf).unwrap();
    assert!(matches!(
        decoded,
        FrameKind::SpawnResource { resource: None, .. }
    ));
}

#[test]
fn spawn_terminal_explicit_terminal_kind_is_accepted_and_reencodes_absent() {
    // `kind = 0` written explicitly is legal on the wire and decodes to the
    // same value as an absent field; the encoder then omits it, so the
    // canonical Terminal image stays the pre-kind one.
    let bytes = spawn_terminal_frame(&[(11, &[0u8])]);
    let (decoded, _) = FrameKind::decode(&bytes).unwrap();
    let mut buf = BytesMut::new();
    decoded.encode(&mut buf);
    assert_eq!(buf.as_ref(), spawn_terminal_frame(&[]).as_slice());
}

#[test]
fn spawn_terminal_kind_rules_are_enforced_per_kind() {
    let parent = local_id_bytes(42);
    // Terminal-kind spawn may not carry the child / facet fields.
    for (field, value) in [
        (12u32, parent.as_slice()),
        (13, b"claude".as_slice()),
        (14, b"session-42".as_slice()),
    ] {
        let bytes = spawn_terminal_frame(&[(field, value)]);
        assert_eq!(
            FrameKind::decode(&bytes).unwrap_err(),
            DecodeError::InvalidSpawnForKind {
                kind: 0,
                field,
                required: false,
            },
            "Terminal kind must refuse field {field}"
        );
    }
    // AgentSession requires parent (12) and provider (13).
    let bytes = spawn_terminal_frame(&[(11, &[1u8]), (13, b"claude")]);
    assert_eq!(
        FrameKind::decode(&bytes).unwrap_err(),
        DecodeError::InvalidSpawnForKind {
            kind: 1,
            field: 12,
            required: true,
        }
    );
    let bytes = spawn_terminal_frame(&[(11, &[1u8]), (12, &parent)]);
    assert_eq!(
        FrameKind::decode(&bytes).unwrap_err(),
        DecodeError::InvalidSpawnForKind {
            kind: 1,
            field: 13,
            required: true,
        }
    );
    // AgentSession forbids the PTY-shape fields 3..=6 and 10, and the
    // window placement field 8.
    let mut cmd = Vec::new();
    cmd.extend_from_slice(&1u32.to_be_bytes());
    cmd.extend_from_slice(&3u32.to_be_bytes());
    cmd.extend_from_slice(b"zsh");
    let mut env = Vec::new();
    env.extend_from_slice(&0u32.to_be_bytes());
    let mut size = Vec::new();
    size.extend_from_slice(&80u16.to_be_bytes());
    size.extend_from_slice(&24u16.to_be_bytes());
    for (field, value) in [
        (3u32, cmd.as_slice()),
        (4, b"/tmp".as_slice()),
        (5, env.as_slice()),
        (6, b"xterm".as_slice()),
        (8, parent.as_slice()),
        (10, size.as_slice()),
    ] {
        let bytes =
            spawn_terminal_frame(&[(11, &[1u8]), (12, &parent), (13, b"claude"), (field, value)]);
        assert_eq!(
            FrameKind::decode(&bytes).unwrap_err(),
            DecodeError::InvalidSpawnForKind {
                kind: 1,
                field,
                required: false,
            },
            "AgentSession kind must refuse field {field}"
        );
    }
    // The minimal legal AgentSession spawn decodes.
    let bytes = spawn_terminal_frame(&[(11, &[1u8]), (12, &parent), (13, b"claude")]);
    let (decoded, _) = FrameKind::decode(&bytes).unwrap();
    let FrameKind::SpawnResource { resource, .. } = decoded else {
        panic!("expected SpawnResource");
    };
    assert_eq!(
        resource.as_deref(),
        Some(&SpawnResource::agent_session(
            ResourceId::local(42),
            "claude"
        ))
    );
    // An unknown kind is passed through with whatever fields it carries.
    let bytes = spawn_terminal_frame(&[(11, &[9u8]), (13, b"whatever")]);
    let (decoded, _) = FrameKind::decode(&bytes).unwrap();
    let FrameKind::SpawnResource { resource, .. } = decoded else {
        panic!("expected SpawnResource");
    };
    assert_eq!(
        resource.as_deref().map(|r| r.kind),
        Some(ResourceKind::Unknown { tag: 9 })
    );
}

#[test]
fn event_pane_spawned_empty_body_decodes_as_root_terminal() {
    // The pre-kind body is empty; it decodes as a root Terminal and a root
    // Terminal re-encodes to the same empty body.
    let mut event = vec![0x04u8]; // EVENT_TAG_RESOURCE_SPAWNED
    event.extend_from_slice(&0u32.to_be_bytes()); // empty body
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_id_bytes(0x2A));
    tlv_field(&mut fields, 2, &event);
    let bytes = framed_tlv(0xB3, &fields);
    let (decoded, _) = FrameKind::decode(&bytes).unwrap();
    let expected = FrameKind::Event {
        terminal: Some(ResourceId::local(0x2A)),
        event: AgentEvent::ResourceSpawned {
            kind: ResourceKind::Terminal,
            parent: None,
        },
        stamp: None,
    };
    assert_eq!(decoded, expected);
    let mut buf = BytesMut::new();
    expected.encode(&mut buf);
    assert_eq!(buf.as_ref(), bytes.as_slice());
    // An unknown field id inside the body is skipped by length.
    let mut body = Vec::new();
    tlv_field(&mut body, 1, &[1u8]);
    tlv_field(&mut body, 200, b"future");
    tlv_field(&mut body, 2, &local_id_bytes(7));
    let mut event = vec![0x04u8];
    event.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
    event.extend_from_slice(&body);
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_id_bytes(0x2B));
    tlv_field(&mut fields, 2, &event);
    let (decoded, _) = FrameKind::decode(&framed_tlv(0xB3, &fields)).unwrap();
    assert_eq!(
        decoded,
        FrameKind::Event {
            terminal: Some(ResourceId::local(0x2B)),
            event: AgentEvent::ResourceSpawned {
                kind: ResourceKind::AgentSession,
                parent: Some(ResourceId::local(7)),
            },
            stamp: None,
        }
    );
}

/// The server wraps `FrameKind` in broadcast enums whose other variants are
/// empty, under clippy's 200-byte `large_enum_variant` threshold. The
/// Terminal-era frame enum was 192 bytes; additive resource fields must not
/// push it past that, which is why the spawn's fields 11-14 ride behind one
/// `Option<Box<SpawnResource>>` and `SatelliteHost` is a boxed str.
#[test]
fn frame_kind_stays_within_its_size_budget() {
    assert!(
        std::mem::size_of::<FrameKind>() <= 192,
        "FrameKind is {} bytes; keep it at or under 192",
        std::mem::size_of::<FrameKind>()
    );
    assert_eq!(std::mem::size_of::<ResourceId>(), 24);
}

#[test]
fn spawn_terminal_agent_facet_strings_are_bounded() {
    let parent = local_id_bytes(42);
    let base: [(u32, &[u8]); 2] = [(11, &[1u8]), (12, &parent)];
    for (field, len) in [
        (13u32, 0usize),
        (13, MAX_RESOURCE_PROVIDER_BYTES + 1),
        (14, 0),
        (14, MAX_RESOURCE_NATIVE_ID_BYTES + 1),
    ] {
        let value = vec![b'a'; len];
        let mut fields: Vec<(u32, &[u8])> = base.to_vec();
        if field == 14 {
            fields.push((13, b"claude"));
        }
        fields.push((field, &value));
        let bytes = spawn_terminal_frame(&fields);
        assert_eq!(
            FrameKind::decode(&bytes).unwrap_err(),
            DecodeError::AgentFacetLimitExceeded,
            "field {field} with {len} bytes must be refused"
        );
    }
    let provider = vec![b'p'; MAX_RESOURCE_PROVIDER_BYTES];
    let native_id = vec![b'n'; MAX_RESOURCE_NATIVE_ID_BYTES];
    let bytes = spawn_terminal_frame(&[
        (11, &[1u8]),
        (12, &parent),
        (13, &provider),
        (14, &native_id),
    ]);
    assert!(FrameKind::decode(&bytes).is_ok());
}

#[test]
fn terminal_closed_reason_is_additive_and_forgiving() {
    // No field 3: the pre-reason body decodes as Unknown.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_id_bytes(42));
    let bytes = framed_tlv(0xA1, &fields);
    let (decoded, _) = FrameKind::decode(&bytes).unwrap();
    assert_eq!(
        decoded,
        FrameKind::ResourceClosed {
            terminal_id: ResourceId::local(42),
            exit_status: None,
            reason: CloseReason::Unknown,
            signal: None,
        }
    );
    // A stated reason decodes to its variant.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_id_bytes(42));
    tlv_field(&mut fields, 3, &[2u8]);
    let (decoded, _) = FrameKind::decode(&framed_tlv(0xA1, &fields)).unwrap();
    assert!(matches!(
        decoded,
        FrameKind::ResourceClosed {
            reason: CloseReason::ParentClosed,
            ..
        }
    ));
    // A reason tag from the future is Unknown, never a decode error, and
    // the frame re-encodes with the field absent.
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_id_bytes(42));
    tlv_field(&mut fields, 3, &[200u8]);
    let (decoded, _) = FrameKind::decode(&framed_tlv(0xA1, &fields)).unwrap();
    assert!(matches!(
        decoded,
        FrameKind::ResourceClosed {
            reason: CloseReason::Unknown,
            ..
        }
    ));
    let mut buf = BytesMut::new();
    decoded.encode(&mut buf);
    assert_eq!(buf.as_ref(), bytes.as_slice());
}

#[test]
fn command_append_resource_output_round_trips_and_is_bounded() {
    assert_round_trip(&FrameKind::Command {
        request_id: 5,
        command: Command::AppendResourceOutput {
            terminal_id: ResourceId::local(0x2B),
            bytes: b"{\"type\":\"stop\",\"data\":{}}\n".to_vec(),
        },
    });
    assert_round_trip(&FrameKind::Command {
        request_id: 6,
        command: Command::AppendResourceOutput {
            terminal_id: ResourceId::satellite("devbox", 0x2B),
            bytes: vec![b'x'; MAX_APPEND_BYTES],
        },
    });
    // Hand-rolled: tag 0x1a, local id, u32-prefixed bytes. Empty and
    // over-limit payloads are refused.
    for len in [0usize, MAX_APPEND_BYTES + 1] {
        let mut command = vec![0x1au8];
        command.extend_from_slice(&local_id_bytes(0x2B));
        command.extend_from_slice(&u32::try_from(len).unwrap().to_be_bytes());
        command.extend(std::iter::repeat_n(b'x', len));
        let mut fields = Vec::new();
        tlv_field(&mut fields, 1, &5u32.to_be_bytes());
        tlv_field(&mut fields, 2, &command);
        assert_eq!(
            FrameKind::decode(&framed_tlv(0x31, &fields)).unwrap_err(),
            DecodeError::AppendResourceOutputLimitExceeded,
            "{len}-byte append must be refused"
        );
    }
}
