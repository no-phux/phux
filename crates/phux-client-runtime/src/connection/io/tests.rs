//! Link I/O tests.

use super::read::take_complete_frames;
use super::*;
use tokio::io::AsyncReadExt;

#[test]
fn a_rebind_socket_keeps_the_peer_family() {
    let v4 = unspecified_like("127.0.0.1:4433".parse().expect("addr"));
    let v6 = unspecified_like("[::1]:4433".parse().expect("addr"));
    assert!(v4.is_ipv4() && v4.port() == 0);
    assert!(v6.is_ipv6() && v6.port() == 0);
}

#[test]
fn complete_frames_are_delivered_before_a_later_malformed_frame() {
    let mut pending = BytesMut::new();
    FrameKind::Ping { nonce: 7 }.encode(&mut pending);
    pending.extend_from_slice(&[0, 0, 0, 0]);

    let valid = take_complete_frames(&mut pending).expect("valid prefix");

    assert_eq!(valid.len(), 1);
    assert!(take_complete_frames(&mut pending).is_err());
}

#[test]
fn a_stream_read_drains_complete_frames_in_bounded_batches() {
    let mut pending = BytesMut::new();
    for nonce in 0..300 {
        FrameKind::Ping { nonce }.encode(&mut pending);
    }

    let first = take_complete_frames(&mut pending).expect("first batch");
    let second = take_complete_frames(&mut pending).expect("second batch");

    assert_eq!(first.len(), MAX_INBOUND_BATCH);
    assert_eq!(second.len(), 300 - MAX_INBOUND_BATCH);
    assert!(pending.is_empty());
    let (first_frame, tail) = FrameKind::decode(&first[0]).expect("frame decodes");
    assert!(tail.is_empty());
    assert!(matches!(first_frame, FrameKind::Ping { nonce: 0 }));
    let (last_frame, tail) =
        FrameKind::decode(second.last().expect("last frame")).expect("frame decodes");
    assert!(tail.is_empty());
    assert!(matches!(last_frame, FrameKind::Ping { nonce: 299 }));
}

/// A non-blocking read takes what is buffered and never waits; a
/// clean close reads as nothing until the blocking read reports it.
#[tokio::test]
async fn a_non_blocking_read_takes_what_is_there_and_never_waits() {
    let (near, mut far) = tokio::io::duplex(4096);
    let (reader, writer) = tokio::io::split(near);
    let mut io = Io::stream(Box::new(reader), Box::new(writer));
    assert!(io.try_read_frames().expect("nothing yet").is_empty());
    far.write_all(&encode(&FrameKind::Ping { nonce: 4 }))
        .await
        .expect("write");
    let frames = io.try_read_frames().expect("one frame");
    assert_eq!(frames.len(), 1);
    drop(far);
    assert!(io.try_read_frames().expect("closed").is_empty());
    assert!(io.read_frames().await.expect("clean close").is_none());
}

#[test]
fn joined_frames_keep_order() {
    assert_eq!(join_frames(&[]), Vec::<u8>::new());
    assert_eq!(join_frames(&[vec![1, 2], vec![], vec![3]]), vec![1, 2, 3]);
}

#[test]
fn a_websocket_message_budget_grows_with_its_size_and_then_caps() {
    assert_eq!(message_budget(0), SEND_BUDGET);
    assert_eq!(message_budget(SEND_STALL_CHUNK), SEND_BUDGET);
    assert_eq!(
        message_budget(SEND_STALL_CHUNK + 1),
        SEND_BUDGET.saturating_mul(2)
    );
    assert_eq!(message_budget(16 * 1024 * 1024), SEND_BUDGET_CEILING);
}

/// Three chunks, each accepted just inside the budget. The whole write
/// takes longer than one budget, which is what a slow live link does.
#[tokio::test(start_paused = true)]
async fn a_link_that_keeps_taking_chunks_survives_past_one_budget() {
    use futures_util::FutureExt;

    let chunk = SEND_STALL_CHUNK;
    let (mut client, mut peer) = tokio::io::duplex(chunk);
    let payload = vec![7u8; chunk * 3];
    let writing = write_all_within_budget(&mut client, &payload);
    tokio::pin!(writing);

    assert!(
        writing.as_mut().now_or_never().is_none(),
        "the second chunk waits on a full buffer"
    );
    tokio::time::advance(almost_one_budget()).await;
    assert!(
        writing.as_mut().now_or_never().is_none(),
        "the first wait stays inside one budget"
    );
    read_one_chunk(&mut peer, chunk).await;

    assert!(
        writing.as_mut().now_or_never().is_none(),
        "the third chunk waits"
    );
    tokio::time::advance(almost_one_budget()).await;
    assert!(
        writing.as_mut().now_or_never().is_none(),
        "a fresh chunk restarts the budget"
    );
    read_one_chunk(&mut peer, chunk).await;

    let end = writing
        .as_mut()
        .now_or_never()
        .expect("the last chunk fits once the peer reads");
    assert!(end.is_ok(), "{end:?}");
}

fn almost_one_budget() -> Duration {
    SEND_BUDGET
        .checked_sub(Duration::from_secs(1))
        .expect("the send budget is longer than one second")
}

async fn read_one_chunk(peer: &mut tokio::io::DuplexStream, n: usize) {
    let mut buf = vec![0u8; n];
    peer.read_exact(&mut buf)
        .await
        .expect("the chunk is buffered");
}
