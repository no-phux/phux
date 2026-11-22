//! Protocol 1.x: an unknown value inside a known, well-framed non-`COMMAND`
//! frame (an input enum, an attach target, a metadata scope, a resource id
//! tag) is refused `Unsupported`, correlated by request id where the frame
//! has one, and the connection keeps working. Nothing in it is applied.

use phux_protocol::wire::field;
use phux_protocol::wire::frame::{
    ErrorCode, FrameKind, TYPE_ATTACH, TYPE_GET_METADATA, TYPE_INPUT_FOCUS, TYPE_INPUT_KEY,
    TYPE_INPUT_MOUSE, TYPE_INPUT_PASTE, TYPE_RESIZE_TERMINAL, TYPE_SET_METADATA,
};
use tokio::net::UnixStream;

use phux_server_testkit::{recv_until, run_local, spawn_server_connected};

use crate::newer_client::{assert_usable, next_error, raw_frame, send_raw};

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

/// A key event: `action`, `key`, then empty mods, not composing, no text,
/// no unshifted codepoint.
fn key_event(action: u32, key: u32) -> Vec<u8> {
    let mut event = action.to_be_bytes().to_vec();
    event.extend_from_slice(&key.to_be_bytes());
    event.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0]);
    event
}

/// A mouse event: `action`, `button`, empty mods, then `x` and `y`.
fn mouse_event(action: u32, button: u32) -> Vec<u8> {
    let mut event = action.to_be_bytes().to_vec();
    event.extend_from_slice(&button.to_be_bytes());
    event.extend_from_slice(&[0, 0]);
    event.extend_from_slice(&1.0f64.to_be_bytes());
    event.extend_from_slice(&1.0f64.to_be_bytes());
    event
}

/// Uncorrelated frames naming a value no build of this line defines yet.
fn uncorrelated_newer_frames() -> Vec<(&'static str, Vec<u8>)> {
    let input = |type_byte, event: &[u8]| {
        raw_frame(
            type_byte,
            &[
                (field::input_key::TERMINAL_ID, &LOCAL_1),
                (field::input_key::EVENT, event),
            ],
        )
    };
    let mut paste = vec![0x7E];
    paste.extend_from_slice(&3u32.to_be_bytes());
    paste.extend_from_slice(b"abc");
    let mut resize_id = vec![0x7E];
    resize_id.extend_from_slice(&1u32.to_be_bytes());
    vec![
        (
            "PhysicalKey",
            input(TYPE_INPUT_KEY, &key_event(1, 0x7FFF_FFFF)),
        ),
        ("KeyAction", input(TYPE_INPUT_KEY, &key_event(0x7E, 0))),
        (
            "MouseButton",
            input(TYPE_INPUT_MOUSE, &mouse_event(0, 0x7E)),
        ),
        (
            "MouseAction",
            input(TYPE_INPUT_MOUSE, &mouse_event(0x7E, 0)),
        ),
        ("PasteTrust", input(TYPE_INPUT_PASTE, &paste)),
        ("FocusEvent", input(TYPE_INPUT_FOCUS, &[0x7E])),
        (
            "AttachTarget",
            raw_frame(TYPE_ATTACH, &[(field::attach::TARGET, &[0x7E])]),
        ),
        (
            "ResourceId",
            raw_frame(
                TYPE_RESIZE_TERMINAL,
                &[
                    (field::terminal_resize::TERMINAL_ID, &resize_id),
                    (field::terminal_resize::COLS, &80u16.to_be_bytes()),
                    (field::terminal_resize::ROWS, &24u16.to_be_bytes()),
                ],
            ),
        ),
    ]
}

#[test]
fn an_unknown_value_in_an_uncorrelated_frame_is_refused_and_the_connection_survives() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        for (nonce, (field, frame)) in (100..).zip(uncorrelated_newer_frames()) {
            send_raw(&mut stream, &frame).await;
            let (code, message) = next_error(&mut stream).await;
            assert_eq!(code, ErrorCode::Unsupported, "{field}: {message}");
            assert!(message.contains(field), "{message:?} names {field}");
            assert_usable(&mut stream, nonce).await;
        }
    });
}

#[test]
fn an_unknown_scope_in_a_metadata_request_is_refused_with_its_request_id() {
    run_local(async {
        let (_server, mut stream) = spawn_server_connected(None).await;
        // GET_METADATA naming a `Scope` tag a newer build added.
        let get = raw_frame(
            TYPE_GET_METADATA,
            &[
                (field::get_metadata::REQUEST_ID, &61u32.to_be_bytes()),
                (field::get_metadata::SCOPE, &[0x7E]),
                (field::get_metadata::KEY, b"k"),
            ],
        );
        send_raw(&mut stream, &get).await;
        let (code, message) = correlated_error(&mut stream, 61).await;
        assert_eq!(code, ErrorCode::Unsupported);
        assert!(message.contains("Scope"), "{message:?}");
        assert_usable(&mut stream, 1).await;

        // SET_METADATA whose resource scope names a newer `ResourceId` tag:
        // refused before anything is written.
        let mut scope = vec![0x00, 0x7E];
        scope.extend_from_slice(&1u32.to_be_bytes());
        let set = raw_frame(
            TYPE_SET_METADATA,
            &[
                (field::set_metadata::REQUEST_ID, &62u32.to_be_bytes()),
                (field::set_metadata::SCOPE, &scope),
                (field::set_metadata::KEY, b"k"),
                (field::set_metadata::VALUE, b"v"),
            ],
        );
        send_raw(&mut stream, &set).await;
        let (code, message) = correlated_error(&mut stream, 62).await;
        assert_eq!(code, ErrorCode::Unsupported);
        assert!(message.contains("ResourceId"), "{message:?}");
        assert_usable(&mut stream, 2).await;
    });
}
