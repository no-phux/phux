//! Consumer contract: native intents never become PTY input or replay on loss.

use phux_client_runtime::control::{ControlOptions, ControlPlane, Event};
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::native_agent::{
    MAX_NATIVE_PENDING, NativeCommandResult, NativeDisposition, NativeIntent, NativeRequest,
};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, CommandValue, ErrorCode, FrameKind,
};
use phux_protocol::{PROTOCOL_VERSION, ResourceId};

use crate::embedded;
use phux_wire_fixtures::command_err;

fn manual() -> ControlOptions {
    ControlOptions {
        automatic_lifecycle: false,
        ..ControlOptions::default()
    }
}

fn connected() -> ControlPlane {
    let mut plane = embedded::opened(
        manual(),
        super::hello_ok_ext(&[ServerFeatureExt::NativeAgent]),
    );
    plane.take_events();
    plane.take_outbound();
    plane
}

const AGENT: ResourceId = ResourceId::local(22);

/// Submit `prompt()` to `AGENT`; returns its operation and request ids.
fn submit(plane: &mut ControlPlane) -> (u64, u32) {
    let operation_id = plane
        .native_agent_command(&AGENT, "activation-A", prompt())
        .unwrap();
    let (request_id, _) = sent(plane);
    (operation_id, request_id)
}

fn answer_receipt(plane: &mut ControlPlane, request_id: u32, receipt: &NativeCommandResult) {
    plane
        .feed(FrameKind::CommandResult {
            request_id,
            result: CommandResult::OkWith(CommandValue::Json(
                serde_json::to_string(receipt).unwrap(),
            )),
        })
        .unwrap();
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
            assert_eq!(agent_session_id, AGENT);
            assert_eq!(command.activation, "activation-A");
            assert_eq!(command.intent, prompt());
            (request_id, command.operation_id)
        }
        other => panic!("native command must not emit PTY input: {other:?}"),
    }
}

#[test]
fn requires_native_capability_before_scheduling() {
    let mut plane = embedded::opened(manual(), embedded::hello_ok(PROTOCOL_VERSION.patch));
    plane.take_outbound();
    assert_eq!(
        plane.native_agent_command(&AGENT, "activation-A", prompt()),
        None,
        "a successful legacy handshake does not authorize a new command tag"
    );
    assert!(plane.take_outbound().is_empty());
}

#[test]
fn validates_before_scheduling_and_bounds_pending_commands() {
    let mut offline = ControlPlane::new(std::time::Instant::now(), ControlOptions::default());
    assert_eq!(
        offline.native_agent_command(&AGENT, "activation-A", prompt()),
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
            plane.native_agent_command(&AGENT, "activation-A", intent),
            None
        );
    }
    assert_eq!(plane.native_agent_command(&AGENT, "", prompt()), None);
    assert_eq!(
        plane.native_agent_command(&AGENT, &"a".repeat(16 * 1024), prompt()),
        None,
        "the complete request must fit even when the intent alone is valid"
    );
    assert!(plane.take_outbound().is_empty());
    assert!(plane.take_events().is_empty());
    let mut ids = std::collections::HashSet::new();
    for _ in 0..MAX_NATIVE_PENDING {
        let id = plane
            .native_agent_command(&AGENT, "activation-A", prompt())
            .unwrap();
        assert_ne!(id, 0);
        assert!(ids.insert(id));
        assert_eq!(sent(&mut plane).1, id);
    }
    assert_eq!(
        plane.native_agent_command(&AGENT, "activation-A", prompt()),
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
        let (operation_id, request_id) = submit(&mut plane);
        let receipt = NativeCommandResult {
            operation_id,
            disposition,
            detail: Some("SDK submission only".into()),
        };
        answer_receipt(&mut plane, request_id, &receipt);
        let events = plane.take_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::NativeAgentResult {
            agent_session_id, operation_id: received, disposition: actual, detail,
        } if *agent_session_id == AGENT && *received == operation_id && *actual == disposition && detail.as_deref() == Some("SDK submission only")));
        assert!(events[0].is_lossless());
    }
}

#[test]
fn lost_receipt_is_unknown_and_never_replays_on_reconnect() {
    let mut plane = connected();
    let (operation_id, _) = submit(&mut plane);
    plane.connection_lost(Some("ack lost".into()));
    let events = plane.take_events();
    assert_eq!(events.iter().filter(|event| matches!(event, Event::NativeAgentResult { operation_id: id, disposition: NativeDisposition::Unknown, .. } if *id == operation_id)).count(), 1);
    embedded::reopen(&mut plane, embedded::hello_ok(PROTOCOL_VERSION.patch));
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
    let (operation_id, request_id) = submit(&mut plane);
    let receipt = NativeCommandResult {
        operation_id: operation_id.wrapping_add(1),
        disposition: NativeDisposition::Completed,
        detail: None,
    };
    answer_receipt(&mut plane, request_id, &receipt);
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
                agent_session_id: AGENT,
                operation_id,
                disposition: NativeDisposition::Completed,
                detail: Some("provider finished".into()),
            },
        })
        .unwrap();
    assert!(
        matches!(plane.take_events().as_slice(), [Event::NativeAgentResult {
        agent_session_id, operation_id: id, disposition: NativeDisposition::Completed, detail,
    }] if *agent_session_id == AGENT && *id == operation_id && detail.as_deref() == Some("provider finished"))
    );
}

#[test]
fn broker_refusal_is_not_fabricated_as_delivery() {
    let mut plane = connected();
    let (_, request_id) = submit(&mut plane);
    plane
        .feed(command_err(
            request_id,
            ErrorCode::PermissionDenied,
            "question already answered",
        ))
        .unwrap();
    assert!(
        matches!(plane.take_events().as_slice(), [Event::NativeAgentResult {
        disposition: NativeDisposition::Refused, detail, ..
    }] if detail.as_deref() == Some("question already answered"))
    );
}

/// `UNSUPPORTED` promises nothing ran, so it is refused; an unreadable reply
/// or a newer server's code may mean the prompt landed, so it is unknown
/// and the caller does not re-prompt.
#[test]
fn unsupported_is_refused_and_unreadable_or_newer_codes_are_unknown() {
    for (code, expected) in [
        (ErrorCode::Unsupported, NativeDisposition::Refused),
        (ErrorCode::UNREADABLE_REPLY, NativeDisposition::Unknown),
        (ErrorCode::Unknown(299), NativeDisposition::Unknown),
    ] {
        let mut plane = connected();
        let (_, request_id) = submit(&mut plane);
        plane
            .feed(command_err(request_id, code, "newer server"))
            .unwrap();
        assert!(
            matches!(plane.take_events().as_slice(), [Event::NativeAgentResult {
                disposition, ..
            }] if *disposition == expected),
            "{code:?}"
        );
    }
}
