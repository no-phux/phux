//! What each runtime event does to the window.

use phux_client_runtime::Client;
use phux_client_runtime::control::{Event, Observation};
use phux_protocol::ids::ResourceId;
use rune::Tx;

use super::{Desk, NoticeKind, layout};

mod receipts;
pub(super) use receipts::Receipts;

impl Desk {
    /// Reconcile a coalesced event batch with its latest-incarnation snapshot
    /// before exposing any resource publications to the window.
    pub(super) fn observe(
        &mut self,
        client: &Client,
        mut observation: Observation,
        tx: &mut Tx,
    ) -> (bool, bool) {
        self.fence_epoch_events(&mut observation.events, observation.connection_epoch);
        let mut changed = self.follow_connection(client, &mut observation);
        changed |= self.reconcile_receipts(client, tx);
        // Output alone can't move anything: no layout pass per keystroke.
        let rearrange = changed || observation.events.iter().any(moves_layout);
        for event in observation.events {
            changed |= self.apply(client, event, tx);
        }
        (changed, rearrange)
    }

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
        match event {
            Event::DocumentPublished { publication } => self.publish_document(&publication),
            Event::TerminalChanged { terminal_id } if self.view_of(&terminal_id).is_some() => {
                self.relink(&terminal_id);
                self.generation += 1;
                true
            }
            Event::TerminalSpawned {
                request_id,
                terminal_id,
                error,
            } => self.spawned(client, request_id, terminal_id, error),
            Event::TerminalClosed {
                terminal_id,
                exit_status,
                ..
            } => self.resource_closed(client, &terminal_id, exit_status),
            Event::OrganizationOperation { operation, outcome } => {
                self.layout_outcome(client, operation, outcome)
            }
            // OSC 52 only from the terminal on screen, while the window has
            // focus; never retained for later.
            Event::ClipboardWrite { terminal_id, text }
                if self.terminal_has_keyboard(&terminal_id) =>
            {
                tx.copy(text.0);
                false
            }
            other => self.apply_connection(client, other),
        }
    }

    fn terminal_has_keyboard(&self, id: &ResourceId) -> bool {
        self.focused
            && self
                .shown
                .as_ref()
                .is_some_and(|s| &s.terminal == id && s.view.is_some())
    }

    fn apply_connection(&mut self, client: &Client, event: Event) -> bool {
        match event {
            Event::Exited {
                terminal_id,
                exit_status,
                ..
            } => self.producer_ended(&terminal_id, exit_status),
            Event::ConnectionLost { .. } => {
                self.retire_documents(client);
                true
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
            other => self.apply_document_subscription(client, other),
        }
    }

    fn apply_document_subscription(&mut self, client: &Client, event: Event) -> bool {
        match event {
            Event::CommandResult { request_id, result } => {
                self.document_input_result(request_id, &result)
                    || self.document_subscription_result(client, request_id, result)
            }
            Event::TopologySnapshot { snapshot, .. } => {
                self.resources.input.note_snapshot(&snapshot);
                true
            }
            Event::TerminalDetached {
                terminal_id,
                error: None,
                ..
            } => self.document_detached(&terminal_id),
            _ => false,
        }
    }

    fn spawned(
        &mut self,
        client: &Client,
        request: u32,
        id: Option<ResourceId>,
        error: Option<String>,
    ) -> bool {
        let source = self.resources.starting.remove(&request);
        if let Some((new, source)) = id.as_ref().zip(source) {
            self.resources.producers.insert(new.clone(), source);
        }
        if let Some(new) = &id
            && let Some(placement) = self.pending_splits.remove(&request)
        {
            self.place_split(client, new, placement);
            return true;
        }
        let ours =
            self.pending_spawns.contains(&request) || self.pending_splits.contains_key(&request);
        if !ours {
            return false;
        }
        self.pending_spawns.retain(|id| *id != request);
        self.pending_splits.remove(&request);
        match id {
            Some(id) => self.want(id),
            None => {
                let error = error.unwrap_or_else(|| "spawn failed".to_owned());
                let text = source.map_or_else(
                    || error.clone(),
                    |source| format!("Mirror not started: {error}. {}", source.prerequisites()),
                );
                self.say(NoticeKind::Error, text);
            }
        }
        true
    }

    fn resource_closed(&mut self, client: &Client, id: &ResourceId, exit: Option<i32>) -> bool {
        let producer = self.producer_ended(id, exit);
        let document = self.forget_document(client, id);
        let pending_phone = self.phone.is_some()
            && self.shown.is_none()
            && self.wanted.as_ref().is_some_and(|(wanted, _)| wanted == id);
        if self.shown.as_ref().is_some_and(|s| &s.terminal == id) || pending_phone {
            let next = self
                .layout
                .as_ref()
                .filter(|_| self.phone.is_none())
                .and_then(|tree| layout::neighbour(tree, id));
            self.close_shown(client, std::slice::from_ref(id));
            if let Some(next) = next {
                self.want_automatic(next);
            }
            return true;
        }
        let other = self.others.remove(id);
        if let Some(view) = other.flatten() {
            let _ = client.destroy_view(view);
        }
        let cancelled = self.wanted.take_if(|(wanted, _)| wanted == id).is_some();
        producer || document || other.is_some() || cancelled
    }
}

pub(super) const fn moves_layout(event: &Event) -> bool {
    !matches!(
        event,
        Event::TerminalChanged { .. } | Event::DocumentPublished { .. }
    )
}

pub(super) const fn changes_resources(event: &Event) -> bool {
    matches!(
        event,
        Event::OrganizationChanged
            | Event::TopologyChanged
            | Event::Attached { .. }
            | Event::ConnectionOpened { .. }
    )
}
