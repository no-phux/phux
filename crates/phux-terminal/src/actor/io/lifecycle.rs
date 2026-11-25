use super::{TerminalActor, debug};

impl TerminalActor {
    /// React to PTY EOF: stop reading, reap the child, record the exit
    /// facet. The actor stays alive for late `SnapshotRequest`s and orderly
    /// shutdown; the run loop fires `exit_notify` after flushing gap
    /// resyncs. The runtime decides whether to close the pane or respawn a
    /// shell.
    #[allow(clippy::future_not_send, reason = "terminal actor runs on a LocalSet")]
    pub(in crate::actor) async fn handle_pty_eof(&mut self) {
        debug!("PTY EOF; recording exit and keeping actor alive for a final resync flush");
        let exit = self.process.observe_eof().await;
        // The program quit on its own. A signal may be the server or the
        // host going down, which is what the token is kept for.
        if exit.status.is_some() {
            self.resume.forget();
        }
        if self.program_status.expire_active() {
            self.publish_program_status();
        }
    }

    /// Return only an in-flight loan for EOF snapshots. Completed publication
    /// fences and history cuts still belong to this incarnation until close.
    pub(in crate::actor) fn land_native_capture(&mut self) {
        if let Some(pending) = self.publication.pending_native_bootstrap.take() {
            self.abort_native_bootstrap(pending, crate::native_state::NativeStateError::Resize);
        }
    }

    /// Fail any in-flight native bootstrap and tombstone every cursor,
    /// returning the terminal to the manager. The teardown paths run outside
    /// the `!bootstrap_pending` guards, so they land the cut instead of
    /// touching a loaned terminal. EOF does not retire an incarnation and
    /// uses `land_native_capture` instead.
    pub(in crate::actor) fn land_native_cuts(&mut self) {
        let _ =
            self.invalidate_all_native_cursors(phux_protocol::wire::frame::TombstoneReason::Other);
    }

}
