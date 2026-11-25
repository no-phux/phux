//! Test-only coordination at the actual owner-thread batch application cut.

use std::sync::mpsc::{self, Receiver, Sender};

use super::{Command, EngineError, EngineHandle};

#[derive(Default)]
pub(super) struct ApplyGate(Option<(Sender<()>, Receiver<()>)>);

impl ApplyGate {
    pub(super) fn arm(&mut self, entered: Sender<()>, resume: Receiver<()>) {
        self.0 = Some((entered, resume));
    }

    pub(super) fn wait(&mut self) {
        if let Some((entered, resume)) = self.0.take() {
            let _ = entered.send(());
            let _ = resume.recv();
        }
    }
}

impl EngineHandle {
    /// Pause the next actual batch, without replacing the production executor.
    pub(crate) fn pause_next_apply_for_test(
        &self,
    ) -> Result<(Receiver<()>, Sender<()>), EngineError> {
        let (entered, waiting) = mpsc::channel();
        let (resume, released) = mpsc::channel();
        Command::PauseNextApply(entered, released).send(&self.commands)?;
        Ok((waiting, resume))
    }
}
