//! What each runtime event does to the window.

use phux_client_runtime::Client;
use phux_client_runtime::control::{Event, Observation};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::DetachReason;
use rune::Tx;

use super::{Desk, NoticeKind};

mod receipts;
pub(super) use receipts::Receipts;

impl Desk {
    /// What an observation says about the connection itself. The topology
    /// is only sampled when the batch can have changed it.
    pub(super) fn follow_connection(
        &mut self,
        client: &Client,
        observation: &mut Observation,
    ) -> bool {
        let mut changed = observation.status != self.connection.status;
        if observation.connection_epoch != self.connection.epoch {
            if let Some(panel) = self.quick.as_mut() {
                panel.forget_move();
            }
            self.retire_documents(client);
            self.first_contact();
            self.connection.epoch = observation.connection_epoch;
            self.connection.focus_told = None;
            changed |= self.reopen_after_reconnect(client);
        }
        self.connection.status = observation.status;
        let failure = observation.last_error.take();
        if let Some(error) = failure.filter(|_| self.connection.status.is_terminal()) {
            changed |= self.connection.error.as_ref() != Some(&error);
            self.failed_with(&error);
            self.connection.error = Some(error);
        }
        if let Some(topology) = observation.topology.take()
            && self.connection.topology.as_ref() != Some(&topology)
        {
            for pane in &topology.panes {
                self.activities.marked(&pane.terminal_id, pane.unseen);
            }
            self.connection.topology = Some(topology);
            changed = true;
        }
        if self.connection.server.is_none() {
            self.connection.server = client.server().map(|info| format!("{info:?}"));
        }
        if changed || observation.events.iter().any(changes_resources) {
            changed |= self.refresh_resources(client);
        }
        changed
    }

    /// Reconcile a coalesced event batch with its latest-incarnation snapshot
    /// before exposing any resource publications to the window.
    pub(super) fn observe(
        &mut self,
        client: &Client,
        mut observation: Observation,
        tx: &mut Tx,
    ) -> (bool, bool) {
        if !self.connection.owns_client(client) {
            return (false, false);
        }
        // Qualify this observation's handles without sampling a newer HELLO
        // for an old batch. The epoch and identity are read under one lock.
        let scope = client.with_control(|control| {
            (control.connection_epoch() == observation.connection_epoch)
                .then(|| control.server().map(|server| server.id.clone()))
        });
        let Some(scope) = scope else {
            return (false, false);
        };
        if observation.topology.is_some()
            && observation.status == phux_client_runtime::control::Status::Attached
        {
            self.connection.view_server = scope;
            self.connection.view_epoch = observation.connection_epoch;
        }
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
        match &self.connection.layout {
            Some(tree) => phux_client_core::layout::leaves(tree),
            None => self
                .connection
                .shown
                .iter()
                .map(|s| s.resource.clone())
                .collect(),
        }
    }

    fn apply_view(&mut self, client: &Client, event: Event, tx: &mut Tx) -> bool {
        match event {
            // A stale wake reads nothing: a newer one follows it.
            Event::DocumentPublished {
                resource_id,
                generation,
            } => client
                .acquire_document_at(&resource_id, generation)
                .is_some_and(|publication| self.publish_document(&publication)),
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
            } => {
                self.popup_ended(client, &terminal_id)
                    | self.resource_closed(client, &terminal_id, exit_status)
            }
            Event::TerminalsClosed {
                error: Some(error), ..
            } => {
                self.say(
                    NoticeKind::Error,
                    format!("Some close requests could not be confirmed. {error}"),
                );
                true
            }
            Event::OrganizationOperation { operation, outcome } => {
                self.session_creation_outcome(client, operation, &outcome)
                    | self.quick_move_outcome(client, operation, &outcome)
                    | self.sidebar_move_outcome(client, operation, &outcome)
                    | self.layout_outcome(client, operation, outcome)
            }
            event @ Event::Frame(_) => phux_client_runtime::control::forward::agent_request(event)
                .is_some_and(|request| self.agent_request(request)),
            Event::ClipboardWrite { terminal_id, text } => {
                self.clipboard_write(terminal_id, text.0, tx)
            }
            Event::ClipboardRead {
                terminal_id,
                selection,
            } => self.clipboard_read(terminal_id, selection, tx),
            Event::OpenUrl {
                terminal_id,
                id,
                url,
            } => self.open_url_asked(terminal_id, id, &url),
            Event::Note {
                terminal_id,
                note: Some(note),
                live: true,
            } if !self.inbox.muted.contains_key(&terminal_id) => {
                self.note_posted(&terminal_id, &note)
            }
            Event::NotifyMode { terminal_id, mode } => {
                if mode.alerts() {
                    self.inbox.muted.remove(&terminal_id).is_some()
                } else {
                    self.inbox.muted.insert(terminal_id, mode) != Some(mode)
                }
            }
            Event::CommandResult {
                request_id,
                ref result,
            } if self.host_copied(request_id, result, tx) => true,
            other => self.apply_connection(client, other),
        }
    }

    pub(super) fn terminal_has_keyboard(&self, id: &ResourceId) -> bool {
        let keyboard = self.keyboard();
        self.focused && keyboard.is_some_and(|(has, view)| has == id && view.is_some())
    }

    pub(super) fn apply_connection(&mut self, client: &Client, event: Event) -> bool {
        match event {
            Event::Exited {
                terminal_id,
                exit_status,
                ..
            } => {
                self.popup_ended(client, &terminal_id)
                    | self.producer_ended(&terminal_id, exit_status)
            }
            Event::ConnectionLost { .. } => {
                self.retire_documents(client);
                true
            }
            // The server ended this attach and keeps the socket. Nothing is
            // lost: `settle_home` picks another session on this connection.
            Event::Detached {
                reason: Some(DetachReason::SessionKilled),
                ..
            } => true,
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
                self.link.org_unavailable = Some(self.connection.epoch);
                true
            }
            Event::InputDelivery {
                delivery_id,
                outcome,
                ..
            } => self.paste_delivered(delivery_id, outcome),
            // A new foreground renames the pane in the sidebar.
            Event::StatusChanged(_)
            | Event::TopologyChanged
            | Event::OrganizationChanged
            | Event::BackgroundWork { .. } => true,
            Event::Attached { .. } => {
                self.connection.in_view.clear();
                if self.connection.size_locked {
                    self.claim_size_lock(true);
                }
                true
            }
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
                self.connection.resources.input.note_snapshot(&snapshot);
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
        if self.popup_spawned(client, request, id.as_ref(), error.as_deref()) {
            return true;
        }
        let origin = self.connection.spawn_servers.remove(&request);
        let follow = origin
            .as_ref()
            .is_some_and(|origin| origin.choice == self.connection.view_choice);
        let ours = self.connection.pending_spawns.contains(&request)
            || self.connection.pending_splits.contains_key(&request);
        if ours
            && id.is_some()
            && origin
                .as_ref()
                .is_none_or(|origin| !origin.owns_client(client))
        {
            self.connection
                .pending_spawns
                .retain(|pending| *pending != request);
            self.connection.pending_splits.remove(&request);
            self.connection.resources.starting.remove(&request);
            return true;
        }
        if let Some(id) = id.as_ref()
            && let Some(origin) = origin
        {
            let intent =
                super::view_intent::ViewIntent::new(client, id, origin.server, origin.epoch);
            if intent.refused() {
                self.connection
                    .pending_spawns
                    .retain(|pending| *pending != request);
                self.connection.pending_splits.remove(&request);
                self.connection.resources.starting.remove(&request);
                self.say(NoticeKind::Error, "Pending pane view cancelled: its original source is no longer available. Check the server’s panes in the palette before retrying; a producer may already be running.");
                return true;
            }
            if follow {
                self.connection.view_intents.insert(id.clone(), intent);
            } else {
                self.connection
                    .view_intents
                    .entry(id.clone())
                    .or_insert(intent);
            }
        }
        let source = self.connection.resources.starting.remove(&request);
        if let Some((new, source)) = id.as_ref().zip(source) {
            self.connection
                .resources
                .producers
                .insert(new.clone(), source);
        }
        if let Some(new) = &id
            && let Some((session, placement)) = self.connection.pending_splits.remove(&request)
        {
            self.place_split(client, new, placement, session, follow);
            return true;
        }
        let ours = self.connection.pending_spawns.contains(&request)
            || self.connection.pending_splits.contains_key(&request);
        if !ours {
            return false;
        }
        self.connection.pending_spawns.retain(|id| *id != request);
        self.connection.pending_splits.remove(&request);
        match id {
            Some(id) if follow => self.want_automatic(id),
            Some(_) => {}
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
        self.connection.tabs.recent.retain(|left| left != id);
        // The next of a closing tab, asked for a moment ago.
        let pending = self.connection.shown.is_none()
            && self
                .connection
                .wanted
                .as_ref()
                .is_some_and(|(wanted, _)| wanted == id);
        if self
            .connection
            .shown
            .as_ref()
            .is_some_and(|s| &s.resource == id)
            || pending
        {
            let closed = std::slice::from_ref(id);
            let next = self.after_closing(closed);
            self.close_shown(client, closed);
            self.drop_leaf(id);
            if let Some(next) = next {
                self.want_automatic(next);
            }
            return true;
        }
        self.drop_leaf(id);
        let other = self.connection.others.remove(id);
        if let Some(view) = other.flatten() {
            let _ = client.destroy_view(view);
        }
        let cancelled = self
            .connection
            .wanted
            .take_if(|(wanted, _)| wanted == id)
            .is_some();
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
