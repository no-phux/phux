//! Tearing a session down, pruning abandoned work, and enforcing detach deadlines.

use super::{
    ClientId, CommandResult, ErrorCode, Outbound, RELAY_COMMAND_TIMEOUT, RelayResume, RelaySession,
    SpawnError, SpawnResult, debug, listing_refusal, path_refusal, unreachable_error,
};

impl RelaySession {
    pub(super) fn link_is_current(&self) -> bool {
        if let Some(journal) = &self.journal {
            return journal.with(|s| {
                s.federation()
                    .satellite_link_is_current(&self.host, self.epoch)
            });
        }
        self.resource_kinds.as_ref().is_none_or(|catalog| {
            catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .epoch
                == self.epoch
        })
    }

    /// Fail in-flight commands, notify subscribers, clear registries. Once
    /// per session. Returns what the next connection resumes: the content
    /// attaches only, which it registers again under its own epoch.
    pub(crate) fn teardown(&mut self, why: &str) -> RelayResume {
        if let Some(journal) = &self.journal {
            journal.with_mut(|s| {
                if s.federation()
                    .satellite_link_is_current(&self.host, self.epoch)
                {
                    s.interrupt_satellite_host(&self.host);
                    s.federation_mut()
                        .end_satellite_link(&self.host, self.epoch);
                }
            });
        }
        if let Some(catalog) = &self.resource_kinds {
            let mut catalog = catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if catalog.epoch == self.epoch {
                catalog.end_link();
            }
        }
        let mut refused = Vec::new();
        for (_, pending) in self.pending.drain() {
            refused.extend(pending.subscription);
            let _ = pending.reply.send(CommandResult::error(
                ErrorCode::SatelliteUnreachable,
                format!("satellite {} is unreachable: {why}", self.host),
            ));
        }
        for (_, (reply, _)) in self.pending_spawns.drain() {
            let _ = reply.send(SpawnResult::Err(SpawnError::SatelliteUnreachable(format!(
                "satellite {} is unreachable: {why}",
                self.host
            ))));
        }
        for (_, pending) in self.pending_listings.drain() {
            let _ = pending.reply.send(Err(listing_refusal(
                &pending.path,
                format!("satellite {} is unreachable: {why}", self.host),
            )));
        }
        for (_, pending) in self.pending_paths.drain() {
            let _ = pending.reply.send(Err(path_refusal(
                &pending.root,
                format!("satellite {} is unreachable: {why}", self.host),
            )));
        }
        // One typed ERROR per consumer, naming the host.
        let mut notified: Vec<ClientId> = Vec::new();
        let error = unreachable_error(&self.host, why);
        for subs in self.subscribers.values() {
            for sub in subs {
                if notified.contains(&sub.client) {
                    continue;
                }
                notified.push(sub.client);
                let _ = sub.out_tx.try_send(Outbound::Frame(error.clone()));
            }
        }
        if !notified.is_empty() {
            debug!(
                satellite = %self.host,
                consumers = notified.len(),
                why,
                "notified proxy subscribers of satellite teardown"
            );
        }
        // A consumer told its attach failed is not attached: resuming it would
        // stream to a pane it never opened.
        for registration in refused {
            self.roll_back_registration(&registration);
        }
        let resume = self.take_resume();
        self.bootstrap_flows.clear();
        self.explicit_content.clear();
        self.legacy_events.clear();
        self.pending_detaches.clear();
        self.mirrored.clear();
        self.pending_mirror_gets.clear();
        self.retained_bytes = 0;
        self.retained_frames = 0;
        self.inflight_generation_bytes = 0;
        self.inflight_generation_frames = 0;
        resume
    }

    /// Drop pending entries whose consumer stopped waiting; returns how many.
    /// Run on the keepalive tick so a satellite that never answers cannot
    /// grow the map without bound.
    pub(crate) fn prune_abandoned(&mut self) -> usize {
        let before = self.pending_request_count();
        let mut abandoned = Vec::new();
        self.pending.retain(|_, pending| {
            let waiting = !pending.reply.is_closed();
            if !waiting {
                abandoned.extend(pending.subscription.take());
            }
            waiting
        });
        // Its consumer was told the attach failed: a late answer finds nobody
        // and is detached.
        for registration in abandoned {
            let (terminal, client) = (registration.terminal, registration.client);
            // A retry still in flight decides instead.
            let retried = self.pending.values().any(|pending| {
                pending
                    .subscription
                    .as_ref()
                    .is_some_and(|rider| (rider.terminal, rider.client) == (terminal, client))
            });
            if retried {
                continue;
            }
            self.roll_back_registration(&registration);
            if !self.subscribers.contains_key(&terminal) {
                self.orphaned_by_delivery.insert(terminal);
            }
        }
        self.pending_spawns
            .retain(|_, (reply, _)| !reply.is_closed());
        self.pending_listings
            .retain(|_, pending| !pending.reply.is_closed());
        self.pending_paths
            .retain(|_, pending| !pending.reply.is_closed());
        let remaining = self.pending_request_count();
        let pruned = before - remaining;
        if pruned > 0 {
            debug!(
                satellite = %self.host,
                pruned,
                remaining,
                "pruned relayed commands whose consumer stopped waiting"
            );
        }
        pruned
    }

    /// Correlated requests still waiting on the satellite, of every kind.
    pub(super) fn pending_request_count(&self) -> usize {
        self.pending.len()
            + self.pending_spawns.len()
            + self.pending_listings.len()
            + self.pending_paths.len()
    }

    /// A peer that never acknowledges a detach leaves generation ownership
    /// uncertain, so an expired barrier resets the link.
    pub(crate) fn check_detach_deadlines(&self) -> Result<(), String> {
        let now = tokio::time::Instant::now();
        if self
            .pending_detaches
            .values()
            .any(|pending| pending.deadline <= now)
        {
            return Err(format!(
                "satellite {} did not acknowledge upstream detach within {}s",
                self.host,
                RELAY_COMMAND_TIMEOUT.as_secs()
            ));
        }
        Ok(())
    }
}

impl Drop for RelaySession {
    fn drop(&mut self) {
        self.teardown("link session dropped");
    }
}
