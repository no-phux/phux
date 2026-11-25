//! Terminal-mode encoding and snapshot publication share the process input owner.

use super::TerminalProcess;
use crate::actor::{GhosttyTerminal, InputEncoderSnapshot, PasteOutcome, TerminalInput};
use tracing::warn;

impl TerminalProcess {
    pub(in crate::actor) fn publish_input_snapshot(
        &self,
        terminal: &GhosttyTerminal<'_, '_>,
        cell_px: (u16, u16),
    ) {
        match InputEncoderSnapshot::capture(
            terminal,
            cell_px,
            &mut self.key_enc.borrow_mut(),
            &mut self.mouse_enc.borrow_mut(),
        ) {
            Ok(snapshot) => {
                self.input_snapshot_tx.send_if_modified(|current| {
                    let changed = *current != snapshot;
                    *current = snapshot;
                    changed
                });
            }
            Err(err) => warn!(error = %err, "input encoder snapshot capture failed"),
        }
    }

    pub(in crate::actor) fn encode_input(
        &self,
        input: &TerminalInput,
        terminal: &GhosttyTerminal<'_, '_>,
        cell_px: (u16, u16),
    ) -> Result<Option<Vec<u8>>, libghostty_vt::Error> {
        match input {
            TerminalInput::Key(event) => {
                let mut enc = self.key_enc.borrow_mut();
                let bytes = enc.encode(event, terminal)?;
                Ok(Some(bytes.to_vec()))
            }
            TerminalInput::Mouse(event) => {
                let mut enc = self.mouse_enc.borrow_mut();
                let bytes = enc.encode(event, terminal, cell_px)?;
                Ok(Some(bytes.to_vec()))
            }
            TerminalInput::Focus(event) => {
                let mut enc = self.focus_enc.borrow_mut();
                let bytes = enc.encode(*event, terminal)?;
                Ok(bytes.map(<[u8]>::to_vec))
            }
            TerminalInput::Paste(event) => {
                let mut enc = self.paste_enc.borrow_mut();
                match enc.encode(event, terminal)? {
                    PasteOutcome::Encoded(bytes) => Ok(Some(bytes.to_vec())),
                    PasteOutcome::Rejected => Ok(None),
                }
            }
            TerminalInput::ClipboardReply { selection, text } => Ok(Some(
                crate::ordered_input::clipboard_reply_bytes(*selection, &text.0),
            )),
        }
    }
}
