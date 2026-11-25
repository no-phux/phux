//! ADR-0158: an OSC 52 write from a pane's output. Only the focused pane may
//! write; `defaults.clipboard-write` allows, asks, or denies. The text never
//! reaches a log.

use std::time::Instant;

use phux_config::{ClipboardRead, ClipboardWrite};
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
                ClipboardWrite::Ask if self.overlays.asking() => {
                    tracing::info!(terminal_id = ?pane, "clipboard write dropped: another question is up");
                }
                ClipboardWrite::Ask => {
                    let source = crate::attach::server_frame::pane_label(&pane);
                    self.overlays.push(Box::new(
                        crate::render::overlay::ConfirmOverlay::clipboard(
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
            let policy = self.settings.clipboard_read;
            // A read that would ask while another question is up is dropped.
            let held = policy == ClipboardRead::Ask && self.overlays.asking();
            let focused = !held && self.mirror.focused_resource.as_ref() == Some(&pane);
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
                    if let Some(title) = self.pane_title(&pane) {
                        label = format!("{label} ({title})");
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

    /// What a held pane calls itself, for a question about it.
    pub(in crate::attach::driver::loop_state) fn pane_title(
        &self,
        pane: &phux_protocol::ResourceId,
    ) -> Option<String> {
        let slot = self.mirror.panes.get(pane)?;
        phux_client_core::pane_label::pane_label(
            None,
            Some(slot.last_title.as_str()),
            slot.foreground.as_deref(),
            slot.cwd.as_deref(),
        )
        .filter(|title| !title.is_empty())
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
