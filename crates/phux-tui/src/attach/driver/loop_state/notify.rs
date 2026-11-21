//! Agent notifications: feed the notifier, post what comes due.

use phux_protocol::ids::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::wire::frame::{
    FrameKind, PaneNotify, RESOURCE_NOTE_KEY, RESOURCE_NOTIFY_KEY, Scope,
};

use super::{AttachError, Connection};
use crate::attach::notify::deliver::{self, Delivery};

impl super::SessionLoop {
    /// Fold the agent list the chrome just projected into the notifier.
    pub(super) fn observe_agents_for_notify(
        &mut self,
        agents: &[crate::render::chrome::sidebar::AgentEntry],
    ) {
        let rows = super::super::chrome::notify_rows(
            agents,
            &self.mirror.workspace,
            self.mirror.focused_resource.as_ref(),
            &self.mirror.session_name,
        );
        self.notifier.observe(
            &rows,
            self.settings.notifications,
            tokio::time::Instant::now(),
        );
    }

    /// Track the outer terminal window's focus from `?1004` reports; the
    /// batch's last report, if any.
    pub(super) fn note_host_focus(&mut self, events: &[InputEvent]) -> Option<bool> {
        let mut focused = None;
        for event in events {
            if let InputEvent::Focus(focus) = event {
                let gained = matches!(focus, FocusEvent::Gained);
                self.notifier.set_client_focused(gained);
                self.clipboard_reads.borrow_mut().set_window_focused(gained);
                focused = Some(gained);
            }
        }
        focused
    }

    /// Post whatever came due: OSC bytes or a bell to the outer terminal, or
    /// a desktop notifier spawned off the loop.
    pub(super) fn on_notify_due<W: crate::attach::RenderSink>(&mut self, out: &mut W) {
        let cfg = self.settings.notifications;
        let Some(note) = self.notifier.take_due(cfg, tokio::time::Instant::now()) else {
            return;
        };
        self.notify_seq = self.notify_seq.wrapping_add(1);
        match deliver::plan(&note, cfg, self.notify_env, self.notify_seq) {
            Some(Delivery::Terminal(bytes)) => {
                if let Err(err) = out.write_all(&bytes).and_then(|()| out.flush()) {
                    tracing::debug!(%err, "notification write failed");
                }
            }
            Some(Delivery::System(argv)) => deliver::spawn_system(&argv),
            None => {}
        }
    }

    /// GET and SUBSCRIBE `phux.notify/v1` for every local pane, here or in a
    /// peer session, not yet watched; forget panes that are gone.
    pub(super) async fn watch_notify_modes(
        &mut self,
        conn: &mut Connection,
    ) -> Result<(), AttachError> {
        let live: Vec<ResourceId> = self
            .mirror
            .panes
            .keys()
            .chain(&self.peers.foreign_agent_subscribed)
            .filter(|id| id.is_local())
            .cloned()
            .collect();
        self.notifier.mode_watched.retain(|id| live.contains(id));
        for id in live {
            if !self.notifier.mode_watched.insert(id.clone()) {
                continue;
            }
            let request_id = self.take_request_id();
            self.notifier.mode_reads.insert(request_id, id.clone());
            let scope = Scope::Resource(id);
            let key = RESOURCE_NOTIFY_KEY.to_owned();
            conn.send(&FrameKind::GetMetadata {
                request_id,
                scope: scope.clone(),
                key: key.clone(),
            })
            .await?;
            conn.send(&FrameKind::SubscribeMetadata { scope, key })
                .await?;
        }
        Ok(())
    }

    /// Fold a notify-mode read or change, and drop a muted pane's note
    /// before it becomes a toast. `None` when the frame was one of those.
    pub(super) fn take_notify_mode(&mut self, frame: FrameKind) -> Option<FrameKind> {
        match frame {
            FrameKind::MetadataValue { request_id, value }
                if self.notifier.mode_reads.contains_key(&request_id) =>
            {
                if let Some(id) = self.notifier.mode_reads.remove(&request_id) {
                    let mode = value.as_deref().and_then(PaneNotify::decode);
                    self.notifier.set_mode(id, mode.unwrap_or_default());
                }
                None
            }
            FrameKind::Error {
                request_id: Some(request_id),
                ..
            } if self.notifier.mode_reads.remove(&request_id).is_some() => None,
            FrameKind::MetadataChanged {
                scope: Scope::Resource(id),
                key,
                value,
                ..
            } if key == RESOURCE_NOTIFY_KEY => {
                let mode = value.as_deref().and_then(PaneNotify::decode);
                self.notifier.set_mode(id, mode.unwrap_or_default());
                None
            }
            FrameKind::MetadataChanged {
                scope: Scope::Resource(ref id),
                ref key,
                ..
            } if key == RESOURCE_NOTE_KEY && !self.notifier.alerts(id) => None,
            other => Some(other),
        }
    }
}
