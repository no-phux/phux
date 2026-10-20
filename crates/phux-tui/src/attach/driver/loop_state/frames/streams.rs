//! QUIC Terminal stream lifecycle and binding.

use super::super::{AttachError, Connection, FrameKind, attach_participants};

impl super::super::SessionLoop {
    /// Apply the QUIC stream lifecycle barriers carried by one control frame.
    /// Returns true when `ATTACH_READY` must be held until every Terminal
    /// stream publishes its own READY/CLOSED outcome.
    pub(in crate::attach::driver::loop_state) async fn coordinate_multistream_frame(
        &mut self,
        conn: &mut Connection,
        frame: &FrameKind,
    ) -> Result<bool, AttachError> {
        self.retire_attach_generation(frame);
        if self.bind_answered_terminal_stream(conn, frame).await? || !conn.multistream_enabled() {
            return Ok(false);
        }
        self.bind_open_multistream_frame(conn, frame).await
    }

    /// A new `ATTACHED` drops the previous generation's binds. A failed
    /// command drops the stream it would have bound.
    fn retire_attach_generation(&mut self, frame: &FrameKind) {
        if matches!(frame, FrameKind::Attached { .. }) {
            self.pending_attach_ready = None;
            // A new aggregate generation: retired replies must not bind
            // streams or fold leaves.
            self.pending_stream_binds.clear();
            // Replies to the retired generation's commands can no longer be
            // attributed; a stale entry would fold a live leaf.
            self.mirror.pending_resource_ops.clear();
        }
        if let FrameKind::Error {
            request_id: Some(request_id),
            ..
        } = frame
        {
            self.pending_stream_binds.remove(request_id);
        }
    }

    /// Bind the Terminal stream a command reply just confirmed.
    /// `Ok(true)` means this frame was that reply and the caller should stop.
    async fn bind_answered_terminal_stream(
        &mut self,
        conn: &mut Connection,
        frame: &FrameKind,
    ) -> Result<bool, AttachError> {
        if let FrameKind::CommandResult { request_id, result } = frame
            && let Some(terminal_id) = self.pending_stream_binds.remove(request_id)
        {
            if conn.multistream_enabled()
                && matches!(result, phux_protocol::wire::frame::CommandResult::Ok)
            {
                conn.bind_terminal(&terminal_id).await?;
                // The bootstrap reflow skipped this pane while it had no
                // stream; size it now that it has one.
                self.bind_reflow_owed = true;
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Bind streams opened by an attach or a local spawn, or report that
    /// `ATTACH_READY` must wait. The caller has established multistream is on.
    async fn bind_open_multistream_frame(
        &self,
        conn: &mut Connection,
        frame: &FrameKind,
    ) -> Result<bool, AttachError> {
        match frame {
            FrameKind::Attached { snapshot, .. } => {
                for terminal_id in attach_participants(snapshot) {
                    conn.bind_terminal(&terminal_id).await?;
                }
            }
            // A local spawn: bind its Terminal stream before the frame's
            // reflow. Satellite spawns bind after their ATTACH_RESOURCE reply.
            FrameKind::ResourceSpawned { request_id, result }
                if self.mirror.pending_splits.contains_key(request_id)
                    || self.mirror.pending_windows.contains_key(request_id)
                    || self.mirror.pending_floating.contains_key(request_id) =>
            {
                if let Some(terminal_id) = result.spawned_id()
                    && terminal_id.is_local()
                {
                    conn.bind_terminal(terminal_id).await?;
                }
            }
            FrameKind::AttachReady { .. } => {
                return Ok(self
                    .mirror
                    .engine_kernel
                    .attach_ready_pending()
                    .is_some_and(|pending| pending > 0));
            }
            _ => {}
        }
        Ok(false)
    }
}
