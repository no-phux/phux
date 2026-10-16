//! Consumer contract: native intents never become PTY input or replay on loss.

use phux_client_runtime::control::{ControlOptions, ControlPlane, Event};
use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::native_agent::{
    MAX_NATIVE_PENDING, NativeCommandResult, NativeDisposition, NativeIntent, NativeRequest,
};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ErrorCode, FrameKind,
};
use phux_protocol::{PROTOCOL_VERSION, ResourceId};

use crate::embedded;

fn connected() -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions {
        automatic_lifecycle: false,
        ..ControlOptions::default()
    });
    plane.connection_opened();
    plane.take_outbound();
    let mut hello = embedded::hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        server_caps.features_ext = ServerFeatureExtSet::with(&[ServerFeatureExt::NativeAgent]);
    }
    plane.feed(hello).unwrap();
    plane.take_events();
    plane.take_outbound();
    plane
}

fn prompt() -> NativeIntent {
    NativeIntent::Prompt {
        text: "hello SDK".into(),
    }
}

fn sent(plane: &mut ControlPlane) -> (u32, u64) {
    let frames = plane.take_outbound();
    assert_eq!(frames.len(), 1);
    match embedded::decode(&frames[0]) {
        FrameKind::Command {
            request_id,
            command: Command::NativeAgent { request },
        } => {
            let NativeRequest::Command {
                agent_session_id,
                command,
            } = request.into_request()
            else {
                panic!("dedicated command")
            };
            assert_eq!(agent_session_id, ResourceId::local(22));
            assert_eq!(command.activation, "activation-A");
            assert_eq!(command.intent, prompt());
            (request_id, command.operation_id)
        }
        other => panic!("native command must not emit PTY input: {other:?}"),
    }
}

#[test]
fn requires_native_capability_before_scheduling() {
    let mut plane = ControlPlane::new(ControlOptions {
        automatic_lifecycle: false,
        ..ControlOptions::default()
    });
    plane.connection_opened();
    plane.take_outbound();
    plane
        .feed(embedded::hello_ok(PROTOCOL_VERSION.patch))
        .unwrap();
    plane.take_outbound();
    assert_eq!(
        plane.native_agent_command(&ResourceId::local(22), "activation-A", prompt()),
        None,
        "a successful legacy handshake does not authorize a new command tag"
    );
    assert!(plane.take_outbound().is_empty());
}

#[test]
fn validates_before_scheduling_and_bounds_pending_commands() {
    let mut offline = ControlPlane::new(ControlOptions::default());
    assert_eq!(
        offline.native_agent_command(&ResourceId::local(22), "activation-A", prompt()),
        None
    );
    assert!(offline.take_outbound().is_empty());
    let mut plane = connected();
    for intent in [
        NativeIntent::Prompt {
            text: "  \n".into(),
        },
        NativeIntent::Prompt {
            text: "x".repeat(16 * 1024),
        },
        NativeIntent::Interrupt {
            turn_id: String::new(),
        },
        NativeIntent::Answer {
            question_id: String::new(),
            response: serde_json::json!({"confirmed": true}),
        },
    ] {
        assert_eq!(
            plane.native_agent_command(&ResourceId::local(22), "activation-A", intent),
            None
        );
    }
    assert_eq!(
        plane.native_agent_command(&ResourceId::local(22), "", prompt()),
        None
    );
    assert_eq!(
        plane.native_agent_command(&ResourceId::local(22), &"a".repeat(16 * 1024), prompt()),
        None,
        "the complete request must fit even when the intent alone is valid"
    );
    assert!(plane.take_outbound().is_empty());
    assert!(plane.take_events().is_empty());
    let mut ids = std::collections::HashSet::new();
    for _ in 0..MAX_NATIVE_PENDING {
        let id = plane
            .native_agent_command(&ResourceId::local(22), "activation-A", prompt())
            .unwrap();
        assert_ne!(id, 0);
        assert!(ids.insert(id));
        assert_eq!(sent(&mut plane).1, id);
    }
    assert_eq!(
        plane.native_agent_command(&ResourceId::local(22), "activation-A", prompt()),
        None
    );
    assert!(plane.take_outbound().is_empty());
}

#[test]
fn keeps_all_native_dispositions_and_provider_detail_without_input_receipts() {
    let mut plane = connected();
    for disposition in [
        NativeDisposition::Admitted,
        NativeDisposition::Queued,
        NativeDisposition::Completed,
        NativeDisposition::Refused,
        NativeDisposition::Unknown,
    ] {
        let operation_id = plane
            .native_agent_command(&ResourceId::local(22), "activation-A", prompt())
            .unwrap();
        let (request_id, _) = sent(&mut plane);
        let receipt = NativeCommandResult {
            operation_id,
            disposition,
            detail: Some("SDK submission only".into()),
        };
        plane
            .feed(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::Json(
                    serde_json::to_string(&receipt).unwrap(),
                )),
            })
            .unwrap();
        let events = plane.take_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::NativeAgentResult {
            agent_session_id, operation_id: received, disposition: actual, detail,
        } if *agent_session_id == ResourceId::local(22) && *received == operation_id && *actual == disposition && detail.as_deref() == Some("SDK submission only")));
        assert!(events[0].is_lossless());
    }
}

#[test]
fn lost_receipt_is_unknown_and_never_replays_on_reconnect() {
    let mut plane = connected();
    let operation_id = plane
        .native_agent_command(&ResourceId::local(22), "activation-A", prompt())
        .unwrap();
    sent(&mut plane);
    plane.connection_lost(Some("ack lost".into()));
    let events = plane.take_events();
    assert_eq!(events.iter().filter(|event| matches!(event, Event::NativeAgentResult { operation_id: id, disposition: NativeDisposition::Unknown, .. } if *id == operation_id)).count(), 1);
    plane.connection_opened();
    plane.take_outbound();
    plane
        .feed(embedded::hello_ok(PROTOCOL_VERSION.patch))
        .unwrap();
    assert!(!plane.take_outbound().iter().any(|bytes| matches!(
        embedded::decode(bytes),
        FrameKind::Command {
            command: Command::NativeAgent { .. },
            ..
        }
    )));
}

#[test]
fn mismatched_receipt_is_unknown_and_later_provider_completion_preserves_exact_agent() {
    let mut plane = connected();
    let operation_id = plane
        .native_agent_command(&ResourceId::local(22), "activation-A", prompt())
        .unwrap();
    let (request_id, _) = sent(&mut plane);
    let receipt = NativeCommandResult {
        operation_id: operation_id.wrapping_add(1),
        disposition: NativeDisposition::Completed,
        detail: None,
    };
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::Json(
                serde_json::to_string(&receipt).unwrap(),
            )),
        })
        .unwrap();
    assert!(matches!(
        plane.take_events().as_slice(),
        [Event::NativeAgentResult {
            disposition: NativeDisposition::Unknown,
            ..
        }]
    ));
    // The event can be scoped to a parent terminal; its explicit agent identity
    // must not be replaced with that terminal or the latest child.
    plane
        .feed(FrameKind::Event {
            terminal: Some(ResourceId::local(7)),
            stamp: None,
            event: AgentEvent::NativeAgentResult {
                agent_session_id: ResourceId::local(22),
                operation_id,
                disposition: NativeDisposition::Completed,
                detail: Some("provider finished".into()),
            },
        })
        .unwrap();
    assert!(
        matches!(plane.take_events().as_slice(), [Event::NativeAgentResult {
        agent_session_id, operation_id: id, disposition: NativeDisposition::Completed, detail,
    }] if *agent_session_id == ResourceId::local(22) && *id == operation_id && detail.as_deref() == Some("provider finished"))
    );
}

#[test]
fn broker_refusal_is_not_fabricated_as_delivery() {
    let mut plane = connected();
    plane
        .native_agent_command(&ResourceId::local(22), "activation-A", prompt())
        .unwrap();
    let (request_id, _) = sent(&mut plane);
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Error {
                code: ErrorCode::PermissionDenied,
                message: "question already answered".into(),
            },
        })
        .unwrap();
    assert!(
        matches!(plane.take_events().as_slice(), [Event::NativeAgentResult {
        disposition: NativeDisposition::Refused, detail, ..
    }] if detail.as_deref() == Some("question already answered"))
    );
}
