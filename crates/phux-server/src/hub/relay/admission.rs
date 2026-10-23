//! Refusing what a satellite cannot serve, and enqueueing listings, path queries, and subscriptions.

#[cfg(test)]
use super::RelayRequest;
use super::{
    ClientId, Command, CommandResult, DirectoryListingResult, ErrorCode, FrameKind, Outbound,
    PathQueryResult, PendingListing, PendingPathQuery, ProxySubscriber, ProxySubscription,
    Registration, RelaySession, ResourceId, SatelliteSpawn, ServerFeature, SnapshotGate,
    SpawnError, listing_refusal, oneshot, path_refusal,
};

impl RelaySession {
    fn method_advertised(&self, name: &str) -> bool {
        phux_protocol::kinds::method_named(name).is_some_and(|method| {
            method.advertised(self.satellite_features, self.satellite_features_ext)
        })
    }

    /// Refuse a spawn with fields the satellite would silently skip.
    pub(super) fn spawn_capability_rejection(&self, spawn: &SatelliteSpawn) -> Option<SpawnError> {
        let resource = spawn.resource.as_ref()?;
        if resource.idempotency_key.is_some()
            && !self
                .satellite_features
                .contains(ServerFeature::SpawnIdempotency)
        {
            return Some(SpawnError::SpawnFailed(format!(
                "satellite {} lacks SPAWN_IDEMPOTENCY; nothing was spawned",
                self.host
            )));
        }
        if resource.retain_secs.is_some()
            && !self
                .satellite_features
                .contains(ServerFeature::RetainOnExit)
        {
            return Some(SpawnError::SpawnFailed(format!(
                "satellite {} lacks RETAIN_ON_EXIT; nothing was spawned",
                self.host
            )));
        }
        None
    }

    /// Refuse, without touching the link, a command this satellite cannot
    /// evaluate (conditional kill, or a keyed op without dedupe).
    pub(super) fn command_rejection(&self, command: &Command) -> Option<CommandResult> {
        if let Some(message) = self.conditional_kill_rejection(command) {
            return Some(CommandResult::Error {
                code: ErrorCode::PreconditionFailed,
                message,
            });
        }
        // Only these existing ordinary admission checks are catalog-driven.
        // Payload-sensitive and keyed-operation checks remain explicit below.
        let name = match command {
            Command::GetFile { .. } => Some("GET_FILE"),
            Command::Checkout { .. } => Some("CHECKOUT"),
            Command::ServiceHttpOpen { .. } => Some("SERVICE_HTTP_OPEN"),
            Command::ServiceHttpRead { .. } => Some("SERVICE_HTTP_READ"),
            Command::ServiceHttpClose { .. } => Some("SERVICE_HTTP_CLOSE"),
            _ => None,
        };
        if let Some(name) = name
            && !self.method_advertised(name)
        {
            return Some(CommandResult::Error {
                code: ErrorCode::UnsupportedSatelliteRoute,
                message: format!(
                    "satellite {} lacks {}; nothing was forwarded",
                    self.host,
                    phux_protocol::kinds::method_named(name)
                        .and_then(|method| method.gate)
                        .map_or(name, phux_protocol::kinds::FeatureGate::wire_name),
                ),
            });
        }
        self.keyed_capability_rejection(command)
    }

    /// A keyed operation reaches only a satellite that dedupes it
    /// (`ACKNOWLEDGED_INPUT` / `KEYED_SIGNAL`), else
    /// `UNSUPPORTED_SATELLITE_ROUTE` (L1 §9.1).
    pub(super) fn keyed_capability_rejection(&self, command: &Command) -> Option<CommandResult> {
        let (needed, name) = match command {
            Command::ApplyInput { .. } => (ServerFeature::AcknowledgedInput, "ACKNOWLEDGED_INPUT"),
            _ if command.idempotency_key().is_some() => {
                (ServerFeature::KeyedSignal, "KEYED_SIGNAL")
            }
            _ => return None,
        };
        if self.satellite_features.contains(needed) {
            return None;
        }
        Some(CommandResult::Error {
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: format!(
                "satellite {} lacks {name}, so it cannot deduplicate this operation; \
                 nothing was forwarded",
                self.host
            ),
        })
    }

    /// The incarnation fence (L1 §9.1): an operation id already sent to an
    /// earlier incarnation is refused `INCARNATION_CHANGED`; a new id is
    /// recorded with `actor`.
    pub(super) fn fence_rejection(
        &self,
        command: &Command,
        actor: ClientId,
    ) -> Option<CommandResult> {
        use crate::hub::operation_fence::{FenceVerdict, fenced_key, fenced_terminals};
        let key = fenced_key(command)?;
        match self.operations.admit(
            key,
            self.incarnation,
            actor,
            fenced_terminals(command),
            std::time::Instant::now(),
        ) {
            FenceVerdict::Forward => None,
            FenceVerdict::IncarnationChanged => Some(CommandResult::Error {
                code: ErrorCode::IncarnationChanged,
                message: format!(
                    "satellite {} restarted since this operation id was first forwarded; \
                     nothing was forwarded",
                    self.host
                ),
            }),
            FenceVerdict::Full => Some(CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                message: format!(
                    "the hub's operation record for satellite {} is full; retry later",
                    self.host
                ),
            }),
        }
    }

    /// Refuse `KILL_RESOURCE_IF` to a satellite without `CONDITIONAL_KILL`
    /// (ADR-0109) instead of letting the consumer time out.
    pub(super) fn conditional_kill_rejection(&self, command: &Command) -> Option<String> {
        let Command::KillResourceIf { .. } = command else {
            return None;
        };
        if self
            .satellite_features
            .contains(ServerFeature::ConditionalKill)
        {
            return None;
        }
        Some(format!(
            "satellite {} cannot evaluate a conditional kill; nothing was killed",
            self.host
        ))
    }

    /// Put one `LIST_DIRECTORY` on the wire, or refuse it when the satellite
    /// never advertised the query.
    pub(super) fn enqueue_listing(
        &mut self,
        path: String,
        reply: oneshot::Sender<DirectoryListingResult>,
    ) -> Option<Vec<u8>> {
        if !self.method_advertised("LIST_DIRECTORY") {
            let _ = reply.send(Err(listing_refusal(
                &path,
                format!("satellite {} does not answer LIST_DIRECTORY", self.host),
            )));
            return None;
        }
        let request_id = self.allocate_request_id();
        let frame = FrameKind::ListDirectory {
            request_id,
            path: path.clone(),
            host: None,
        };
        self.pending_listings
            .insert(request_id, PendingListing { path, reply });
        Some(self.encode(&frame))
    }

    pub(super) fn enqueue_path_query(
        &mut self,
        root: String,
        query: String,
        recursive: bool,
        reply: oneshot::Sender<PathQueryResult>,
    ) -> Option<Vec<u8>> {
        if !self.method_advertised("PATH_QUERY") {
            let _ = reply.send(Err(path_refusal(
                &root,
                format!("satellite {} does not answer PATH_QUERY", self.host),
            )));
            return None;
        }
        let request_id = self.allocate_request_id();
        let frame = FrameKind::PathQuery {
            request_id,
            root: root.clone(),
            query,
            recursive,
            host: None,
        };
        self.pending_paths
            .insert(request_id, PendingPathQuery { root, reply });
        Some(self.encode(&frame))
    }

    /// Register and remember event scope atomically with its forward frame.
    pub(super) fn subscribe_forward(
        &mut self,
        subscription: ProxySubscription,
        forward: &FrameKind,
    ) -> Option<Vec<u8>> {
        if let Some((code, message)) = self.subscription_rejection(&subscription) {
            let _ = subscription
                .out_tx
                .try_send(Outbound::Frame(FrameKind::Error {
                    request_id: None,
                    code,
                    message,
                }));
            return None;
        }
        if let FrameKind::SubscribeEvents {
            terminal: Some(ResourceId::Local { id }),
            ..
        } = forward
        {
            self.legacy_events.insert(*id);
        }
        self.register_subscriber(subscription);
        let forward = self.with_link_cursor(forward);
        Some(self.encode(&forward))
    }

    #[cfg(test)]
    pub(super) fn handle_request(&mut self, request: RelayRequest) -> Vec<u8> {
        self.handle_request_checked(request)
            .expect("test request must be forwarded")
    }

    pub(super) fn subscription_rejection(
        &self,
        subscription: &ProxySubscription,
    ) -> Option<(ErrorCode, String)> {
        if !subscription.awaits_snapshot {
            return None;
        }
        let profile_matches = subscription.resource_kind
            == Some(phux_protocol::ids::ResourceKind::AgentSession)
            || subscription.bootstrap_profile == Some(self.bootstrap_profile);
        if profile_matches && subscription.bootstrap_limits == Some(self.bootstrap_limits) {
            return None;
        }
        Some((
            ErrorCode::CodecUnavailable,
            format!(
                "satellite {} link uses profile {:?} and limits {:?}; downstream content connection selected profile {:?} and limits {:?}; relay transcoding is unavailable",
                self.host,
                self.bootstrap_profile,
                self.bootstrap_limits,
                subscription.bootstrap_profile,
                subscription.bootstrap_limits,
            ),
        ))
    }

    /// Register one proxy subscriber idempotently, returning the
    /// [`Registration`] effect for rollback.
    pub(super) fn register_subscriber(&mut self, subscription: ProxySubscription) -> Registration {
        let ProxySubscription {
            terminal,
            client,
            out_tx,
            consumer_cancel,
            seq,
            awaits_snapshot,
            bootstrap_profile: _,
            bootstrap_limits: _,
            resource_kind: _,
        } = subscription;
        self.orphaned_by_delivery.remove(&terminal);
        let subs = self.subscribers.entry(terminal).or_default();
        if let Some(existing) = subs.iter_mut().find(|s| s.client == client) {
            existing.out_tx = out_tx;
            existing.consumer_cancel = consumer_cancel;
            // Keep the freshest token.
            existing.seq = existing.seq.max(seq);
            // An upgrade from an `Open` stream to a snapshot attach re-gates
            // until this attach's snapshot lands (L1 §9.1). Other gates
            // already suppress deltas; event-only re-subscribes leave it.
            if awaits_snapshot && matches!(existing.gate, SnapshotGate::Open) {
                existing.gate = SnapshotGate::AwaitingFirst;
                Registration::Regated
            } else {
                Registration::Idempotent
            }
        } else {
            subs.push(ProxySubscriber {
                client,
                out_tx,
                consumer_cancel,
                seq,
                // Attaches gate until their snapshot; event-only opens now.
                gate: if awaits_snapshot {
                    SnapshotGate::AwaitingFirst
                } else {
                    SnapshotGate::Open
                },
                retire_after_flush: false,
                current_generation: None,
            });
            Registration::New
        }
    }
}
