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

#[test]
fn older_native_catalogs_do_not_imply_an_auth_command() {
    let catalog: NativeIntegrationCatalog = serde_json::from_str(r#"{"integrations":[{"integration":"opencode","display_name":"OpenCode","native_available":true,"native_detail":null}]}"#).unwrap();
    assert_eq!(catalog.integrations[0].auth_command, None);
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
        NativeRequest::ListIntegrations {},
        NativeRequest::LaunchIntegration {
            integration: "opencode".into(),
            session: phux_protocol::ids::SessionKey::new([8; 16]).unwrap(),
            cwd: "/workspace".into(),
        },
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
/// Native requests a newer client extended: a field on the spawn spec, on
/// the request itself, and on a publication's update.
fn native_requests_with_a_newer_field() -> Vec<Vec<u8>> {
    let spawn = NativeRequest::Spawn {
        spec: SpawnNativeAgent {
            session: "s".into(),
            backend: "b".into(),
            provider: "p".into(),
            command: vec!["agent".into()],
            cwd: None,
            env: std::collections::BTreeMap::new(),
            native_id: None,
            guardian: None,
            guardian_key: None,
            guardian_expected: None,
        },
    };
    let close = NativeRequest::Close {
        agent_session_id: ResourceId::local(1),
        activation: "a".into(),
        binding_proof: "p".into(),
        generation: 1,
    };
    let publish = NativeRequest::Publish {
        agent_session_id: ResourceId::local(1),
        activation: "a".into(),
        binding_proof: "p".into(),
        generation: 1,
        publication_id: 1,
        update: NativeUpdate::Notice {
            level: "info".into(),
            text: "t".into(),
        },
    };
    let mut cases = Vec::new();
    for (request, path) in [(spawn, "spec"), (close, ""), (publish, "update")] {
        let mut json = serde_json::to_value(request).unwrap();
        let target = if path.is_empty() {
            &mut json
        } else {
            &mut json[path]
        };
        target["new_opt"] = serde_json::Value::Bool(true);
        cases.push(serde_json::to_vec(&json).unwrap());
    }
    cases
}

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
    let mut cases = vec![br#"{"type":"not_a_request"}"#.to_vec(), unknown_field];
    cases.extend(native_requests_with_a_newer_field());
    for json in &cases {
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
        // `detail` sits behind a presence byte, never inferred from length.
        body.push(u8::from(detail.is_some()));
        if let Some(detail) = detail {
            body.extend_from_slice(&u32::try_from(detail.len()).unwrap().to_be_bytes());
            body.extend_from_slice(detail.as_bytes());
        }
        let raw = |body: Vec<u8>| FrameKind::Event {
            terminal: Some(ResourceId::local(8)),
            event: AgentEvent::Unknown { tag: 0x1c, body },
            stamp: None,
        };
        let mut grown = body.clone();
        grown.extend_from_slice(&[0xEE, 0xEE]);
        let wire = raw(body);
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
        // A newer peer's bytes after `detail` are skipped.
        assert_eq!(
            common::decode(&common::encode(&raw(grown))).unwrap(),
            expected
        );
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

/// The types that also travel server to client stay tolerant: a stored
/// launch recipe and a relayed publication from a newer server still read,
/// and a newer state or disposition folds to `Unknown`.
#[test]
fn shared_native_types_tolerate_a_newer_peer() {
    let spec: SpawnNativeAgent = serde_json::from_str(
        r#"{"session":"s","backend":"b","provider":"p","command":["a"],"cwd":null,"env":{},"later":1}"#,
    )
    .unwrap();
    assert_eq!(spec.provider, "p");
    let update: NativeUpdate =
        serde_json::from_str(r#"{"type":"state","state":"thinking","turn_id":null,"later":1}"#)
            .unwrap();
    assert!(matches!(
        update,
        NativeUpdate::State {
            state: NativeState::Unknown,
            ..
        }
    ));
    let result: NativeCommandResult =
        serde_json::from_str(r#"{"operation_id":1,"disposition":"deferred","detail":null}"#)
            .unwrap();
    assert_eq!(result.disposition, NativeDisposition::Unknown);
}

/// The server's request decode is the strict side: a value a tolerant type
/// folded to `Unknown` is refused, not run as `Unknown`.
#[test]
fn a_request_carrying_a_newer_value_is_refused_as_unsupported() {
    let mut publish = serde_json::to_value(NativeRequest::Publish {
        agent_session_id: ResourceId::local(1),
        activation: "a".into(),
        binding_proof: "p".into(),
        generation: 1,
        publication_id: 1,
        update: NativeUpdate::State {
            state: NativeState::Working,
            turn_id: None,
        },
    })
    .unwrap();
    publish["update"]["state"] = serde_json::Value::String("thinking".into());
    assert_eq!(
        common::decode(&raw_json(&serde_json::to_vec(&publish).unwrap())).unwrap_err(),
        DecodeError::UnsupportedPayload {
            payload: "NativeRequest"
        }
    );
}
