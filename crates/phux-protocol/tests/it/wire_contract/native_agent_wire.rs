//! Dedicated native broker wire contract, including hostile JSON bounds.
#![allow(clippy::unwrap_used)]
use crate::common;

use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::*;
use phux_protocol::wire::error::DecodeError;
use phux_protocol::wire::frame::{AgentEvent, Command, FrameKind};

fn frame(request: NativeRequest) -> FrameKind {
    FrameKind::Command {
        request_id: 7,
        command: Command::NativeAgent {
            request: NativeRequestPayload::new(request).unwrap(),
        },
    }
}

// COMMAND envelope uses TLV field 1=request_id, field 2=command.
fn raw_json(json: &[u8]) -> Vec<u8> {
    let mut command = vec![0x38];
    command.extend_from_slice(&u32::try_from(json.len()).unwrap().to_be_bytes());
    command.extend_from_slice(json);
    let mut fields = Vec::new();
    common::tlv_field(&mut fields, 1, &7u32.to_be_bytes());
    common::tlv_field(&mut fields, 2, &command);
    common::framed_tlv(phux_protocol::wire::frame::TYPE_COMMAND, &fields)
}

#[test]
fn all_native_request_roles_round_trip() {
    let id = ResourceId::local(9);
    let activation = "activation".to_owned();
    let binding_proof = "proof".to_owned();
    let result = NativeCommandResult {
        operation_id: 3,
        disposition: NativeDisposition::Queued,
        detail: Some("SDK submission".into()),
    };
    let requests = vec![
        NativeRequest::Spawn {
            spec: SpawnNativeAgent {
                session: "work".into(),
                backend: "acp".into(),
                provider: "omp".into(),
                command: vec!["omp".into(), "--mode".into(), "rpc".into()],
                cwd: None,
                env: std::collections::BTreeMap::new(),

                native_id: Some("actual-provider-session".into()),
                guardian: None,
                guardian_key: Some(phux_protocol::ids::ResourceKey::new([4; 16]).unwrap()),
                guardian_expected: None,
            },
        },
        NativeRequest::Command {
            agent_session_id: id.clone(),
            command: NativeCommand {
                operation_id: 3,
                activation: activation.clone(),
                intent: NativeIntent::Answer {
                    question_id: "question".into(),
                    response: serde_json::json!({"confirmed": false}),
                },
            },
        },
        NativeRequest::Register {
            agent_session_id: id.clone(),
            activation: activation.clone(),
            binding_proof: binding_proof.clone(),
        },
        NativeRequest::Receive {
            agent_session_id: id.clone(),
            activation: activation.clone(),
            binding_proof: binding_proof.clone(),
            generation: 2,
        },
        NativeRequest::Complete {
            agent_session_id: id.clone(),
            activation: activation.clone(),
            binding_proof: binding_proof.clone(),
            generation: 2,
            result,
        },
        NativeRequest::Publish {
            agent_session_id: id.clone(),
            activation: activation.clone(),
            binding_proof: binding_proof.clone(),
            generation: 2,
            publication_id: 4,
            update: NativeUpdate::State {
                state: NativeState::Blocked,
                turn_id: Some("turn".into()),
            },
        },
        NativeRequest::Close {
            agent_session_id: id,
            activation,
            binding_proof,
            generation: 2,
        },
    ];
    for request in requests {
        let expected = frame(request.clone());
        common::assert_round_trip(&expected);
        let json = serde_json::to_vec(&request).unwrap();
        assert_eq!(common::decode(&raw_json(&json)).unwrap(), expected);
    }
}

#[test]
fn request_payload_bounds_the_encoded_envelope_not_unescaped_text() {
    let request = NativeRequest::Command {
        agent_session_id: ResourceId::local(9),
        command: NativeCommand {
            operation_id: 1,
            activation: "activation".into(),
            intent: NativeIntent::Prompt {
                text: "\0".repeat(MAX_NATIVE_PAYLOAD_BYTES / 3),
            },
        },
    };
    assert!(NativeRequestPayload::new(request).is_err());
}

#[test]
fn native_json_rejects_oversize_before_deserialization() {
    assert!(matches!(
        common::decode(&raw_json(&vec![b' '; MAX_NATIVE_PAYLOAD_BYTES + 1])),
        Err(DecodeError::NativeAgentLimitExceeded)
    ));
    assert!(matches!(
        common::decode(&raw_json(&vec![b' '; MAX_NATIVE_PAYLOAD_BYTES])),
        Err(DecodeError::InvalidNativeAgentPayload)
    ));
}

#[test]
fn native_json_rejects_malformed_and_wrong_dto() {
    for json in [b"{".as_slice(), b"{}", b"{\"type\":\"register\"}"] {
        assert!(matches!(
            common::decode(&raw_json(json)),
            Err(DecodeError::InvalidNativeAgentPayload)
        ));
    }
}

/// A request type or field this build does not know is newer, not
/// malformed: still refused, but as unsupported, so a server keeps the
/// connection.
#[test]
fn native_json_refuses_an_unknown_type_or_field_as_unsupported() {
    // `NativeCommand` denies unknown fields; add one to a real command.
    let mut command = serde_json::to_value(NativeRequest::Command {
        agent_session_id: ResourceId::local(9),
        command: NativeCommand {
            operation_id: 1,
            activation: "activation".into(),
            intent: NativeIntent::Prompt { text: "hi".into() },
        },
    })
    .unwrap();
    command["command"]["later"] = serde_json::Value::Bool(true);
    let unknown_field = serde_json::to_vec(&command).unwrap();
    for json in [br#"{"type":"not_a_request"}"#.as_slice(), &unknown_field] {
        let err = common::decode(&raw_json(json)).unwrap_err();
        assert_eq!(
            err,
            DecodeError::UnsupportedPayload {
                payload: "NativeRequest"
            },
            "{}",
            String::from_utf8_lossy(json)
        );
        assert!(err.is_unsupported());
    }
}

#[test]
fn native_outcomes_match_the_typed_wire_contract() {
    for (tag, disposition, detail) in [
        (0, NativeDisposition::Admitted, None),
        (1, NativeDisposition::Queued, Some("")),
        (2, NativeDisposition::Completed, Some("SDK completion")),
        (3, NativeDisposition::Refused, Some("Refused · 原因")),
        (4, NativeDisposition::Unknown, None),
    ] {
        let mut body = vec![0, 0, 0, 0, 9]; // local ResourceId @9
        body.extend_from_slice(&u64::MAX.to_be_bytes());
        body.push(tag);
        if let Some(detail) = detail {
            body.extend_from_slice(&u32::try_from(detail.len()).unwrap().to_be_bytes());
            body.extend_from_slice(detail.as_bytes());
        }
        let wire = FrameKind::Event {
            terminal: Some(ResourceId::local(8)),
            event: AgentEvent::Unknown { tag: 0x1c, body },
            stamp: None,
        };
        let expected = FrameKind::Event {
            terminal: Some(ResourceId::local(8)),
            event: AgentEvent::NativeAgentResult {
                agent_session_id: ResourceId::local(9),
                operation_id: u64::MAX,
                disposition,
                detail: detail.map(str::to_owned),
            },
            stamp: None,
        };
        let encoded = common::encode(&wire);
        assert_eq!(common::encode(&expected), encoded);
        assert_eq!(common::decode(&encoded).unwrap(), expected);
    }
}

#[test]
fn future_native_outcome_is_opaque_but_oversized_body_is_refused() {
    let mut body = vec![0, 0, 0, 0, 9];
    body.extend_from_slice(&u64::MAX.to_be_bytes());
    body.extend_from_slice(&[0xFE, 0xAA, 0xBB]);
    let future = FrameKind::Event {
        terminal: None,
        event: AgentEvent::Unknown { tag: 0x1c, body },
        stamp: None,
    };
    common::assert_round_trip(&future);
    let oversized = FrameKind::Event {
        terminal: None,
        event: AgentEvent::NativeAgentResult {
            agent_session_id: ResourceId::local(9),
            operation_id: 3,
            disposition: NativeDisposition::Unknown,
            detail: Some("x".repeat(MAX_NATIVE_PAYLOAD_BYTES)),
        },
        stamp: None,
    };
    assert!(matches!(
        common::decode(&common::encode(&oversized)),
        Err(DecodeError::NativeAgentLimitExceeded)
    ));
}
