//! When a headless attach has settled: ready and history.

use super::{FrameKind, HashSet, ResourceId};

pub(in crate::attach::driver) type HeadlessHistoryGeneration = (
    ResourceId,
    phux_protocol::StreamId,
    phux_protocol::BootstrapId,
);

#[derive(Debug, Default)]
pub(in crate::attach::driver) struct HeadlessCompletion {
    attach_ready: bool,
    pending_history: HashSet<HeadlessHistoryGeneration>,
}

impl HeadlessCompletion {
    pub(in crate::attach::driver) fn observe_frame(&mut self, frame: &FrameKind, attach_id: u32) {
        match frame {
            FrameKind::AttachReady {
                attach_id: ready_id,
            } if *ready_id == attach_id => self.attach_ready = true,
            // Any answer settles the request. History is pulled lazily
            // (ADR-0119), so a page stays pending only if the engine asks
            // for its next cursor.
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => {
                self.pending_history
                    .remove(&(terminal_id.clone(), *stream_id, *bootstrap_id));
            }
            _ => {}
        }
    }

    pub(in crate::attach::driver) fn note_history_request(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: phux_protocol::StreamId,
        bootstrap_id: phux_protocol::BootstrapId,
    ) {
        self.pending_history
            .insert((terminal_id.clone(), stream_id, bootstrap_id));
    }
    pub(in crate::attach::driver) fn restart_attach(&mut self) {
        self.attach_ready = false;
        self.pending_history.clear();
    }

    pub(in crate::attach::driver) fn is_complete(&self, agent_metadata_complete: bool) -> bool {
        self.attach_ready && self.pending_history.is_empty() && agent_metadata_complete
    }
}
