//! ADR-0158 delivery: what an OSC 52 write from a pane's live output does in
//! this client. Only the focused pane may write; `defaults.clipboard-write`
//! then sets the host clipboard (`allow`), asks first (`ask`), or drops it
//! (`deny`). The text never reaches a log.

use phux_config::ClipboardWrite;

use super::super::{AttachError, FrameOutcome};

impl super::super::SessionLoop {
    /// Apply the frame's clipboard writes, emptying them from `outcome`.
    pub(super) fn deliver_clipboard_writes<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        outcome: &mut FrameOutcome,
    ) -> Result<(), AttachError> {
        for (pane, text) in std::mem::take(&mut outcome.clipboard_writes) {
            if self.mirror.focused_resource.as_ref() != Some(&pane) {
                tracing::debug!(terminal_id = ?pane, "clipboard write from an unfocused pane dropped");
                continue;
            }
            match self.settings.clipboard_write {
                ClipboardWrite::Allow => crate::attach::copy::write_host_clipboard(out, &text.0)?,
                ClipboardWrite::Ask => {
                    let source = crate::attach::server_frame::pane_label(&pane);
                    self.overlays.push(Box::new(
                        crate::render::overlay::ClipboardConfirmOverlay::new(
                            &source,
                            text,
                            &self.settings.theme,
                        ),
                    ));
                    outcome.chrome_dirty = true;
                }
                ClipboardWrite::Deny => {
                    tracing::info!(terminal_id = ?pane, bytes = text.0.len(), "clipboard write denied");
                }
            }
        }
        Ok(())
    }
}
