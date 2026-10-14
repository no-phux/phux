//! Agent notifications: feed the notifier, post what comes due.

use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;

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
            &self.mirror.panes,
            self.mirror.focused_resource.as_ref(),
            &self.mirror.session_name,
        );
        self.notifier.observe(
            &rows,
            self.settings.notifications,
            tokio::time::Instant::now(),
        );
    }

    /// Track the outer terminal window's focus from `?1004` reports.
    pub(super) fn note_host_focus(&mut self, events: &[InputEvent]) {
        for event in events {
            if let InputEvent::Focus(focus) = event {
                self.notifier
                    .set_client_focused(matches!(focus, FocusEvent::Gained));
            }
        }
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
}
