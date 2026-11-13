//! ADR-0158 delivery: what an OSC 52 write from a pane's live output does in
//! this client. Only the focused pane may write; `defaults.clipboard-write`
//! then sets the host clipboard (`allow`), asks first (`ask`), or drops it
//! (`deny`). The text never reaches a log. Reads follow
//! `defaults.clipboard-read` ([`crate::attach::clipboard_read`]).

use std::time::Instant;

use phux_config::ClipboardWrite;
use phux_protocol::input::paste::ClipboardData;

use super::super::{AttachError, Connection, FrameKind, FrameOutcome};
use crate::attach::clipboard_read::{ClipboardReadOverlay, ReadAction};

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

    /// Apply `defaults.clipboard-read` to the frame's reads: only the
    /// focused pane may read; the rest are dropped without a prompt.
    pub(super) fn deliver_clipboard_reads<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        outcome: &mut FrameOutcome,
    ) -> Result<(), AttachError> {
        for (pane, selection) in std::mem::take(&mut outcome.clipboard_reads) {
            let focused = self.mirror.focused_resource.as_ref() == Some(&pane);
            let policy = self.settings.clipboard_read;
            let action = self.clipboard_reads.borrow_mut().on_read(
                &pane,
                selection,
                focused,
                policy,
                Instant::now(),
            );
            match action {
                ReadAction::Drop => {
                    tracing::debug!(terminal_id = ?pane, ?policy, focused, "clipboard read dropped");
                }
                ReadAction::Ask => {
                    let mut label = crate::attach::server_frame::pane_label(&pane);
                    if let Some(slot) = self.mirror.panes.get(&pane)
                        && !slot.last_title.is_empty()
                    {
                        label = format!("{label} ({})", slot.last_title);
                    }
                    self.overlays.push(Box::new(ClipboardReadOverlay::new(
                        pane,
                        selection,
                        &label,
                        std::rc::Rc::clone(&self.clipboard_reads),
                        &self.settings.theme,
                    )));
                    outcome.chrome_dirty = true;
                }
                ReadAction::Query => {}
            }
        }
        self.flush_clipboard_query(out)
    }

    /// Ask the host terminal for its clipboard for an allowed read.
    pub(in crate::attach::driver::loop_state) fn flush_clipboard_query<
        W: crate::attach::RenderSink,
    >(
        &self,
        out: &mut W,
    ) -> Result<(), AttachError> {
        let query = self.clipboard_reads.borrow_mut().take_query(Instant::now());
        if let Some(query) = query {
            out.write_all(&query)?;
            out.flush()?;
        }
        Ok(())
    }

    /// The host terminal answered a query: send it to the pane that asked.
    pub(in crate::attach::driver::loop_state) async fn send_clipboard_reply(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        let Some(base64) = self.parser.take_clipboard_reply() else {
            return Ok(());
        };
        let reply = self.clipboard_reads.borrow_mut().take_reply(
            &base64,
            self.mirror.focused_resource.as_ref(),
            Instant::now(),
        );
        if let Some((terminal_id, selection, text)) = reply {
            conn.send(&FrameKind::ClipboardReply {
                terminal_id,
                selection: selection.byte(),
                text: ClipboardData(text),
            })
            .await?;
        }
        Ok(())
    }
}
