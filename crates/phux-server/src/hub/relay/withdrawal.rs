//! Withdrawing proxy subscribers and detaching the satellite side when the last one leaves.

use super::{
    ClientId, Command, CommandResult, ErrorCode, FrameKind, PendingDetach, RELAY_COMMAND_TIMEOUT,
    RelaySession, ResourceId, Unsubscribe, debug,
};

impl RelaySession {
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
            Ok(orphaned) => (
                CommandResult::Ok,
                self.encode_withdrawals(orphaned.into_iter().collect()),
            ),
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
            return Err(CommandResult::Error {
                code: ErrorCode::InvalidCommand,
                message: "terminal detach was superseded by a newer attachment".to_owned(),
            });
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
