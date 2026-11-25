//! Exact source reservations and the tasks they own.

use super::{Arc, AtomicU64, AttachResourceGeneration, AttachResourcePumpReplacement, BootstrapId, CancellationToken, ClientId, LastValidAttachSequence, OutputPumpTask, StreamDisposition, ResourceId, ResourceTable, AbortHandle};

impl ResourceTable {
    // -- output task lifetimes and ATTACH_RESOURCE generations --------

    /// Ask terminal pumps for their final capture, retaining completion fences
    /// before the resource and subscription tables are removed.
    pub(in crate::state) fn begin_output_drain(
        &mut self,
        terminal: ResourceId,
    ) -> Vec<(ClientId, Vec<CancellationToken>)> {
        self.pumps.iter_mut().filter_map(|((client, pane), generation)| {
            if *pane != terminal || generation.cancel.is_cancelled()
                || generation.publication.as_ref().is_some_and(CancellationToken::is_cancelled) { return None; }
            generation.disposition = StreamDisposition::Draining;
            if let Some(tasks) = self.output_pumps.get(&(*client, *pane)) {
                for task in tasks {
                    if Arc::ptr_eq(&task.generation, &generation.last_valid_seq)
                        && !task.done.is_cancelled() && let Some(drain) = &task.drain { drain.cancel(); }
                }
            }
            (!generation.source_done.is_cancelled()).then(|| (*client, vec![generation.source_done.clone()]))
        }).collect()
    }

    pub(in crate::state) fn remember_draining_terminal(
        &mut self,
        wire: phux_protocol::ids::ResourceId,
        core: ResourceId,
    ) {
        self.draining_resources.insert(wire, core);
    }

    /// The core id of a closed Terminal whose output is still draining, by
    /// its retired wire id.
    #[must_use]
    pub(crate) fn draining_terminal(
        &self,
        wire: &phux_protocol::ids::ResourceId,
    ) -> Option<ResourceId> {
        self.draining_resources.get(wire).copied()
    }

    /// End `terminal`'s close drain: cancel its pumps and forget its wire id.
    pub(crate) fn finish_output_drain(&mut self, terminal: ResourceId) {
        self.cancel_pumps_for_terminal(terminal);
        self.draining_resources.retain(|_, core| *core != terminal);
    }

    /// Let this pane's output pumps finish without aborting them.
    pub(super) fn release_output_pumps(&mut self, terminal: ResourceId) {
        let keys: Vec<_> = self
            .output_pumps
            .keys()
            .filter(|(_, pane)| *pane == terminal)
            .copied()
            .collect();
        for key in keys {
            let mut draining = Vec::new();
            for task in self.output_pumps.remove(&key).unwrap_or_default() {
                if task
                    .drain
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
                    && !task.done.is_cancelled()
                {
                    draining.push(task);
                } else {
                    task.release();
                }
            }
            if !draining.is_empty() {
                self.output_pumps.insert(key, draining);
            }
        }
    }

    /// Abort every output task for this subscription, returning their exit
    /// fences; await them off the lock before acknowledging.
    pub(crate) fn stop_output_pumps(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
    ) -> Vec<CancellationToken> {
        self.abort_output_pumps(client, terminal);
        self.output_pumps.get(&(client, terminal)).into_iter().flatten()
            .map(|task| task.done.clone()).collect()
    }

    fn abort_output_pumps(&mut self, client: ClientId, terminal: ResourceId) {
        if let Some(tasks) = self.output_pumps.get_mut(&(client, terminal)) {
            tasks.retain(|task| !task.done.is_cancelled());
            for task in tasks.iter() { if let Some(abort) = &task.abort { abort.abort(); } }
        }
    }

    /// Admit the new exact source and retire only its predecessors, under one
    /// lock and before the source can await. Retain their completion fences so
    /// a third admission cannot bypass a first admission still draining them.
    pub(crate) fn reserve_pump(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        bootstrap_id: BootstrapId,
        cancel: CancellationToken,
        publication: Option<CancellationToken>,
    ) -> AttachResourcePumpReplacement {
        // SESSION ATTACH supplies a shared generation rather than allocating
        // one per resource. Its first lag recovery must still choose a fresh id.
        let next = self.next_bootstrap.entry(client).or_insert(1);
        *next = (*next).max(bootstrap_id.get().saturating_add(1));
        let source_done = CancellationToken::new();
        let last_valid_seq: LastValidAttachSequence = Arc::new(AtomicU64::new(0));
        let prior = self
            .pumps
            .insert(
                (client, terminal),
                AttachResourceGeneration {
                    cancel: cancel.clone(),
                    source_done: source_done.clone(),
                    disposition: StreamDisposition::Active,
                    last_valid_seq: Arc::clone(&last_valid_seq),
                    bootstrap_id,
                    publication,
                },
            )
            .map(|prior| {
                prior.cancel.cancel();
                if let Some(publication) = prior.publication { publication.cancel(); }
                (prior.bootstrap_id, prior.last_valid_seq)
            });
        self.abort_output_pumps(client, terminal);
        (cancel, source_done, last_valid_seq, prior)
    }

    /// Identity, not bootstrap numbers: lag recovery changes the latter.
    pub(crate) fn pump_matches(&self, client: ClientId, terminal: ResourceId, fence: &LastValidAttachSequence) -> bool {
        self.pumps.get(&(client, terminal)).is_some_and(|generation| Arc::ptr_eq(&generation.last_valid_seq, fence))
    }

    pub(crate) fn pump_is_current(&self, client: ClientId, terminal: ResourceId, fence: &LastValidAttachSequence) -> bool {
        self.pumps.get(&(client, terminal)).is_some_and(|generation| {
            Arc::ptr_eq(&generation.last_valid_seq, fence) && !generation.cancel.is_cancelled()
                && generation.publication.as_ref().is_none_or(|publication| !publication.is_cancelled())
        })
    }

    /// One predecessor at a time: no per-wait scratch allocation or timer.
    pub(crate) fn retiring_pump(&mut self, client: ClientId, terminal: ResourceId, fence: &LastValidAttachSequence) -> Option<CancellationToken> {
        let tasks = self.output_pumps.get_mut(&(client, terminal))?;
        tasks.retain(|task| !task.done.is_cancelled());
        tasks.iter().find(|task| !Arc::ptr_eq(&task.generation, fence))
            .map(|task| task.done.clone())
    }

    pub(crate) fn source_has_emitter(&self, client: ClientId, terminal: ResourceId, fence: &LastValidAttachSequence) -> bool {
        self.output_pumps.get(&(client, terminal)).is_some_and(|tasks| tasks.iter().any(|task| Arc::ptr_eq(&task.generation, fence) && !task.done.is_cancelled()))
    }

    pub(crate) fn prepare_output_task(&self, client: ClientId, terminal: ResourceId, fence: &LastValidAttachSequence, drain: Option<&CancellationToken>) -> Option<CancellationToken> {
        if !self.pump_is_current(client, terminal, fence) { return None; }
        let generation = self.pumps.get(&(client, terminal))?;
        if generation.disposition == StreamDisposition::Draining && let Some(drain) = drain { drain.cancel(); }
        Some(generation.source_done.clone())
    }

    pub(crate) fn track_reserved_output(&mut self, client: ClientId, terminal: ResourceId, fence: &LastValidAttachSequence, abort: AbortHandle, done: CancellationToken, drain: Option<CancellationToken>) -> bool {
        if !self.pump_is_current(client, terminal, fence) { abort.abort(); return false; }
        let tasks = self.output_pumps.entry((client, terminal)).or_default();
        tasks.retain(|task| !task.done.is_cancelled());
        tasks.push(OutputPumpTask { abort: Some(abort), done, drain, generation: Arc::clone(fence) });
        true
    }

    pub(crate) fn publish_pump(&mut self, client: ClientId, terminal: ResourceId, fence: &LastValidAttachSequence) -> bool {
        if !self.pump_is_current(client, terminal, fence) { return false; }
        if let Some(generation) = self.pumps.get_mut(&(client, terminal)) { generation.publication = None; }
        true
    }

    /// Move a still-current pump onto a fresh bootstrap id before it publishes
    /// a replacement generation, so a later re-attach tombstones the right
    /// one. `None` when the pump was displaced or ids are exhausted.
    pub(crate) fn advance_pump_generation(
        &mut self,
        client: ClientId,
        terminal: ResourceId,
        last_valid_seq: &LastValidAttachSequence,
    ) -> Option<BootstrapId> {
        let current = self.pump_is_current(client, terminal, last_valid_seq);
        if !current {
            return None;
        }
        let bootstrap_id = self.next_bootstrap_id(client)?;
        if let Some(generation) = self.pumps.get_mut(&(client, terminal)) {
            generation.bootstrap_id = bootstrap_id;
        }
        Some(bootstrap_id)
    }

    /// Next per-terminal bootstrap id for `client`; `None` on overflow (the
    /// connection must reconnect).
    pub(crate) fn next_bootstrap_id(&mut self, client: ClientId) -> Option<BootstrapId> {
        let next = self.next_bootstrap.entry(client).or_insert(1);
        let raw = *next;
        *next = raw.checked_add(1)?;
        BootstrapId::new(raw)
    }

    /// Cancel and forget the pump for `(client, terminal)`. Idempotent.
    pub(crate) fn cancel_pump(&mut self, client: ClientId, terminal: ResourceId) {
        self.abort_output_pumps(client, terminal);
        if let Some(generation) = self.pumps.remove(&(client, terminal)) {
            generation.cancel.cancel();
            if let Some(publication) = generation.publication { publication.cancel(); }
        }
    }

    /// Cancel every client's pumps for `terminal`: a killed Terminal's
    /// process outlives its close (the hangup grace), and nothing it prints
    /// then may follow its `RESOURCE_CLOSED`.
    pub(crate) fn cancel_pumps_for_terminal(&mut self, terminal: ResourceId) {
        let clients: Vec<ClientId> = self
            .pumps
            .keys()
            .chain(self.output_pumps.keys())
            .filter(|(_, pane)| *pane == terminal)
            .map(|(client, _)| *client)
            .collect();
        for client in clients {
            self.cancel_pump(client, terminal);
        }
    }

    /// Cancel every pump `client` owns.
    pub(in crate::state) fn cancel_pumps_for_client(&mut self, client: ClientId) {
        self.output_pumps.retain(|(owner, _), _| *owner != client);
        self.pumps.retain(|(owner, _), generation| {
            if *owner == client {
                generation.cancel.cancel();
                if let Some(publication) = &generation.publication { publication.cancel(); }
                false
            } else {
                true
            }
        });
        self.next_bootstrap.remove(&client);
    }

}
