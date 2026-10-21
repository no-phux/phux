//! Protocol 1.0 fails closed on what a newer 1.x client may add: an unknown
//! modifier or role bit, a bool or presence byte other than `0`/`1`, an
//! undefined op, transport, format, or checkout area, and a spawn field
//! combination its kind's rules refuse. Each is refused `Unsupported`
//! (correlated where the frame has a request id) and nothing is applied;
//! the connection keeps working.

use phux_protocol::checkout::CheckoutRequest;
use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::field;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, ListenerTransport, PerfOp, RolePolicy,
    SpawnResource, TYPE_ATTACH, TYPE_INPUT_KEY, TYPE_INPUT_MOUSE, TYPE_PATH_QUERY,
};
use tokio::net::UnixStream;

use phux_server_testkit::{
    await_command_result, encode_frame, recv_until, run_local, spawn_server_connected,
};

use crate::newer_client::{
    assert_unsupported, assert_usable, command_body, next_error, raw_command, raw_frame, send_raw,
};

/// A local `ResourceId` (tag `0`, then the id).
const LOCAL_1: [u8; 5] = [0, 0, 0, 0, 1];

/// The next `ERROR` correlated to `request_id`.
async fn correlated_error(stream: &mut UnixStream, request_id: u32) -> (ErrorCode, String) {
    recv_until(stream, |_, frame| match frame {
        FrameKind::Error {
            request_id: Some(got),
            code,
            message,
        } if got == request_id => Some((code, message)),
        FrameKind::Detached { .. } => panic!("connection ended: {frame:?}"),
        _ => None,
    })
    .await
}

/// A key event: press of key 0 with these modifier bits and composing byte.
fn key_event(mods: u16, composing: u8) -> Vec<u8> {
    let mut event = 1u32.to_be_bytes().to_vec();
    event.extend_from_slice(&0u32.to_be_bytes());
    event.extend_from_slice(&mods.to_be_bytes());
    event.extend_from_slice(&[0, 0, composing, 0, 0]);
    event
}

/// A mouse move with these modifier bits.
fn mouse_event(mods: u16) -> Vec<u8> {
    let mut event = vec![0; 8];
    event.extend_from_slice(&mods.to_be_bytes());
    event.extend_from_slice(&1.0f64.to_be_bytes());
    event.extend_from_slice(&1.0f64.to_be_bytes());
    event
}

/// Uncorrelated frames carrying a newer bit or byte, with the field named.
fn uncorrelated_frames() -> Vec<(&'static str, Vec<u8>)> {
    let input = |type_byte, event: &[u8]| {
        raw_frame(
            type_byte,
            &[
                (field::input_key::TERMINAL_ID, &LOCAL_1),
                (field::input_key::EVENT, event),
            ],
        )
    };
    let attach = |id, value: &[u8]| raw_frame(TYPE_ATTACH, &[(id, value)]);
    vec![
        (
            "KeyEvent.mods",
            input(TYPE_INPUT_KEY, &key_event(0x8000, 0)),
        ),
        (
            "KeyEvent.consumed_mods",
            input(TYPE_INPUT_KEY, &{
                let mut event = key_event(0, 0);
                event[10] = 0x40;
                event
            }),
        ),
        (
            "KeyEvent.composing",
            input(TYPE_INPUT_KEY, &key_event(0, 2)),
        ),
        (
            "MouseEvent.mods",
            input(TYPE_INPUT_MOUSE, &mouse_event(0x0400)),
        ),
        ("RolePolicy", attach(field::attach::ROLE_POLICY, &[0x04])),
        (
            "Attach.request_scrollback",
            attach(field::attach::REQUEST_SCROLLBACK, &[2]),
        ),
    ]
}

#[test]
fn a_newer_bit_or_byte_in_an_uncorrelated_frame_is_refused() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        for (nonce, (field, frame)) in (200..).zip(uncorrelated_frames()) {
            send_raw(&mut stream, &frame).await;
            let (code, message) = next_error(&mut stream).await;
            assert_eq!(code, ErrorCode::Unsupported, "{field}: {message}");
            assert!(message.contains(field), "{message:?} names {field}");
            assert_usable(&mut stream, nonce).await;
        }
    });
}

/// `body` with the byte at `index` replaced.
fn patched(mut body: Vec<u8>, index: usize, value: u8) -> Vec<u8> {
    body[index] = value;
    body
}

/// `COMMAND` bodies carrying a newer value, with the field named.
fn newer_commands() -> Vec<(&'static str, Vec<u8>)> {
    let last = |command: Command, value: u8| {
        let body = command_body(command);
        let index = body.len() - 1;
        patched(body, index, value)
    };
    let checkout_tag = command_body(Command::Checkout {
        terminal_id: ResourceId::local(1),
        request: CheckoutRequest::Status,
    })[0];
    let mut checkout = vec![checkout_tag];
    checkout.extend_from_slice(&LOCAL_1);
    let json = br#"{"op":"diff","path":"a","area":"sideways"}"#;
    checkout.extend_from_slice(&u32::try_from(json.len()).unwrap().to_be_bytes());
    checkout.extend_from_slice(json);
    vec![
        (
            "PerfOp",
            last(
                Command::PerfControl {
                    op: PerfOp::TraceStart,
                },
                0xEE,
            ),
        ),
        (
            "ListenerTransport",
            patched(
                command_body(Command::OpenListener {
                    transport: ListenerTransport::Quic,
                    port_range: None,
                    linger_secs: 0,
                }),
                1,
                7,
            ),
        ),
        (
            "GetScreen.format",
            last(
                Command::GetScreen {
                    terminal_id: ResourceId::local(1),
                    request_scrollback: None,
                    cells: false,
                    format: 0,
                },
                9,
            ),
        ),
        ("CheckoutRequest", checkout),
        (
            "DetachClients.session",
            last(Command::DetachClients { session: None }, 2),
        ),
        (
            "GetFile.host",
            patched(
                command_body(Command::GetFile {
                    host: None,
                    path: "/tmp/x".to_owned(),
                    offset: 0,
                    max_bytes: 1,
                }),
                1,
                2,
            ),
        ),
        (
            "PutFile.sha256",
            last(
                Command::PutFile {
                    upload_id: phux_protocol::ids::FileUploadId::new([7; 16]).unwrap(),
                    terminal_id: ResourceId::local(1),
                    extension: "txt".to_owned(),
                    offset: 0,
                    data: b"x".to_vec(),
                    final_chunk: true,
                    sha256: None,
                },
                2,
            ),
        ),
        ("GetPerf.reset", last(Command::GetPerf { reset: false }, 2)),
        (
            "RolePolicy",
            patched(
                command_body(Command::AttachResource {
                    terminal_id: ResourceId::local(1),
                    role_policy: Some(RolePolicy::PRIMARY),
                    agent_after_seq: None,
                }),
                7,
                0x04,
            ),
        ),
    ]
}

#[test]
fn a_newer_value_in_a_command_is_refused_with_its_request_id() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        for (request_id, (field, body)) in (300..).zip(newer_commands()) {
            send_raw(&mut stream, &raw_command(request_id, &body)).await;
            let result = await_command_result(&mut stream, request_id).await;
            assert_unsupported(&result);
            let CommandResult::Error { message, .. } = result else {
                unreachable!()
            };
            assert!(message.contains(field), "{message:?} names {field}");
            assert_usable(&mut stream, u64::from(request_id)).await;
        }
    });
}

#[test]
fn a_spawn_its_kind_rules_refuse_and_a_newer_recursive_flag_are_refused_correlated() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        // An agent session with a working directory: a rule a newer minor
        // may relax, so a 1.0 server refuses the spawn but keeps the client.
        let spawn = FrameKind::SpawnResource {
            request_id: 71,
            group: GroupId::new(1),
            command: None,
            cwd: Some("/tmp".to_owned()),
            env: None,
            term: None,
            satellite: None,
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: Some(Box::new(SpawnResource::agent_session(
                ResourceId::local(1),
                "codex",
            ))),
        };
        send_raw(&mut stream, &encode_frame(&spawn)).await;
        let (code, _) = correlated_error(&mut stream, 71).await;
        assert_eq!(code, ErrorCode::Unsupported);
        assert_usable(&mut stream, 71).await;

        let query = raw_frame(
            TYPE_PATH_QUERY,
            &[
                (field::path_query::REQUEST_ID, &72u32.to_be_bytes()),
                (field::path_query::ROOT, b"/"),
                (field::path_query::QUERY, b""),
                (field::path_query::RECURSIVE, &[2]),
            ],
        );
        send_raw(&mut stream, &query).await;
        let (code, message) = correlated_error(&mut stream, 72).await;
        assert_eq!(code, ErrorCode::Unsupported);
        assert!(message.contains("PathQuery.recursive"), "{message:?}");
        assert_usable(&mut stream, 72).await;
    });
}
