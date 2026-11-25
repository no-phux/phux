//! The forwarded ssh-agent: its offer, its questions, its replies.

use phux_protocol::forward::ForwardOp;

use super::{
    AttachError, Command, Connection, FrameKind, Notice, RepaintAccumulator, RepaintLevel,
    SidebarReservation, Step,
};
use crate::attach::agent_forward::{AgentSignOverlay, Due};

impl super::SessionLoop {
    /// Serve `link` for as long as this connection lasts.
    pub(in super::super) fn set_agent_link(
        &mut self,
        link: Option<std::sync::Arc<crate::attach::agent_forward::AgentLink>>,
    ) {
        self.agent_link = link;
    }

    /// Make this connection's one offer, or say on the bar why there is none.
    pub(super) async fn offer_agent(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        let Some(link) = self.agent_link.clone() else {
            return Ok(());
        };
        match link.take_offer(self.agent_supported, || self.take_request_id()) {
            Ok(None) => Ok(()),
            Ok(Some(offer)) => conn.send(&offer).await,
            Err(why) => {
                self.show_notices(why.map(Notice::info));
                Ok(())
            }
        }
    }

    /// Hand the link its frames; any other comes back.
    pub(super) fn take_agent_frame(
        &mut self,
        frame: FrameKind,
        repaint: &mut RepaintAccumulator,
    ) -> Option<FrameKind> {
        let taken = self.agent_link.as_ref().and_then(|link| link.take(&frame));
        let Some(said) = taken else {
            return Some(frame);
        };
        self.apply_notices(said.into_iter().map(Notice::info).collect(), repaint);
        None
    }

    /// Put the question in hand up, once no other is.
    pub(super) fn raise_agent_question<W: crate::attach::RenderSink>(
        &mut self,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
    ) {
        if self.overlays.asking() {
            return;
        }
        let Some(question) = self.agent_link.as_ref().and_then(|link| link.take_held()) else {
            return;
        };
        let label = question
            .pane
            .as_ref()
            .and_then(|pane| self.pane_title(pane));
        let overlay = AgentSignOverlay::new(question, label.as_deref(), &self.settings.theme);
        self.overlays.push(Box::new(overlay));
        self.paint_overlay(out, sidebar);
    }

    /// Ask a question, or send an answer and take its question down.
    pub(super) async fn on_agent_due<W: crate::attach::RenderSink>(
        &mut self,
        conn: &mut Connection,
        out: &mut W,
        sidebar: Option<SidebarReservation>,
        due: Due,
    ) -> Result<Step, AttachError> {
        match due {
            Due::Ask(question) => {
                if let Some(link) = &self.agent_link {
                    link.hold(question);
                }
                self.raise_agent_question(out, sidebar);
            }
            Due::Reply { id, message } => {
                if self.overlays.dismiss_expired() {
                    if self.overlays.is_active() {
                        self.paint_overlay(out, sidebar);
                    } else {
                        self.repaint_view(out, sidebar, RepaintLevel::Full);
                    }
                }
                conn.send(&FrameKind::Command {
                    request_id: self.take_request_id(),
                    command: Command::Forward {
                        op: ForwardOp::AgentReply { id, message },
                    },
                })
                .await?;
            }
        }
        Ok(Step::Continue)
    }
}
