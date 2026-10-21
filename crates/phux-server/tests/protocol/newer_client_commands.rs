//! Protocol 1.x `COMMAND` growth: a JSON payload naming an op or field this
//! build does not know is refused `Unsupported` with its request id (still
//! refused, never closed), and a command body a newer client extends after
//! its optional fields decodes as the command this build knows.

use phux_protocol::checkout::CheckoutRequest;
use phux_protocol::ids::{IdempotencyKey, ResourceId};
use phux_protocol::native_agent::{
    NativeCommand, NativeIntent, NativeRequest, NativeRequestPayload,
};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, InputMode, KillPrecondition, RolePolicy, TerminalSignal,
};

use phux_server_testkit::{await_command_result, run_local, spawn_server_connected};

use crate::newer_client::{assert_unsupported, assert_usable, command_body, raw_command, send_raw};

/// A local `ResourceId` (tag `0`, then the id).
const LOCAL_1: [u8; 5] = [0, 0, 0, 0, 1];

/// A `u32`-length-prefixed byte string.
fn prefixed(bytes: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(bytes.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(bytes);
    out
}

/// A `CHECKOUT` body for Terminal 1 carrying this raw JSON request.
fn checkout_body(json: &[u8]) -> Vec<u8> {
    let tag = command_body(Command::Checkout {
        terminal_id: ResourceId::local(1),
        request: CheckoutRequest::Status,
    })[0];
    let mut body = vec![tag];
    body.extend_from_slice(&LOCAL_1);
    body.extend_from_slice(&prefixed(json));
    body
}

/// A `NATIVE_AGENT` body carrying this raw JSON request.
fn native_body(json: &[u8]) -> Vec<u8> {
    let request = NativeRequestPayload::new(NativeRequest::Close {
        agent_session_id: ResourceId::local(1),
        activation: "a".into(),
        binding_proof: "p".into(),
        generation: 1,
    })
    .unwrap();
    let mut body = vec![command_body(Command::NativeAgent { request })[0]];
    body.extend_from_slice(&prefixed(json));
    body
}

/// A native `command` request whose `NativeCommand` (which denies unknown
/// fields) carries one a newer client added.
fn native_command_with_a_newer_field() -> Vec<u8> {
    let mut json = serde_json::to_value(NativeRequest::Command {
        agent_session_id: ResourceId::local(1),
        command: NativeCommand {
            operation_id: 1,
            activation: "a".into(),
            intent: NativeIntent::Prompt { text: "x".into() },
        },
    })
    .unwrap();
    json["command"]["later"] = serde_json::Value::Bool(true);
    serde_json::to_vec(&json).unwrap()
}

#[test]
fn an_unknown_json_op_or_field_is_refused_with_its_request_id() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        let cases = [
            checkout_body(br#"{"op":"rebase_interactive"}"#),
            checkout_body(br#"{"op":"tree","dir":"","depth":3}"#),
            native_body(br#"{"type":"teleport"}"#),
            native_body(&native_command_with_a_newer_field()),
        ];
        for (request_id, body) in (40..).zip(cases) {
            send_raw(&mut stream, &raw_command(request_id, &body)).await;
            let result = await_command_result(&mut stream, request_id).await;
            assert_unsupported(&result);
            assert_usable(&mut stream, u64::from(request_id)).await;
        }
    });
}

#[test]
fn an_unknown_enum_keeps_its_value_in_the_refusal() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        let mut acquire = command_body(Command::AcquireInput {
            terminal_id: ResourceId::local(1),
            mode: InputMode::Seize,
            ttl_ms: 1,
        });
        // tag, the 5-byte id, then the mode byte.
        acquire[6] = 0x7E;
        send_raw(&mut stream, &raw_command(50, &acquire)).await;
        let result = await_command_result(&mut stream, 50).await;
        assert_unsupported(&result);
        let CommandResult::Error { message, .. } = result else {
            unreachable!()
        };
        assert!(message.contains("InputMode value 126"), "{message:?}");
        assert_usable(&mut stream, 5).await;
    });
}

/// Every command whose body ends in optional fields, keyed or with each
/// optional present, so the tail follows the last field this build reads.
fn commands_with_optional_fields() -> Vec<Command> {
    let key = Some(IdempotencyKey::new([0x5a; 16]).unwrap());
    let id = ResourceId::local(424_242);
    let mut commands = Vec::new();
    for operation_id in [None, key] {
        commands.extend([
            Command::KillResource {
                terminal_id: id.clone(),
                operation_id,
            },
            Command::KillResourceIf {
                terminal_id: id.clone(),
                precondition: KillPrecondition::default(),
                operation_id,
            },
            Command::KillResources {
                ids: vec![id.clone()],
                operation_id,
            },
            Command::SignalTerminal {
                terminal_id: id.clone(),
                signal: TerminalSignal::Interrupt,
                operation_id,
            },
        ]);
    }
    for (role_policy, agent_after_seq) in [(None, None), (Some(RolePolicy::VIEWER), Some(9))] {
        commands.push(Command::AttachResource {
            terminal_id: id.clone(),
            role_policy,
            agent_after_seq,
        });
    }
    for request_scrollback in [None, Some(10)] {
        commands.push(Command::GetScreen {
            terminal_id: id.clone(),
            request_scrollback,
            cells: true,
            format: 0,
        });
    }
    commands
}

#[test]
fn a_tail_after_a_commands_optional_fields_is_skipped() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        for (request_id, command) in (60..).zip(commands_with_optional_fields()) {
            let mut body = command_body(command.clone());
            body.extend_from_slice(&[0x01, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE]);
            send_raw(&mut stream, &raw_command(request_id, &body)).await;
            let result = await_command_result(&mut stream, request_id).await;
            assert!(
                !matches!(
                    result,
                    CommandResult::Error {
                        code: ErrorCode::Unsupported | ErrorCode::MalformedMessage,
                        ..
                    }
                ),
                "{command:?} with a tail: {result:?}"
            );
            assert_usable(&mut stream, u64::from(request_id)).await;
        }
    });
}
