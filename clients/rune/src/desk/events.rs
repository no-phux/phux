//! What each runtime event does to the window.

use phux_client_core::org_ops::OrgOutcome;
use phux_client_runtime::Client;
use phux_client_runtime::control::Event;
use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::{Desk, NoticeKind, layout};

impl Desk {
    pub(super) fn apply(&mut self, client: &Client, event: Event, tx: &mut Tx) -> bool {
        // A background window shows nothing to anyone.
        let on_screen = if self.focused {
            self.on_screen()
        } else {
            Vec::new()
        };
        let marked = self.activities.apply(&event, &on_screen);
        let inbox = self.inbox_event(client, &event);
        marked | inbox | self.apply_view(client, event, tx)
    }

    /// The terminals in front of the person.
    pub(super) fn on_screen(&self) -> Vec<ResourceId> {
        match &self.layout {
            Some(tree) => phux_client_core::layout::leaves(tree),
            None => self.shown.iter().map(|s| s.terminal.clone()).collect(),
        }
    }

    fn apply_view(&mut self, client: &Client, event: Event, tx: &mut Tx) -> bool {
        let shown = |id: &ResourceId| self.shown.as_ref().is_some_and(|s| &s.terminal == id);
        match event {
            Event::TerminalChanged { terminal_id }
                if shown(&terminal_id) || self.others.contains_key(&terminal_id) =>
            {
                self.relink(&terminal_id);
                self.generation += 1;
                true
            }
            Event::TerminalSpawned {
                request_id,
                terminal_id: Some(new),
                ..
            } if self.pending_splits.contains_key(&request_id) => {
                if let Some(placement) = self.pending_splits.remove(&request_id) {
                    self.place_split(client, &new, placement);
                }
                true
            }
            Event::TerminalSpawned {
                request_id,
                terminal_id,
                error,
            } if self.pending_spawns.contains(&request_id)
                || self.pending_splits.contains_key(&request_id) =>
            {
                self.pending_spawns.retain(|id| *id != request_id);
                self.pending_splits.remove(&request_id);
                match (terminal_id, error) {
                    (Some(id), _) => self.want(id),
                    (None, error) => self.say(
                        NoticeKind::Error,
                        error.unwrap_or_else(|| "spawn failed".to_owned()),
                    ),
                }
                true
            }
            Event::TerminalClosed { terminal_id, .. } if shown(&terminal_id) => {
                // Keep the keyboard in the split: the neighbour takes focus.
                let next = self
                    .layout
                    .as_ref()
                    .and_then(|t| layout::neighbour(t, &terminal_id));
                self.close_shown(client);
                if let Some(next) = next {
                    self.focus_pane(&next);
                }
                true
            }
            Event::OrganizationOperation { operation, outcome }
                if self.layout_ops.remove(&operation) =>
            {
                let refused = matches!(outcome, OrgOutcome::Refused(_) | OrgOutcome::Error { .. });
                self.layout_written(client, operation, refused);
                true
            }
            Event::TerminalClosed { terminal_id, .. } if self.others.contains_key(&terminal_id) => {
                if let Some(view) = self.others.remove(&terminal_id) {
                    let _ = client.destroy_view(view);
                }
                true
            }
            // OSC 52 only from the terminal on screen, while the window has
            // focus; never retained for later.
            Event::ClipboardWrite { terminal_id, text } if shown(&terminal_id) && self.focused => {
                tx.copy(text.0);
                false
            }
            Event::Detached { reason, message } => {
                let said = reason.map(|reason| reason.describe().to_owned());
                self.link.ended = said.or((!message.is_empty()).then_some(message));
                true
            }
            Event::ConnectionOpened { .. } => {
                self.link.ended = None;
                true
            }
            Event::OrganizationUnavailable { .. } => {
                self.link.org_unavailable = Some(self.epoch);
                true
            }
            Event::InputDelivery {
                delivery_id,
                outcome,
                ..
            } => self.paste_delivered(delivery_id, outcome),
            Event::StatusChanged(_)
            | Event::TopologyChanged
            | Event::OrganizationChanged
            | Event::Attached { .. } => true,
            _ => false,
        }
    }
}

pub(super) const fn moves_layout(event: &Event) -> bool {
    !matches!(event, Event::TerminalChanged { .. })
}
