//! A pane asks to open a link here (`docs/ssh.md#opening-links`): raise the
//! prompt, and send what it answered.

use std::time::Instant;

use phux_protocol::open_url::OpenUrlOp;
use phux_protocol::wire::frame::Command;

use super::super::{AttachError, Connection, FrameKind, FrameOutcome};
use crate::attach::hosts::{AttachOrigin, origin_label};
use crate::attach::open_url::{self, OpenUrlOverlay};

impl super::super::SessionLoop {
    /// Ask about the frame's link, emptying it from `outcome`.
    pub(super) fn ask_open_url(&mut self, outcome: &mut FrameOutcome) {
        let Some((terminal, id, url)) = outcome.open_url.take() else {
            return;
        };
        // One question at a time: a link never covers another.
        if self.overlays.asking() {
            tracing::info!(terminal_id = ?terminal, id, "link request declined: another question is up");
            self.open_urls.borrow_mut().decline(&terminal, id);
            return;
        }
        let now = Instant::now();
        let asked = self
            .open_urls
            .borrow_mut()
            .on_request(&terminal, id, &url, now);
        let Some(link) = asked else {
            tracing::info!(terminal_id = ?terminal, id, "link request declined unasked");
            return;
        };
        let peers = &self.peers;
        let host = (peers.origin.as_ref())
            .and_then(|origin| {
                origin_label(&peers.remote_hosts, origin, peers.serving_host.as_deref())
            })
            .or_else(|| peers.serving_host.clone())
            .unwrap_or_else(|| "this server".to_owned());
        let remote = match &peers.origin {
            Some(AttachOrigin::Remote(name)) => Some(name.as_str()),
            _ => None,
        };
        self.overlays.push(Box::new(OpenUrlOverlay::new(
            (terminal, id, link),
            (&host, &self.mirror.session_name, remote),
            std::rc::Rc::clone(&self.open_urls),
            now,
            &self.settings.theme,
        )));
        outcome.chrome_dirty = true;
    }

    /// Open what was agreed to and tell the server how each request ended.
    pub(in crate::attach::driver::loop_state) async fn send_open_url_answers(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        let answers = self.open_urls.borrow_mut().take_answers();
        for answer in answers {
            let opened = answer.link.is_some_and(|link| match open_url::open(&link) {
                Ok(()) => true,
                Err(err) => {
                    tracing::warn!(%err, "could not open a link");
                    false
                }
            });
            let request_id = self.take_request_id();
            let command = Command::OpenUrl {
                terminal_id: answer.terminal,
                op: OpenUrlOp::Reply {
                    id: answer.id,
                    opened,
                },
            };
            let frame = FrameKind::Command {
                request_id,
                command,
            };
            super::super::super::session_io::send_unless_peer_gone(conn, &frame).await?;
        }
        Ok(())
    }
}
