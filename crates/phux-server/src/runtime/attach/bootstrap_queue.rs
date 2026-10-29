//! Synthesized bootstrap frames and contiguous queueing onto a consumer.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn synthesized_bootstrap_frames(
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    limits: BootstrapLimits,
    cols: u16,
    rows: u16,
    base_seq: u64,
    payloads: impl IntoIterator<Item = bytes::Bytes>,
) -> Result<Vec<FrameKind>, ()> {
    let mut frames = Vec::new();
    frames.try_reserve(2).map_err(|_| ())?;
    frames.push(FrameKind::BootstrapBegin {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        profile,
        cols,
        rows,
        base_seq,
    });
    let max_chunk = usize::try_from(limits.max_chunk_bytes()).map_err(|_| ())?;
    if max_chunk == 0 {
        return Err(());
    }
    let mut chunk_seq = 0_u32;
    for payload in payloads {
        let mut offset = 0_usize;
        while offset < payload.len() {
            let end = offset.saturating_add(max_chunk).min(payload.len());
            frames.try_reserve(1).map_err(|_| ())?;
            frames.push(FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload: payload.slice(offset..end),
            });
            chunk_seq = chunk_seq.checked_add(1).ok_or(())?;
            offset = end;
        }
    }
    frames.try_reserve(1).map_err(|_| ())?;
    frames.push(FrameKind::BootstrapReady {
        terminal_id,
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    Ok(frames)
}

/// How a resync bootstrap met the consumer mailbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SnapshotQueue {
    /// Every bootstrap frame is queued in generation order.
    Queued,
    /// The mailbox is full and this is not the final grid: the pump stays
    /// fenced and keeps draining, so a later exit snapshot still reaches it.
    Deferred,
    /// The consumer is gone.
    Closed,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn send_synthesized_bootstrap(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
    profile: BootstrapStreamProfile,
    limits: BootstrapLimits,
    cols: u16,
    rows: u16,
    base_seq: u64,
    payloads: impl IntoIterator<Item = bytes::Bytes>,
) -> Result<(), ()> {
    let frames = synthesized_bootstrap_frames(
        terminal_id,
        stream_id,
        bootstrap_id,
        profile,
        limits,
        cols,
        rows,
        base_seq,
        payloads,
    )?;
    send_frames_contiguously(out_tx, &frames).await
}

/// Queue a replacement atomically when it fits the mailbox. A fitting gap
/// replacement gets a bounded wait before deferring without publishing a prefix.
/// Final generations always wait; natural close waits for the pump to finish.
/// Batches larger than the mailbox stream frame by frame and cannot be deferred
/// once publication begins.
pub(crate) async fn queue_resync_bootstrap(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    reason: crate::terminal_actor::ResyncReason,
    frames: Vec<FrameKind>,
    defer_if_full: bool,
) -> SnapshotQueue {
    if frames.is_empty() {
        return SnapshotQueue::Queued;
    }
    let fits = frames.len() <= out_tx.max_capacity();
    let must_deliver =
        !fits || matches!(reason, crate::terminal_actor::ResyncReason::Exit) || !defer_if_full;
    if !must_deliver {
        return match try_queue_frames(out_tx, &frames) {
            Ok(()) => SnapshotQueue::Queued,
            Err(QueueFramesError::Full)
                if matches!(reason, crate::terminal_actor::ResyncReason::OutboundGap) =>
            {
                wait_for_queue_room(out_tx, &frames).await
            }
            Err(QueueFramesError::Full) => SnapshotQueue::Deferred,
            Err(QueueFramesError::Closed) => SnapshotQueue::Closed,
        };
    }
    match try_queue_frames(out_tx, &frames) {
        Ok(()) => SnapshotQueue::Queued,
        Err(QueueFramesError::Closed) => SnapshotQueue::Closed,
        Err(QueueFramesError::Full) => match send_frames_contiguously(out_tx, &frames).await {
            Ok(()) => SnapshotQueue::Queued,
            Err(()) => SnapshotQueue::Closed,
        },
    }
}

enum QueueFramesError {
    Full,
    Closed,
}

/// A fitting gap batch waits for capacity for at most this interval. Timeout
/// releases all reservations without publishing any replacement frames.
pub(super) const GAP_RESYNC_QUEUE_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// Queue a gap resync that found the mailbox full, giving the consumer one
/// bounded window to drain room for it. Returns [`SnapshotQueue::Deferred`]
/// if the window closes first, so the caller keeps its existing retry path.
async fn wait_for_queue_room(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    frames: &[FrameKind],
) -> SnapshotQueue {
    match tokio::time::timeout(
        GAP_RESYNC_QUEUE_WAIT,
        send_frames_contiguously(out_tx, frames),
    )
    .await
    {
        Ok(Ok(())) => SnapshotQueue::Queued,
        Ok(Err(())) => SnapshotQueue::Closed,
        Err(_elapsed) => SnapshotQueue::Deferred,
    }
}

/// Reserve every frame, then send them. A later `send` cannot take a slot
/// in the middle of the bootstrap.
fn try_queue_frames(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    frames: &[FrameKind],
) -> Result<(), QueueFramesError> {
    if frames.is_empty() {
        return Ok(());
    }
    if frames.len() > out_tx.max_capacity() {
        return Err(QueueFramesError::Full);
    }
    let mut permits = out_tx
        .try_reserve_many(frames.len())
        .map_err(|err| match err {
            tokio::sync::mpsc::error::TrySendError::Full(()) => QueueFramesError::Full,
            tokio::sync::mpsc::error::TrySendError::Closed(()) => QueueFramesError::Closed,
        })?;
    for frame in frames {
        let Some(permit) = permits.next() else {
            return Err(QueueFramesError::Closed);
        };
        permit.send(Outbound::Frame(frame.clone()));
    }
    Ok(())
}

async fn send_frames_contiguously(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    frames: &[FrameKind],
) -> Result<(), ()> {
    if frames.is_empty() {
        return Ok(());
    }
    if frames.len() <= out_tx.max_capacity() {
        let Ok(mut permits) = out_tx.reserve_many(frames.len()).await else {
            return Err(());
        };
        for frame in frames {
            let Some(permit) = permits.next() else {
                return Err(());
            };
            permit.send(Outbound::Frame(frame.clone()));
        }
        return Ok(());
    }
    for frame in frames {
        out_tx
            .send(Outbound::Frame(frame.clone()))
            .await
            .map_err(|_| ())?;
    }
    Ok(())
}

/// Queue the in-band resync after a broadcast gap, addressed to `pump` alone
/// so other consumers keep their generation. The pump forwards nothing until
/// the actor accepts it; a closed or persistently full mailbox fails
/// boundedly.
pub(crate) async fn enqueue_output_resync(resize: &ResizeSender, pump: ResyncTarget) -> bool {
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            resize.send(ResizeRequest {
                cols: 0,
                rows: 0,
                cell_px: None,
                resync_clients: true,
                resync_only: true,
                resync_for: Some(pump),
            }),
        )
        .await,
        Ok(Ok(()))
    )
}
