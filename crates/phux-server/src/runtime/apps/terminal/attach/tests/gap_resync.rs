//! Gap resync batching, timeouts, and exit ordering.
#![allow(
    clippy::wildcard_imports,
    reason = "the attach test topics share the parent test namespace"
)]

use super::*;

fn final_screen_frames(terminal_id: &phux_protocol::ids::ResourceId) -> Vec<FrameKind> {
    synthesized_bootstrap_frames(
        terminal_id.clone(),
        phux_protocol::ids::StreamId::new(1).expect("stream id"),
        phux_protocol::ids::BootstrapId::new(2).expect("bootstrap id"),
        BootstrapStreamProfile::SynthesizedVtRaw,
        BootstrapLimits::default(),
        80,
        24,
        9,
        [bytes::Bytes::from_static(b"FINAL_SCREEN")],
    )
    .expect("bootstrap frames")
}

async fn poll_pending<F: std::future::Future>(mut future: std::pin::Pin<&mut F>) {
    std::future::poll_fn(|cx| {
        assert!(
            future.as_mut().poll(cx).is_pending(),
            "future completed before its barrier"
        );
        std::task::Poll::Ready(())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn gap_resync_waits_for_the_entire_replacement_batch() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    for nonce in 0..8 {
        tx.try_send(Outbound::Frame(FrameKind::Pong { nonce }))
            .unwrap();
    }
    let frames = final_screen_frames(&phux_protocol::ids::ResourceId::local(1));
    let count = frames.len();
    let mut queued = Box::pin(queue_resync_bootstrap(
        &tx,
        crate::terminal_actor::ResyncReason::OutboundGap,
        frames.clone(),
        true,
    ));
    poll_pending(queued.as_mut()).await;
    for _ in 0..count - 1 {
        rx.recv().await.unwrap();
    }
    poll_pending(queued.as_mut()).await;
    rx.recv().await.unwrap();
    assert_eq!(queued.await, SnapshotQueue::Queued);
    for _ in 0..8 - count {
        assert!(matches!(
            rx.recv().await,
            Some(Outbound::Frame(FrameKind::Pong { .. }))
        ));
    }
    for expected in frames {
        let Some(Outbound::Frame(actual)) = rx.recv().await else {
            panic!("missing replacement frame")
        };
        assert_eq!(actual, expected);
    }
}

#[tokio::test(start_paused = true)]
async fn a_gap_timeout_publishes_no_partial_generation() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    for nonce in 0..8 {
        tx.try_send(Outbound::Frame(FrameKind::Pong { nonce }))
            .unwrap();
    }
    let frames = final_screen_frames(&phux_protocol::ids::ResourceId::local(1));
    let mut queued = Box::pin(queue_resync_bootstrap(
        &tx,
        crate::terminal_actor::ResyncReason::OutboundGap,
        frames,
        true,
    ));
    poll_pending(queued.as_mut()).await;
    rx.recv().await.unwrap();
    tokio::time::advance(GAP_RESYNC_QUEUE_WAIT).await;
    assert_eq!(queued.await, SnapshotQueue::Deferred);
    for nonce in 1..8 {
        let Some(Outbound::Frame(FrameKind::Pong { nonce: actual })) = rx.recv().await else {
            panic!("timeout leaked a replacement frame");
        };
        assert_eq!(actual, nonce);
    }
    assert_eq!(tx.capacity(), 8, "timeout kept reserved permits");
    assert!(rx.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_gap_wait_returns_its_partial_reservation() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    for nonce in 0..8 {
        tx.try_send(Outbound::Frame(FrameKind::Pong { nonce }))
            .unwrap();
    }
    let frames = final_screen_frames(&phux_protocol::ids::ResourceId::local(1));
    let mut queued = Box::pin(queue_resync_bootstrap(
        &tx,
        crate::terminal_actor::ResyncReason::OutboundGap,
        frames,
        true,
    ));
    poll_pending(queued.as_mut()).await;
    rx.recv().await.unwrap();
    poll_pending(queued.as_mut()).await;
    drop(queued);
    assert_eq!(tx.capacity(), 1, "cancelled reservation kept a permit");
    tx.try_send(Outbound::Frame(FrameKind::Pong { nonce: 8 }))
        .unwrap();
    for nonce in 1..=8 {
        let Some(Outbound::Frame(FrameKind::Pong { nonce: actual })) = rx.recv().await else {
            panic!("cancellation leaked a replacement frame");
        };
        assert_eq!(actual, nonce);
    }
    assert!(rx.try_recv().is_err());
}

/// phux-fpgl.28: a lagged mailbox is full. A gap resync must not park on
/// it, and the exit snapshot's chunk — the final screen — must be queued
/// ahead of `RESOURCE_CLOSED`, not split off after `BOOTSTRAP_BEGIN`.
#[tokio::test(flavor = "current_thread")]
async fn exit_resync_on_a_full_mailbox_precedes_close() {
    use phux_protocol::wire::frame::CloseReason;

    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<Outbound>(crate::mailbox::DEFAULT_CLIENT_MAILBOX);
    for nonce in 0..u64::try_from(tx.max_capacity()).expect("mailbox depth fits u64") {
        tx.try_send(Outbound::Frame(FrameKind::Pong { nonce }))
            .expect("fill the lagged mailbox");
    }
    let terminal_id = phux_protocol::ids::ResourceId::local(1);
    let frames = final_screen_frames(&terminal_id);
    assert!(frames.len() > 1 && frames.len() <= tx.max_capacity());

    let deferred = queue_resync_bootstrap(
        &tx,
        crate::terminal_actor::ResyncReason::OutboundGap,
        frames.clone(),
        true,
    )
    .await;
    assert_eq!((deferred, tx.capacity()), (SnapshotQueue::Deferred, 0));

    // Park the exit reservation on this task before close asks for a
    // slot. `yield_now` does not order two spawned waiters, and the
    // first one in the semaphore queue takes every freed slot.
    let exit = queue_resync_bootstrap(&tx, crate::terminal_actor::ResyncReason::Exit, frames, true);
    tokio::pin!(exit);
    std::future::poll_fn(|cx| match std::future::Future::poll(exit.as_mut(), cx) {
        std::task::Poll::Pending => std::task::Poll::Ready(()),
        std::task::Poll::Ready(queue) => {
            panic!("exit bootstrap finished on a full mailbox: {queue:?}")
        }
    })
    .await;
    let close = tx.send(Outbound::Frame(FrameKind::ResourceClosed {
        terminal_id,
        exit_status: Some(0),
        reason: CloseReason::Exited,
        signal: None,
    }));
    tokio::pin!(close);
    tokio::select! {
        biased;
        queue = &mut exit => panic!("exit bootstrap finished before any drain: {queue:?}"),
        result = &mut close => {
            panic!("close took a slot ahead of the exit bootstrap: {result:?}")
        }
        () = std::future::ready(()) => {}
    }
    assert_eq!(tx.capacity(), 0);

    let mut saw_chunk = false;
    let mut saw_ready = false;
    let mut exit_queued = false;
    let mut close_queued = false;
    loop {
        tokio::select! {
            biased;
            queue = &mut exit, if !exit_queued => {
                assert_eq!(queue, SnapshotQueue::Queued);
                exit_queued = true;
            }
            result = &mut close, if !close_queued => {
                result.expect("queue RESOURCE_CLOSED");
                close_queued = true;
            }
            message = rx.recv() => {
                match message {
                    Some(Outbound::Frame(FrameKind::BootstrapChunk { payload, .. })) => {
                        assert_eq!(payload.as_ref(), b"FINAL_SCREEN");
                        saw_chunk = true;
                    }
                    Some(Outbound::Frame(FrameKind::BootstrapReady { .. })) => {
                        assert!(saw_chunk, "READY arrived before the final chunk");
                        saw_ready = true;
                    }
                    Some(Outbound::Frame(FrameKind::ResourceClosed { .. })) => break,
                    Some(_) => {}
                    None => panic!("mailbox closed before RESOURCE_CLOSED"),
                }
            }
        }
    }
    assert!(
        saw_chunk && saw_ready && exit_queued && close_queued,
        "RESOURCE_CLOSED split the final screen off the bootstrap"
    );
}
