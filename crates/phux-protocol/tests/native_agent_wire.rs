//! Dedicated native broker wire contract, including hostile JSON bounds.
#![allow(clippy::unwrap_used)]
mod common;

use phux_protocol::ids::ResourceId;
use phux_protocol::native_agent::*;
use phux_protocol::wire::error::DecodeError;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, FrameKind};

fn frame(request: NativeRequest) -> FrameKind {
    FrameKind::Command { request_id: 7, command: Command::NativeAgent { request: Box::new(request) } }
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
    let result = NativeCommandResult { operation_id: 3, disposition: NativeDisposition::Queued, detail: Some("SDK submission".into()) };
    let requests = vec![
        NativeRequest::Spawn { spec: SpawnNativeAgent { session: "work".into(), backend: "omp-rpc".into(), provider: "omp".into(), command: vec!["omp".into(), "--mode".into(), "rpc".into()], cwd: None, env: Default::default(), sdk_runtime: None, node_bridge: None, native_id: Some("actual-provider-session".into()), guardian: None, guardian_key: Some(phux_protocol::ids::ResourceKey::new([4; 16]).unwrap()), guardian_expected: None } },
        NativeRequest::Command { agent_session_id: id.clone(), command: NativeCommand { operation_id: 3, activation: activation.clone(), intent: NativeIntent::Answer { question_id: "question".into(), response: serde_json::json!({"confirmed": false}) } } },
        NativeRequest::Register { agent_session_id: id.clone(), activation: activation.clone(), binding_proof: binding_proof.clone() },
        NativeRequest::Receive { agent_session_id: id.clone(), activation: activation.clone(), binding_proof: binding_proof.clone(), generation: 2 },
        NativeRequest::Complete { agent_session_id: id.clone(), activation: activation.clone(), binding_proof: binding_proof.clone(), generation: 2, result },
        NativeRequest::Publish { agent_session_id: id.clone(), activation: activation.clone(), binding_proof: binding_proof.clone(), generation: 2, publication_id: 4, update: NativeUpdate::State { state: NativeState::Blocked, turn_id: Some("turn".into()) } },
        NativeRequest::Close { agent_session_id: id, activation, binding_proof, generation: 2 },
    ];
    for request in requests {
        let expected = frame(request.clone());
        common::assert_round_trip(&expected);
        let json = serde_json::to_vec(&request).unwrap();
        assert_eq!(common::decode(&raw_json(&json)).unwrap(), expected);
    }
}

#[test]
fn native_json_rejects_oversize_before_deserialization() {
    assert!(matches!(common::decode(&raw_json(&vec![b' '; MAX_NATIVE_PAYLOAD_BYTES + 1])), Err(DecodeError::NativeAgentLimitExceeded)));
    assert!(matches!(common::decode(&raw_json(&vec![b' '; MAX_NATIVE_PAYLOAD_BYTES])), Err(DecodeError::InvalidNativeAgentPayload)));
}

#[test]
fn native_json_rejects_malformed_and_wrong_dto() {
    for json in [b"{".as_slice(), b"{}", b"{\"type\":\"not_a_request\"}", b"{\"type\":\"register\"}"] {
        assert!(matches!(common::decode(&raw_json(json)), Err(DecodeError::InvalidNativeAgentPayload)));
    }
}

#[test]
fn native_receipts_use_existing_json_result_envelope() {
    let receipt = NativePublicationReceipt { publication_id: 4, first_seq: 20, last_seq: 21 };
    common::assert_round_trip(&FrameKind::CommandResult { request_id: 7, result: CommandResult::OkWith(CommandValue::Json(serde_json::to_string(&receipt).unwrap())) });
}

#[test]
fn later_native_outcomes_have_a_dedicated_event() {
    for disposition in [NativeDisposition::Admitted, NativeDisposition::Completed, NativeDisposition::Refused, NativeDisposition::Unknown] {
        common::assert_round_trip(&FrameKind::Event {
            terminal: Some(ResourceId::local(8)),
            event: phux_protocol::wire::frame::AgentEvent::NativeAgentResult {
                agent_session_id: ResourceId::local(9), operation_id: 3,
                disposition, detail: Some("actual SDK outcome".into()),
            },
            stamp: None,
        });
    }
    let oversized = FrameKind::Event {
        terminal: None,
        event: phux_protocol::wire::frame::AgentEvent::NativeAgentResult {
            agent_session_id: ResourceId::local(9), operation_id: 3,
            disposition: NativeDisposition::Unknown,
            detail: Some("x".repeat(MAX_NATIVE_PAYLOAD_BYTES)),
        },
        stamp: None,
    };
    assert!(matches!(common::decode(&common::encode(&oversized)), Err(DecodeError::NativeAgentLimitExceeded)));
}
