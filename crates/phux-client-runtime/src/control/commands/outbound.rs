//! Encoded frames retain private delivery fences until the shared drain.

use super::super::{ControlPlane, FrameKind, PrivateUploadDestination, encode};

/// Private payloads keep their admission identity until the transport drain.
#[derive(Debug)]
pub(in crate::control) enum OutboundFrame {
    Ordinary(Vec<u8>),
    Private {
        bytes: Vec<u8>,
        destination: PrivateUploadDestination,
        transfer_id: Option<u64>,
    },
}

impl ControlPlane {
    /// Every still-admitted encoded frame, in send order. Private destinations
    /// are revalidated here, under the control lock, before bytes leave the plane.
    pub fn take_outbound(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::with_capacity(self.outbound.len());
        while !self.outbound.is_empty() {
            for frame in std::mem::take(&mut self.outbound) {
                match frame {
                    OutboundFrame::Ordinary(bytes) => frames.push(bytes),
                    OutboundFrame::Private {
                        bytes,
                        destination,
                        transfer_id,
                    } => {
                        if self.validate_private_frame(&destination, transfer_id) {
                            frames.push(bytes);
                        }
                    }
                }
            }
            // A refused private chunk releases the upload queue. Drain the
            // next admitted chunk too, rather than waiting for a reply to bytes
            // that were never sent.
            self.queue_next_upload();
        }
        frames
    }

    /// Extension point: queue any frame, encoded.
    pub fn queue_frame(&mut self, frame: &FrameKind) {
        self.outbound.push(OutboundFrame::Ordinary(encode(frame)));
    }

    pub(in crate::control) fn queue_private_frame(
        &mut self,
        frame: &FrameKind,
        destination: PrivateUploadDestination,
        transfer_id: Option<u64>,
    ) {
        self.outbound.push(OutboundFrame::Private {
            bytes: encode(frame),
            destination,
            transfer_id,
        });
    }
}
