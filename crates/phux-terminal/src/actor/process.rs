//! One owner for PTY admission, EOF observation, retained identity and teardown.
//! The writer's existing fast gate remains the only input ordering point; this
//! owner never inserts a channel between the input lane and that gate.

use std::cell::{Cell, RefCell};

use tokio::sync::{mpsc, watch};

use super::{
    EncodedInputRequest, HangupPolicy, InputEncoderSnapshot, PerTerminalFocusEncoder,
    PerTerminalKeyEncoder, PerTerminalMouseEncoder, PerTerminalPasteEncoder, PtyEvent, PtyOwned,
    TerminalInput, WriteCompletion, debug, error, warn,
};

pub(super) struct TerminalProcess {
    pub(super) input_rx: mpsc::Receiver<TerminalInput>,
    pub(super) encoded_input_rx: mpsc::Receiver<EncodedInputRequest>,
    pub(super) key_enc: RefCell<PerTerminalKeyEncoder>,
    pub(super) mouse_enc: RefCell<PerTerminalMouseEncoder>,
    pub(super) focus_enc: RefCell<PerTerminalFocusEncoder>,
    pub(super) paste_enc: RefCell<PerTerminalPasteEncoder>,
    pub(super) input_snapshot_tx: watch::Sender<InputEncoderSnapshot>,
    pub(super) last_input_at: Cell<Option<std::time::Instant>>,
    pub(super) last_output_at: Cell<Option<std::time::Instant>>,
    pub(super) prompt: super::effects::PromptTracker,
    pub(super) child_start_ms: Option<u64>,
    pub(super) released_child_pid: Option<i32>,
    pub(super) exit: Option<phux_core::process::ProcessExit>,
    pub(super) lifecycle: phux_protocol::wire::frame::ResourceLifecycle,
    pub(super) hangup: HangupPolicy,
    /// Failed kernel geometry is retryable until this process retires.
    pub(super) resize_pending: bool,
    // Only lifecycle methods replace these together. EOF closes input admission
    // before waiting for the child, even while retained output stays queryable.
    pub(super) pty: Option<PtyOwned>,
    pub(super) pty_rx: Option<mpsc::Receiver<PtyEvent>>,
    pub(super) pty_tx: Option<mpsc::Sender<EncodedInputRequest>>,
}

impl TerminalProcess {
    /// Forward encoded input at the existing writer ordering gate. Nothing
    /// accepted after an observed EOF can reach the process or jump an input
    /// operation whose delivery is still pending.
    pub(super) fn submit(&self, request: EncodedInputRequest) {
        if request.bytes.is_empty() && request.completion.is_none() {
            return;
        }
        if self.exit.is_some() {
            Self::not_written(request);
            return;
        }
        let echo_probe = request.echo_probe;
        let len = request.bytes.len();
        let queued = if let Some(pty) = &self.pty {
            pty.fast.enqueue(request)
        } else if let Some(tx) = &self.pty_tx {
            super::try_send_to_writer(tx, request)
        } else {
            debug!("no PTY; encoded input discarded");
            Self::not_written(request);
            return;
        };
        match queued {
            Ok(()) => {
                crate::perf::INPUT_EVENTS.incr();
                if echo_probe
                    && self
                        .last_output_at
                        .get()
                        .is_none_or(|at| at.elapsed() >= crate::perf::ECHO_QUIET_WINDOW)
                {
                    self.last_input_at.set(Some(std::time::Instant::now()));
                }
                debug!(len, "input queued to PTY writer");
            }
            Err(mpsc::error::TrySendError::Full(request)) => {
                warn!(
                    len,
                    credited = request.credit.is_some(),
                    "PTY writer queue full; dropping input"
                );
                Self::not_written(request);
            }
            Err(mpsc::error::TrySendError::Closed(request)) => {
                error!(len, "PTY writer channel closed; pane input is dead");
                Self::not_written(request);
            }
        }
    }

    fn not_written(request: EncodedInputRequest) {
        if let Some(completion) = request.completion {
            completion.complete(WriteCompletion::NotWritten);
        }
    }

    /// Close both direct and queued admission before observing process exit.
    /// Already-written input keeps its original completion; unwritten work is
    /// settled by the writer, never silently replayed against another process.
    pub(super) fn close_input(&mut self) {
        self.resize_pending = false;
        self.pty_rx = None;
        drop(self.pty_tx.take());
        if let Some(pty) = &self.pty {
            pty.fast.close();
        }
        self.input_rx.close();
        self.encoded_input_rx.close();
        while let Ok(request) = self.encoded_input_rx.try_recv() {
            Self::not_written(request);
        }
        while self.input_rx.try_recv().is_ok() {}
    }

    /// Whether the tty's foreground process is a same-uid direct child of this pane's shell.
    pub(super) fn reporter_trusted(&self) -> bool {
        let Some(pty) = self.pty.as_ref() else {
            return false;
        };
        let Some(shell) = pty
            .child
            .process_id()
            .and_then(|pid| i32::try_from(pid).ok())
        else {
            return false;
        };
        let Some(master) = pty.master.lock().ok().and_then(|master| master.as_raw_fd()) else {
            return false;
        };
        super::program_resume::reporter_trusted(shell, master)
    }
}

mod input;
mod lifecycle;
