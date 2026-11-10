//! Folds one document resource's frames into documents through the
//! kernel's document lane, so a headless snapshot or follow keeps the same
//! rails as an attached client: exact generation identity, contiguous
//! chunks, bounded staging, tombstones, and forward-only live frames.

use std::convert::Infallible;
use std::time::Instant;

use phux_client_core::session::{DocumentLane, DocumentPublication, KernelError};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{AgentEvent, FrameKind};

use super::{Document, TreeError};
use crate::attach::AttachError;

pub(super) enum Absorbed {
    Document(Document),
    Closed,
    Nothing,
}

/// A bootstrap's chunks make one document, and every live `RESOURCE_OUTPUT`
/// is one. A tombstone fences its generation; the recut's bootstrap carries
/// the newest document.
pub(super) struct Reader {
    tree: ResourceId,
    lane: DocumentLane,
    started: Instant,
}

impl Reader {
    pub(super) fn new(tree: ResourceId) -> Self {
        Self {
            lane: DocumentLane::new(tree.clone()),
            tree,
            started: Instant::now(),
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(super) fn absorb(&mut self, frame: FrameKind) -> Result<Absorbed, TreeError> {
        let now_ms = self.now_ms();
        let result = match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                base_seq,
                ..
            } if terminal_id == self.tree => self
                .lane
                .begin(stream_id, bootstrap_id, profile, base_seq, now_ms)
                .map(|()| Absorbed::Nothing),
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } if terminal_id == self.tree => self
                .lane
                .chunk(stream_id, bootstrap_id, chunk_seq, &payload, now_ms)
                .map(|()| Absorbed::Nothing),
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } if terminal_id == self.tree => self
                .lane
                .ready(stream_id, bootstrap_id, now_ms)
                .map(|cut| Absorbed::Document(document(cut))),
            FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } if terminal_id == self.tree => self
                .lane
                .output(stream_id, bootstrap_id, seq, &bytes)
                .map(|live| Absorbed::Document(document(live))),
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } if terminal_id == self.tree => {
                self.lane
                    .tombstone(stream_id, bootstrap_id, reason, last_valid_seq);
                Ok(Absorbed::Nothing)
            }
            FrameKind::ResourceClosed {
                terminal_id: terminal,
                ..
            }
            | FrameKind::Event {
                terminal: Some(terminal),
                event: AgentEvent::ResourceClosed { .. },
                ..
            } if terminal == self.tree => Ok(Absorbed::Closed),
            FrameKind::Error {
                request_id: Some(_),
                message,
                ..
            } => return Err(TreeError::Refused(message)),
            _ => Ok(Absorbed::Nothing),
        };
        result.or_else(|error| self.refused(&error))
    }

    /// A late frame of a fenced generation is dropped, as an attached client
    /// drops it; any other refusal means the stream broke its contract.
    fn refused(&self, error: &KernelError<Infallible>) -> Result<Absorbed, TreeError> {
        if matches!(error, KernelError::RetiredGeneration { .. }) {
            return Ok(Absorbed::Nothing);
        }
        Err(AttachError::Protocol(format!("document stream {}: {error}", self.tree)).into())
    }
}

fn document(publication: DocumentPublication) -> Document {
    Document {
        seq: publication.seq,
        bytes: publication.bytes,
        content_type: publication.content_type,
    }
}

#[cfg(test)]
mod tests;
