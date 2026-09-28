//! SPEC §5: a frame `length` outside `1..=16_777_216` earns
//! `ERROR { FRAME_TOO_LARGE }` (uncorrelated, naming the length), then the
//! §14 fatal-close tail `DETACHED { PROTOCOL_ERROR }` and EOF.

use std::time::Duration;

use phux_protocol::wire::frame::{ErrorCode, FrameKind, MAX_FRAME_LEN};
use tokio::io::AsyncWriteExt;

use phux_server_testkit::{
    expect_protocol_error_close, recv_typed, run_local, spawn_server_connected,
};

#[test]
fn out_of_range_frame_lengths_get_error_then_close() {
    run_local(async {
        let (server, _first) = spawn_server_connected(None).await;
        // One past the cap, zero (no room for the type byte), and all-ones.
        for length in [MAX_FRAME_LEN + 1, 0, u32::MAX] {
            let mut stream = server.connect().await;
            stream.write_all(&length.to_be_bytes()).await.unwrap();
            stream.flush().await.unwrap();
            let (_, frame) = recv_typed(&mut stream).await;
            let FrameKind::Error {
                request_id,
                code,
                message,
            } = frame
            else {
                panic!("length {length}: expected ERROR, got {frame:?}");
            };
            assert_eq!(request_id, None, "a framing violation is uncorrelated");
            assert_eq!(code, ErrorCode::FrameTooLarge);
            assert_eq!(code.as_wire(), 4);
            assert!(message.contains(&length.to_string()), "{message:?}");
            expect_protocol_error_close(&mut stream, Duration::from_secs(5)).await;
        }
    });
}
