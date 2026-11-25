//! Withdrawing proxy subscribers and detaching the satellite side when the last one leaves.

use super::{
    ClientId, Command, CommandResult, ErrorCode, FrameKind, PendingDetach, RELAY_COMMAND_TIMEOUT,
    RelaySession, ResourceId, Unsubscribe, debug,
};

impl RelaySession {
    pub(crate) fn handle_queued_unsubscribe(
        &mut self,
        queued: super::QueuedUnsubscribe,
    ) -> Vec<Vec<u8>> {
        if !self.link_is_current() {
            return Vec::new();
        }
        if queued.epoch != self.epoch && !self.resumed_from(queued.epoch, &queued.unsubscribe) {
            return Vec::new();
        }
        self.handle_unsubscribe(queued.unsubscribe)
    }

    /// Whether a withdrawal pinned to an earlier link names a subscription
    /// this one resumed from that link: one registered before the withdrawal
    /// was issued. A newer re-attach on this link is not the old link's to
    /// withdraw, so that withdrawal finds no live registration of its own.
    fn resumed_from(&self, epoch: u64, unsubscribe: &Unsubscribe) -> bool {
        if epoch >= self.epoch {
            return false;
        }
        match *unsubscribe {
            Unsubscribe::Client(client) => self
                .subscribers
                .values()
                .flatten()
                .any(|sub| sub.client == client),
            Unsubscribe::Terminal {
                client,
                terminal,
                seq,
                ..
            } => self
                .subscribers
                .get(&terminal)
                .is_some_and(|subs| subs.iter().any(|sub| sub.client == client && sub.seq < seq)),
        }
    }

    /// Withdraw proxy subscriptions. Returns the upstream
    /// `DETACH_RESOURCE` frames for terminals that lost their last proxy (or
    /// had none after an automatic spawn). The upstream reply retires the old
    /// bootstrap flow; the downstream receipt certifies only local removal.
    pub(crate) fn handle_unsubscribe(&mut self, unsubscribe: Unsubscribe) -> Vec<Vec<u8>> {
        match unsubscribe {
            Unsubscribe::Client(client) => {
                let orphaned = self.withdraw_client(client);
                self.encode_withdrawals(orphaned)
            }
            Unsubscribe::Terminal {
                client,
                terminal,
                seq,
                reply,
            } => match reply {
                Some(reply) => {
                    reply.apply(|| self.apply_terminal_withdrawal(client, terminal, seq))
                }
                None => self.apply_terminal_withdrawal(client, terminal, seq).1,
            },
        }
    }

    pub(super) fn apply_terminal_withdrawal(
        &mut self,
        client: ClientId,
        terminal: u32,
        seq: u64,
    ) -> (CommandResult, Vec<Vec<u8>>) {
        match self.withdraw_terminal(client, terminal, seq) {
            Ok(orphaned) => {
                if let Some(journal) = &self.journal {
                    journal.with_mut(|s| {
                        if s.federation()
                            .satellite_link_is_current(&self.host, self.epoch)
                        {
                            s.federation_mut()
                                .unregister_satellite_proxy_attach(client, &self.host, terminal);
                            s.unsubscribe_resource_events(
                                client,
                                &ResourceId::satellite(self.host.clone(), terminal),
                            );
                        }
                    });
                }
                (
                    CommandResult::Ok,
                    self.encode_withdrawals(orphaned.into_iter().collect()),
                )
            }
            Err(result) => (result, Vec::new()),
        }
    }

    pub(super) fn encode_withdrawals(&mut self, orphaned: Vec<u32>) -> Vec<Vec<u8>> {
        self.recalculate_retained_totals();
        orphaned
            .into_iter()
            .map(|terminal| self.encode_terminal_detach(terminal))
            .collect()
    }

    pub(super) fn withdraw_client(&mut self, client: ClientId) -> Vec<u32> {
        let mut orphaned = Vec::new();
        self.subscribers.retain(|terminal, subs| {
            subs.retain(|s| s.client != client);
            if subs.is_empty() {
                orphaned.push(*terminal);
                false
            } else {
                true
            }
        });
        orphaned
    }

    pub(super) fn withdraw_terminal(
        &mut self,
        client: ClientId,
        terminal: u32,
        seq: u64,
    ) -> Result<Option<u32>, CommandResult> {
        // A relayed SPAWN streams before any proxy attaches; detach stops it.
        let Some(subs) = self.subscribers.get_mut(&terminal) else {
            return Ok(Some(terminal));
        };
        // Never let a stale detach remove a newer registration.
        if subs.iter().any(|s| s.client == client && s.seq >= seq) {
            debug!(satellite = %self.host, terminal, ?client,
                "stale terminal unsubscribe superseded by a newer re-attach; dropping");
            return Err(CommandResult::error(
                ErrorCode::InvalidCommand,
                "terminal detach was superseded by a newer attachment",
            ));
        }
        subs.retain(|s| s.client != client);
        if !subs.is_empty() {
            return Ok(None);
        }
        self.subscribers.remove(&terminal);
        Ok(Some(terminal))
    }

    pub(super) fn encode_terminal_detach(&mut self, terminal: u32) -> Vec<u8> {
        self.explicit_content.remove(&terminal);
        self.legacy_events.remove(&terminal);
        debug!(satellite = %self.host, terminal,
            "last proxy subscriber gone; detaching satellite-side");
        let request_id = self.allocate_request_id();
        self.pending_detaches.insert(
            request_id,
            PendingDetach {
                terminal,
                deadline: tokio::time::Instant::now() + RELAY_COMMAND_TIMEOUT,
            },
        );
        self.encode(&FrameKind::Command {
            request_id,
            command: Command::DetachResource {
                terminal_id: ResourceId::local(terminal),
            },
        })
    }
}
