//! Settling pending commands and spawns, and retagging inbound ids.

use super::{
    ClientId, CommandResult, ErrorCode, PendingProxyRegistration, Registration, RelaySession,
    ResourceId, SnapshotGate, SpawnResult, debug, retag_spawn_result, warn,
};

impl RelaySession {
    /// Resolve a link-side id to its consumer. An error reply rolls back the
    /// command's subscription rider.
    pub(super) fn resolve_pending(&mut self, request_id: u32, mut result: CommandResult) {
        match self.pending.remove(&request_id) {
            Some(pending) => {
                if let CommandResult::OkWith(phux_protocol::wire::frame::CommandValue::State(
                    snapshot,
                )) = &result
                    && let Some(catalog) = &self.resource_kinds
                {
                    let mut catalog = catalog
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    catalog.kinds.clear();
                    catalog.controls.clear();
                    for resource in &snapshot.resources {
                        if let Some(id) = resource.id.local_id() {
                            catalog.kinds.insert(id, resource.kind);
                            catalog.controls.insert(id, resource.control);
                        }
                    }
                }
                // With no rider, the attach is the hub's own ask.
                let asked = pending.subscription.is_none();
                if let Some(registration) = pending.subscription {
                    let current = self.registration_is_current(&registration);
                    if !matches!(result, CommandResult::Error { .. })
                        && (!current
                            || pending
                                .owner
                                .as_ref()
                                .is_some_and(tokio_util::sync::CancellationToken::is_cancelled))
                    {
                        result = CommandResult::error(
                            ErrorCode::InvalidCommand,
                            "proxy registration was withdrawn before its reply",
                        );
                    }
                    if matches!(result, CommandResult::Error { .. }) {
                        self.roll_back_registration(&registration);
                    } else if registration.content
                        && !pending.reply.is_closed()
                        && let Some(journal) = &self.journal
                    {
                        journal.with_mut(|s| {
                            if s.federation()
                                .satellite_link_is_current(&self.host, self.epoch)
                            {
                                s.federation_mut().register_satellite_proxy_attach(
                                    registration.client,
                                    self.host.clone(),
                                    registration.terminal,
                                );
                            }
                        });
                    }
                }
                let refused = match &result {
                    CommandResult::Error { code, .. } => Some(*code),
                    _ => None,
                };
                if let Some(terminal) = pending.attach {
                    if asked && refused == Some(ErrorCode::TerminalNotFound) {
                        self.asked_for_a_closed_terminal(terminal);
                    }
                    self.attach_answered(terminal);
                }
                // A dropped receiver (detached relay) is fine.
                let _ = pending.reply.send(result);
            }
            None => {
                debug!(
                    satellite = %self.host,
                    request_id,
                    "satellite reply with no pending command; dropping"
                );
            }
        }
    }

    /// Whether the subscriber `registration` installed is still the one held
    /// for its `(terminal, client)`: a newer re-attach is not its to undo.
    pub(super) fn registration_is_current(&self, registration: &PendingProxyRegistration) -> bool {
        self.subscribers
            .get(&registration.terminal)
            .is_some_and(|subs| {
                subs.iter()
                    .any(|sub| sub.client == registration.client && sub.seq == registration.seq)
            })
    }

    /// Undo `registration`'s effect if it still holds; see
    /// [`Self::registration_is_current`].
    pub(super) fn roll_back_registration(&mut self, registration: &PendingProxyRegistration) {
        if self.registration_is_current(registration) {
            self.roll_back_subscription(
                registration.terminal,
                registration.client,
                registration.effect,
            );
        }
    }

    /// Undo a failed subscribing command's registration effect.
    pub(super) fn roll_back_subscription(
        &mut self,
        terminal: u32,
        client: ClientId,
        effect: Registration,
    ) {
        match effect {
            // The command created it: remove it.
            Registration::New => {
                if let Some(subs) = self.subscribers.get_mut(&terminal) {
                    subs.retain(|s| s.client != client);
                    if subs.is_empty() {
                        self.subscribers.remove(&terminal);
                    }
                    debug!(
                        satellite = %self.host,
                        terminal,
                        ?client,
                        "satellite refused the subscribing command; proxy registration rolled back"
                    );
                }
            }
            // Restore `Open` after a failed upgrade, but only if this
            // command's gate is still there: a snapshot retained meanwhile
            // must stay ahead of later deltas.
            Registration::Existing {
                regated,
                first_content,
            } => {
                if let Some(sub) = self
                    .subscribers
                    .get_mut(&terminal)
                    .and_then(|subs| subs.iter_mut().find(|s| s.client == client))
                {
                    if first_content {
                        sub.content = None;
                    }
                    if regated && matches!(sub.gate, SnapshotGate::AwaitingFirst) {
                        sub.gate = SnapshotGate::Open;
                        debug!(
                            satellite = %self.host,
                            terminal,
                            ?client,
                            "satellite refused the upgrade attach; re-gated stream restored to Open"
                        );
                    }
                }
            }
        }
        self.recalculate_retained_totals();
    }

    /// Resolve a spawn reply, re-tagging the new id to this host; a
    /// `Satellite`-tagged id in the reply becomes `SpawnFailed`.
    pub(super) fn resolve_pending_spawn(&mut self, request_id: u32, result: SpawnResult) {
        let Some((reply, kind)) = self.pending_spawns.remove(&request_id) else {
            debug!(
                satellite = %self.host,
                request_id,
                "satellite spawn reply with no pending spawn; dropping"
            );
            return;
        };
        if let SpawnResult::Ok(ResourceId::Local { id }) = &result
            && let Some(catalog) = &self.resource_kinds
        {
            catalog
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .kinds
                .insert(*id, kind);
        }
        let retagged = retag_spawn_result(&self.host, result);
        // A dropped receiver (consumer timed out / disconnected) is fine.
        let _ = reply.send(retagged);
    }

    /// The satellite-local id of an inbound frame's scope; `None` when
    /// unscoped or already satellite-tagged (never chained).
    pub(super) fn retag_inbound(&self, terminal: Option<&ResourceId>) -> Option<u32> {
        match terminal {
            Some(ResourceId::Local { id }) => Some(*id),
            Some(ResourceId::Satellite { .. }) => {
                warn!(
                    satellite = %self.host,
                    "satellite forwarded a Satellite-tagged id; hub-and-spoke does not chain — dropping"
                );
                None
            }
            None => None,
        }
    }
}
