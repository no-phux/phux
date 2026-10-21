//! Protocol 1.x `HELLO` growth. HELLO precedes negotiation, so a value
//! this build does not know cannot be refused in place: a newer client
//! would be closed and redial forever. Unknown capability values decode to
//! documented defaults, over-large limits clamp, and the connection is
//! admitted.

use std::time::Duration;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    ClientCapabilities, LayerSet, MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_PAGE_BYTES,
};
use phux_protocol::wire::decode::Decoder;
use phux_protocol::wire::field;
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, FrameKind, StateScope, TYPE_HELLO,
};

use phux_server_testkit::{
    command, encode_frame, recv_typed, run_local, spawn_server, wait_for_raw_socket,
};

use crate::newer_client::{raw_frame, send_raw};

/// This build's HELLO, its capability record rewritten the way a newer
/// client might: an unknown color tier, output mode, hyperlink flag, and
/// palette presence byte, limits beyond the hard caps, and an appended tail.
fn newer_hello() -> Vec<u8> {
    let hello = encode_frame(&FrameKind::Hello {
        client_name: "newer-client".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor + 9,
        protocol_patch: 0,
        client_caps: ClientCapabilities::new().with_layers(LayerSet::all()),
    });
    let mut fields: Vec<(u32, Vec<u8>)> = Vec::new();
    let mut dec = Decoder::new(&hello[5..]);
    while let Some((id, value)) = dec.read_field().unwrap() {
        fields.push((id, value.to_vec()));
    }
    for (id, value) in &mut fields {
        if *id != field::hello::CLIENT_CAPS {
            continue;
        }
        assert_eq!(value[6], 0, "this build's caps carry no palette");
        value[0] = 0xEE; // ColorSupport
        value[4] = 7; // hyperlinks
        value[5] = 9; // OutputMode
        value[6] = 5; // palette presence, then its six bytes
        value.splice(7..7, [1, 2, 3, 4, 5, 6]);
        let limits = value.len() - 8;
        value[limits..].copy_from_slice(&[0xFF; 8]);
        value.extend_from_slice(&[0xEE; 6]);
    }
    let fields: Vec<(u32, &[u8])> = fields.iter().map(|(id, v)| (*id, v.as_slice())).collect();
    raw_frame(TYPE_HELLO, &fields)
}

#[test]
fn a_hello_with_unknown_capability_values_is_admitted() {
    run_local(async {
        let tmp = tempfile::TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let _server = spawn_server(socket.clone(), None);
        let mut stream = wait_for_raw_socket(&socket, Duration::from_secs(10)).await;
        send_raw(&mut stream, &newer_hello()).await;
        let (_, hello_ok) = recv_typed(&mut stream).await;
        let FrameKind::HelloOk {
            bootstrap_limits, ..
        } = hello_ok
        else {
            panic!("HELLO refused: {hello_ok:?}");
        };
        assert!(bootstrap_limits.max_chunk_bytes() <= MAX_BOOTSTRAP_CHUNK_BYTES);
        assert!(bootstrap_limits.max_history_page_bytes() <= MAX_HISTORY_PAGE_BYTES);

        let state = command(
            &mut stream,
            1,
            Command::GetState {
                scope: StateScope::Server,
            },
        )
        .await;
        assert!(matches!(
            state,
            CommandResult::OkWith(CommandValue::State(_))
        ));
    });
}
