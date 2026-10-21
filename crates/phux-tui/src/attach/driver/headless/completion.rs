//! When a headless attach has settled: ready, history, and the layout read.

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
    pending_layout: Option<u32>,
}

impl HeadlessCompletion {
    pub(in crate::attach::driver) fn new(pending_layout: Option<u32>) -> Self {
        Self {
            pending_layout,
            ..Self::default()
        }
    }

    pub(in crate::attach::driver) fn observe_frame(&mut self, frame: &FrameKind, attach_id: u32) {
        match frame {
            FrameKind::AttachReady {
                attach_id: ready_id,
            } if *ready_id == attach_id => self.attach_ready = true,
            // Any answer settles the outstanding request. A page with a next
            // cursor stays pending only if the engine asks for that cursor
            // (`note_history_request` re-arms it): the client pulls history
            // lazily (ADR-0119) and stops once its prefetch window is full,
            // so waiting for the chain's end would time out on deep history.
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
            FrameKind::MetadataValue { request_id, .. }
                if self.pending_layout == Some(*request_id) =>
            {
                self.pending_layout = None;
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
        self.attach_ready
            && self.pending_history.is_empty()
            && self.pending_layout.is_none()
            && agent_metadata_complete
    }
}
