//! The bounded link write queue and its writer task.

use super::{Cell, LINK_SEND_TIMEOUT, LINK_WRITE_QUEUE_BYTES, LinkWriter, Rc};

pub(super) enum LinkWrite {
    Frames(Vec<Vec<u8>>),
    Keepalive,
    Final(Vec<u8>, tokio::sync::oneshot::Sender<()>),
}

pub(super) struct QueuedLinkWrite {
    pub(super) write: Option<LinkWrite>,
    pub(super) bytes: usize,
    pub(super) total: Rc<Cell<usize>>,
}

impl QueuedLinkWrite {
    pub(super) const fn take_write(&mut self) -> Option<LinkWrite> {
        self.write.take()
    }
}

impl Drop for QueuedLinkWrite {
    fn drop(&mut self) {
        self.total.set(self.total.get().saturating_sub(self.bytes));
    }
}

impl LinkWrite {
    pub(super) fn encoded_bytes(&self) -> usize {
        match self {
            Self::Frames(frames) => frames.iter().map(Vec::len).sum(),
            Self::Keepalive => 0,
            Self::Final(frame, _) => frame.len(),
        }
    }
}

pub(super) fn queue_link_write(
    tx: &tokio::sync::mpsc::Sender<QueuedLinkWrite>,
    queued_bytes: &Rc<Cell<usize>>,
    write: LinkWrite,
) -> Result<(), String> {
    if matches!(&write, LinkWrite::Frames(frames) if frames.is_empty()) {
        return Ok(());
    }
    let bytes = write.encoded_bytes();
    let next_bytes = queued_bytes.get().saturating_add(bytes);
    if next_bytes > LINK_WRITE_QUEUE_BYTES {
        return Err(format!(
            "satellite ordered writer queue exceeded {LINK_WRITE_QUEUE_BYTES} bytes; closing link without dropping accepted requests"
        ));
    }
    queued_bytes.set(next_bytes);
    tx.try_send(QueuedLinkWrite {
        write: Some(write),
        bytes,
        total: Rc::clone(queued_bytes),
    })
    .map_err(|error| match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => {
            "satellite ordered writer queue saturated; closing link without dropping accepted requests".to_owned()
        }
        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
            "satellite ordered writer stopped".to_owned()
        }
    })
}

#[allow(
    clippy::future_not_send,
    reason = "the hub writer runs on ADR-0014's current-thread LocalSet and its byte accounting is Rc-local"
)]
pub(super) async fn drive_link_writer<W: LinkWriter>(
    mut writer: W,
    mut rx: tokio::sync::mpsc::Receiver<QueuedLinkWrite>,
    errors: tokio::sync::mpsc::Sender<String>,
) {
    while let Some(mut queued) = rx.recv().await {
        // Hold `queued` until the write completes so its bytes stay charged.
        let Some(write) = queued.take_write() else {
            continue;
        };
        let result = match write {
            LinkWrite::Frames(frames) => {
                let mut result = Ok(());
                for frame in frames {
                    if let Err(error) = send_bounded(&mut writer, &frame).await {
                        result = Err(error);
                        break;
                    }
                }
                result
            }
            LinkWrite::Keepalive => tokio::time::timeout(LINK_SEND_TIMEOUT, writer.keepalive())
                .await
                .unwrap_or_else(|_| {
                    Err(format!(
                        "keepalive write to satellite stalled for {}s",
                        LINK_SEND_TIMEOUT.as_secs()
                    ))
                }),
            LinkWrite::Final(frame, sent) => {
                let result = send_bounded(&mut writer, &frame).await;
                if result.is_ok() {
                    let _ = sent.send(());
                }
                result
            }
        };
        if let Err(error) = result {
            let _ = errors.send(error).await;
            return;
        }
    }
}

/// Put one frame on the wire, bounded by [`LINK_SEND_TIMEOUT`].
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: runs on the server's LocalSet inside run_relay_session"
)]
pub(super) async fn send_bounded<C: LinkWriter>(conn: &mut C, frame: &[u8]) -> Result<(), String> {
    tokio::time::timeout(LINK_SEND_TIMEOUT, conn.send_frame(frame))
        .await
        .unwrap_or_else(|_elapsed| {
            Err(format!(
                "write to satellite stalled for {}s",
                LINK_SEND_TIMEOUT.as_secs()
            ))
        })
}
