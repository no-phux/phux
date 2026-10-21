//! Protocol 1.x from the server's side: a newer 1.x client may send what
//! this build does not know, and the connection must survive it.
//!
//! After HELLO, an unknown frame type earns an uncorrelated
//! `ERROR { UNSUPPORTED }` naming the type; a `COMMAND` whose tag, or an
//! enum inside whose body, is unknown earns a `COMMAND_RESULT` error
//! `UNSUPPORTED` correlated by its `request_id`. Bytes appended to a field
//! value and unknown field ids are skipped. Each case then proves the
//! connection still works. Only truly malformed bytes still close it.

use std::time::Duration;

use bytes::BytesMut;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::decode::Decoder;
use phux_protocol::wire::encode::Encoder;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, StateScope, TYPE_COMMAND, TYPE_PING,
};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use phux_server_testkit::{
    await_command_result, encode_frame, expect_protocol_error_close, recv_until, run_local,
    spawn_server_connected,
};

/// A framed message of `type_byte` with these TLV fields, in order.
fn raw_frame(type_byte: u8, fields: &[(u32, &[u8])]) -> Vec<u8> {
    let mut body = BytesMut::new();
    let mut enc = Encoder::new(&mut body);
    enc.write_u8(type_byte);
    for (id, value) in fields {
        enc.write_field(*id, value);
    }
    let mut frame = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    frame.extend_from_slice(&body);
    frame
}

/// The `COMMAND` field (id 2) this build writes for `command`.
fn command_body(command: Command) -> Vec<u8> {
    let bytes = encode_frame(&FrameKind::Command {
        request_id: 0,
        command,
    });
    let mut dec = Decoder::new(&bytes[5..]);
    while let Some((id, value)) = dec.read_field().unwrap() {
        if id == 2 {
            return value.to_vec();
        }
    }
    panic!("COMMAND without a command field");
}

/// A `COMMAND` with `request_id` and this raw command body.
fn raw_command(request_id: u32, body: &[u8]) -> Vec<u8> {
    raw_frame(TYPE_COMMAND, &[(1, &request_id.to_be_bytes()), (2, body)])
}

async fn send_raw(stream: &mut UnixStream, bytes: &[u8]) {
    stream.write_all(bytes).await.unwrap();
    stream.flush().await.unwrap();
}

/// The connection is still open and dispatching: a `PING` gets its `PONG`.
async fn assert_usable(stream: &mut UnixStream, nonce: u64) {
    let ping = raw_frame(TYPE_PING, &[(1, &nonce.to_be_bytes())]);
    send_raw(stream, &ping).await;
    recv_until(stream, |_, frame| match frame {
        FrameKind::Pong { nonce: got } if got == nonce => Some(()),
        FrameKind::Detached { .. } => panic!("connection ended: {frame:?}"),
        _ => None,
    })
    .await;
}

/// The next uncorrelated `ERROR`.
async fn next_error(stream: &mut UnixStream) -> (ErrorCode, String) {
    recv_until(stream, |_, frame| match frame {
        FrameKind::Error {
            request_id: None,
            code,
            message,
        } => Some((code, message)),
        _ => None,
    })
    .await
}

fn assert_unsupported(result: &CommandResult) {
    assert!(
        matches!(
            result,
            CommandResult::Error {
                code: ErrorCode::Unsupported,
                ..
            }
        ),
        "{result:?}"
    );
}

#[test]
fn an_unknown_frame_type_is_refused_by_name_and_the_connection_survives() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        for type_byte in [0x40_u8, 0x7E, 0xEE] {
            let frame = raw_frame(type_byte, &[(1, b"from a newer client")]);
            assert!(
                matches!(
                    FrameKind::decode(&frame),
                    Err(DecodeError::UnknownFrameKind { .. })
                ),
                "{type_byte:#04x} must be unallocated for this test"
            );
            send_raw(&mut stream, &frame).await;
            let (code, message) = next_error(&mut stream).await;
            assert_eq!(code, ErrorCode::Unsupported);
            assert!(
                message.contains(&format!("0x{type_byte:02x}")),
                "{message:?} names the type"
            );
            assert_usable(&mut stream, u64::from(type_byte)).await;
        }
    });
}

#[test]
fn an_unknown_command_tag_is_refused_with_its_request_id() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        // The request id may follow the command field: field order is free.
        let frame = raw_frame(
            TYPE_COMMAND,
            &[(2, &[0x7F, 1, 2]), (1, &77u32.to_be_bytes())],
        );
        send_raw(&mut stream, &frame).await;
        assert_unsupported(&await_command_result(&mut stream, 77).await);
        assert_usable(&mut stream, 1).await;

        // A known command still works on the same connection.
        let get_state = command_body(Command::GetState {
            scope: StateScope::Server,
        });
        send_raw(&mut stream, &raw_command(78, &get_state)).await;
        assert!(matches!(
            await_command_result(&mut stream, 78).await,
            CommandResult::OkWith(CommandValue::State(_))
        ));
    });
}

#[test]
fn an_unknown_enum_inside_a_known_command_is_refused_with_its_request_id() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        let mut get_state = command_body(Command::GetState {
            scope: StateScope::Server,
        });
        // GET_STATE is a tag then a `StateScope` tag; name a newer scope.
        *get_state.last_mut().unwrap() = 0x7F;
        send_raw(&mut stream, &raw_command(90, &get_state)).await;
        let result = await_command_result(&mut stream, 90).await;
        assert_unsupported(&result);
        let CommandResult::Error { message, .. } = result else {
            unreachable!()
        };
        assert!(message.contains("StateScope"), "{message:?}");
        assert_usable(&mut stream, 2).await;
    });
}

#[test]
fn bytes_and_fields_a_newer_client_appends_are_skipped() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        // A PING whose nonce field grew, beside a field id no build defines.
        let nonce = 0x5eed_u64;
        let mut grown_nonce = nonce.to_be_bytes().to_vec();
        grown_nonce.extend_from_slice(&[0xEE; 4]);
        send_raw(
            &mut stream,
            &raw_frame(TYPE_PING, &[(1, &grown_nonce), (99, b"future field")]),
        )
        .await;
        let echoed = recv_until(&mut stream, |_, frame| match frame {
            FrameKind::Pong { nonce } => Some(nonce),
            _ => None,
        })
        .await;
        assert_eq!(echoed, nonce);

        // A command body with bytes after everything this build reads.
        let mut get_state = command_body(Command::GetState {
            scope: StateScope::Server,
        });
        get_state.extend_from_slice(&[0xEE, 0xEE, 0xEE]);
        send_raw(&mut stream, &raw_command(31, &get_state)).await;
        assert!(matches!(
            await_command_result(&mut stream, 31).await,
            CommandResult::OkWith(CommandValue::State(_))
        ));
        assert_usable(&mut stream, 3).await;
    });
}

#[test]
fn truly_malformed_command_bytes_still_close_the_connection() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        let mut get_state = command_body(Command::GetState {
            scope: StateScope::Server,
        });
        // GET_STATE without its scope: truncated, not newer.
        get_state.truncate(1);
        send_raw(&mut stream, &raw_command(12, &get_state)).await;
        let (code, _) = next_error(&mut stream).await;
        assert_eq!(code, ErrorCode::MalformedMessage);
        expect_protocol_error_close(&mut stream, Duration::from_secs(5)).await;
    });
}
