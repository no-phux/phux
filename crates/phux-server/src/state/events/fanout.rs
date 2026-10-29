//! The [`ServerState`] side: recording, subscribing, the pump, and the
//! one fan-out path.

use std::sync::Arc;

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::wire::frame::{AgentEvent, EventStamp, FrameKind};
use tokio::sync::mpsc;

use super::{EventFilter, EventPump, EventScope, PumpStep, SatelliteScopeChange};
use crate::mailbox::Outbound;
use crate::state::ServerState;
use crate::state::client::ClientId;
use crate::state::journal::{EventRecord, JournalEntry, now_unix_ms};

impl ServerState {
    /// Stamp `record` in the journal and offer it to every subscription,
    /// under the state lock. Returns the `seq` (`None` if exhausted).
    pub fn record_and_fanout(&mut self, mut record: EventRecord) -> Option<u64> {
        let actor = record
            .actor_ref
            .take()
            .or_else(|| record.actor.map(|client| self.clients.actor_ref(client)));
        record.subject = crate::policy::filter::EventSubject::at(
            self,
            record.terminal.as_ref(),
            record.parent.as_ref(),
        );
        let entry = self.journal.record(record, actor, now_unix_ms())?;
        self.clients.offer_event(&entry);
        Some(entry.seq())
    }

    /// [`Self::record_relayed_event_as`] with no known consumer.
    #[cfg(test)]
    pub fn record_relayed_event(
        &mut self,
        terminal: WireResourceId,
        event: AgentEvent,
        satellite: Option<&EventStamp>,
    ) -> Option<u64> {
        self.record_relayed_event_as(terminal, event, satellite, None)
    }

    /// Stamp and deliver an event a hub relays from a satellite (L1 §7.3),
    /// with this server's `seq`, to that satellite Terminal's subscribers.
    /// The satellite's actor does not cross (it names a satellite
    /// connection); `actor` is the consumer whose keyed operation caused it
    /// (L1 §9.1), when known. A relayed `RESOURCE_CLOSED` first withdraws
    /// holds on that Terminal (ADR-0128). Not retained: satellite cursors are
    /// answered with a gap.
    pub fn record_relayed_event_as(
        &mut self,
        terminal: WireResourceId,
        event: AgentEvent,
        satellite: Option<&EventStamp>,
        actor: Option<super::ClientId>,
    ) -> Option<u64> {
        // Nothing follows a close (L1 §7): withdraw holds first.
        if matches!(event, AgentEvent::ResourceClosed { .. }) {
            self.withdraw_approvals_naming(&terminal);
        }
        let seq = self.journal.allocate_seq()?;
        let ts_ms = satellite.map_or_else(now_unix_ms, |stamp| stamp.ts_ms);
        let actor = actor.map(|client| self.clients.actor_ref(client));
        let stamp = EventStamp::new(seq, ts_ms)
            .with_actor(actor)
            .with_operation_id(satellite.and_then(|stamp| stamp.operation_id));
        let entry = JournalEntry::relayed(terminal, event, stamp);
        self.clients.offer_event(&entry);
        Some(seq)
    }

    /// The newest journal `seq`, `0` before the first event.
    #[must_use]
    pub const fn journal_head(&self) -> u64 {
        self.journal.head()
    }

    /// The journal head `GET_STATE` reports to `client` (L1 §7.3): with
    /// subscriptions, the newest `seq` their replay will deliver (gaps
    /// included) so catch-up always completes; otherwise the global head.
    #[must_use]
    pub fn journal_head_for(&self, client: Option<ClientId>) -> u64 {
        match client.and_then(|id| self.clients.event_subscriptions.get(&id)) {
            Some(sub) if !sub.scopes.is_empty() => self
                .journal
                .newest_admitted(|entry| sub.admits(entry))
                .max(sub.reach())
                .max(sub.owed_eviction(&self.journal)),
            _ => self.journal.head(),
        }
    }

    /// Re-bound the journal (entries and bytes).
    pub fn set_event_journal_bounds(&mut self, entries: usize, bytes: usize) {
        self.journal.set_bounds(entries, bytes);
    }

    /// Install a live `SUBSCRIBE_EVENTS` scope (`None` is server-wide),
    /// clearing any filter on it; `tx` reaches a client that never attached.
    pub fn subscribe_events(
        &mut self,
        client_id: ClientId,
        terminal: Option<WireResourceId>,
        tx: mpsc::Sender<Outbound>,
    ) {
        self.clients
            .event_subscription(client_id, tx)
            .scopes
            .insert(EventScope::of(terminal), EventFilter::all());
    }

    /// Install `SUBSCRIBE_RESOURCE_EVENTS`: `terminal`'s scope with `filter`.
    pub fn subscribe_resource_events(
        &mut self,
        client_id: ClientId,
        terminal: WireResourceId,
        filter: EventFilter,
        tx: mpsc::Sender<Outbound>,
    ) {
        self.clients
            .event_subscription(client_id, tx)
            .scopes
            .insert(EventScope::Terminal(terminal), filter);
    }

    /// Install a cursor subscription and start its replay (L1 §7.3). What
    /// the mailbox takes is queued now; the pump pulls the rest.
    pub fn subscribe_events_after(
        &mut self,
        client_id: ClientId,
        terminal: Option<WireResourceId>,
        after_seq: u64,
        tx: mpsc::Sender<Outbound>,
    ) {
        let head = self.journal.head();
        let sub = self.clients.event_subscription(client_id, tx);
        sub.scopes
            .insert(EventScope::of(terminal), EventFilter::all());
        sub.journal_aware = true;
        sub.begin_replay(after_seq, head);
        sub.fill(&self.journal);
    }

    /// Install a hub consumer's satellite Terminal scope (L1 §7.3); relayed
    /// events are not retained, so a missing cursor range is owed a gap.
    /// Returns the prior scope for [`Self::restore_satellite_scope`].
    pub fn subscribe_satellite_events(
        &mut self,
        client_id: ClientId,
        terminal: WireResourceId,
        filter: EventFilter,
        after_seq: Option<u64>,
        tx: mpsc::Sender<Outbound>,
    ) -> SatelliteScopeChange {
        let gap = after_seq.and_then(|after| self.journal.gap_without_replay(after));
        let sub = self.clients.event_subscription(client_id, tx);
        let prior = sub
            .scopes
            .insert(EventScope::Terminal(terminal.clone()), filter);
        sub.journal_aware |= after_seq.is_some();
        if let Some((first, last)) = gap {
            sub.owe_gap(first, last);
            sub.fill(&self.journal);
        }
        SatelliteScopeChange { terminal, prior }
    }

    /// Undo a refused satellite subscribe: restore the prior filter or
    /// remove the scope; an owed gap is kept.
    pub fn restore_satellite_scope(&mut self, client_id: ClientId, change: SatelliteScopeChange) {
        let SatelliteScopeChange { terminal, prior } = change;
        let Some(prior) = prior else {
            self.clients
                .unsubscribe_terminal_events(client_id, &terminal);
            return;
        };
        if let Some(sub) = self.clients.event_subscriptions.get_mut(&client_id) {
            sub.scopes.insert(EventScope::Terminal(terminal), prior);
        }
    }

    /// End a satellite scope whose link went away: the consumer is owed a
    /// gap at a freshly taken `seq`. The subscription stays until detach so
    /// the gap is delivered.
    pub fn interrupt_satellite_scope(&mut self, client_id: ClientId, terminal: &WireResourceId) {
        let scope = EventScope::Terminal(terminal.clone());
        let Some(sub) = self.clients.event_subscriptions.get_mut(&client_id) else {
            return;
        };
        if sub.scopes.remove(&scope).is_none() {
            return;
        }
        let Some(seq) = self.journal.allocate_seq() else {
            return;
        };
        sub.owe_gap(seq, seq);
        sub.fill(&self.journal);
    }

    /// Claim `client_id`'s event pump, once; `None` without a subscription
    /// or if its pump already runs.
    pub fn claim_event_pump(&mut self, client_id: ClientId) -> Option<EventPump> {
        let sub = self.clients.event_subscriptions.get_mut(&client_id)?;
        if sub.pump_claimed {
            return None;
        }
        sub.pump_claimed = true;
        let pump = EventPump {
            wake: Arc::clone(&sub.wake),
            tx: sub.tx.clone(),
            epoch: sub.epoch,
        };
        if sub.is_owed_work() {
            pump.wake.notify_one();
        }
        Some(pump)
    }

    /// The next frame for subscription `epoch`'s pump: an owed frame,
    /// [`PumpStep::Idle`], or [`PumpStep::Gone`] once replaced or ended.
    pub fn next_owed_event_frame(&mut self, client_id: ClientId, epoch: u64) -> PumpStep {
        let Some(sub) = self.clients.event_subscriptions.get_mut(&client_id) else {
            return PumpStep::Gone;
        };
        if sub.epoch != epoch {
            return PumpStep::Gone;
        }
        sub.next_owed(&self.journal)
            .map_or(PumpStep::Idle, PumpStep::Frame)
    }

    /// Drop `client`'s event scope for terminal `wire` (`DETACH_RESOURCE`).
    pub fn unsubscribe_terminal_events(&mut self, client: ClientId, wire: &WireResourceId) {
        self.clients.unsubscribe_terminal_events(client, wire);
    }

    /// Remember `client_id`'s `HELLO.client_name` for its `ActorRef`.
    pub fn set_client_name(&mut self, client_id: ClientId, name: String) {
        self.clients.client_names.insert(client_id, name);
    }

    /// Offer an unjournaled `input_activity` for `terminal` to every
    /// subscriber except `sender`. The frame has no stamp, the journal is
    /// not touched, and a full mailbox drops the frame (there is no `seq`
    /// to owe as a `journal_gap`). `origin` is the HELLO name, or
    /// `client:<id>` when the name is missing.
    pub fn offer_input_activity(&mut self, terminal: &WireResourceId, sender: ClientId) {
        let origin = self
            .clients
            .client_names
            .get(&sender)
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("client:{}", sender.0));
        let subject = crate::policy::filter::EventSubject::at(self, Some(terminal), None);
        let frame = FrameKind::Event {
            terminal: Some(terminal.clone()),
            event: AgentEvent::InputActivity { origin },
            stamp: None,
        };
        for (client, sub) in &mut self.clients.event_subscriptions {
            if *client == sender || !sub.admits_input_activity(terminal, &subject) {
                continue;
            }
            // A miss is a drop. Owing a gap would invent a journal seq.
            let _ = sub.try_send(frame.clone());
        }
    }
}
