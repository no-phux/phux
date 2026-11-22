//! An in-memory PTY for tests, in this crate's and in the host's.

use super::{CanonicalTerminal, EncodedInputRequest, PtyEvent, TerminalActor, mpsc};

/// Depth of the test PTY channel: deep enough to pre-queue a whole burst
/// before spawning the actor.
const TEST_PTY_CHANNEL_DEPTH: usize = 512;

impl TerminalActor {
    /// Install in-memory PTY channels on a PTY-less actor: inject output on
    /// the returned sender, observe encoded input on the receiver.
    ///
    /// # Panics
    ///
    /// When the terminal refuses the writer effect; fixtures fail loudly.
    #[allow(clippy::expect_used, reason = "test fixture")]
    pub fn install_test_pty_channels(
        &mut self,
    ) -> (mpsc::Sender<PtyEvent>, mpsc::Receiver<EncodedInputRequest>) {
        let (evt_tx, evt_rx) = mpsc::channel::<PtyEvent>(TEST_PTY_CHANNEL_DEPTH);
        let (writer_tx, writer_rx) = mpsc::channel::<EncodedInputRequest>(super::PTY_WRITER_QUEUE);
        if let CanonicalTerminal::Plain(Some(terminal)) = &mut *self.terminal.borrow_mut() {
            Self::install_pty_writer(terminal, &writer_tx, None).expect("pty writer");
        }
        self.pty_rx = Some(evt_rx);
        self.pty_tx = Some(writer_tx);
        (evt_tx, writer_rx)
    }
}
